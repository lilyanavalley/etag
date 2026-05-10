//! BLE Mesh foundation types and vendor-model payload codecs.
//!
//! This module is transport-focused and `no_std` compatible so it can be shared
//! by firmware logic regardless of the final radio stack integration details.

use heapless::{String, Vec};

use crate::config::{MAX_GROCYCODE_LEN, MAX_PRODUCT_NAME_LEN};

/// Role assigned at provisioning time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshNodeRole {
    /// Battery-powered shelf tag profile (expected to run in LPN mode).
    TagLpn = 0,
    /// Mains-powered relay/repeater profile.
    RelayRepeater = 1,
}

impl core::convert::TryFrom<u8> for MeshNodeRole {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::TagLpn),
            1 => Ok(Self::RelayRepeater),
            _ => Err(()),
        }
    }
}

/// Inventory message operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryOp {
    /// Delta update from local button interaction.
    StockDelta = 0,
    /// Absolute stock value update.
    StockAbsolute = 1,
}

impl core::convert::TryFrom<u8> for InventoryOp {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::StockDelta),
            1 => Ok(Self::StockAbsolute),
            _ => Err(()),
        }
    }
}

/// Vendor-model inventory payload (wire format v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryMessage {
    /// Monotonic per-node revision/sequence.
    pub revision: u32,
    /// Grocy product ID.
    pub product_id: u32,
    /// Operation semantics (`delta` or `absolute`).
    pub op: InventoryOp,
    /// Stock value (delta or absolute depending on `op`).
    pub stock_value: i32,
    /// Battery percentage 0..=100.
    pub battery_pct: u8,
}

/// Provisioning/config payload (wire format v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMessage {
    /// Monotonic per-node revision/sequence.
    pub revision: u32,
    /// Runtime role profile.
    pub role: MeshNodeRole,
    /// Desired publish interval for status updates.
    pub publish_interval_secs: u16,
    /// Grocycode (e.g. `grcy-p-42`).
    pub grocycode: String<MAX_GROCYCODE_LEN>,
    /// Product display label.
    pub product_name: String<MAX_PRODUCT_NAME_LEN>,
}

/// Fixed length of encoded [`InventoryMessage`].
pub const INVENTORY_WIRE_LEN: usize = 14;

/// Maximum encoded length of [`ConfigMessage`].
///
/// Header bytes:
/// - revision (4)
/// - role (1)
/// - publish_interval_secs (2)
/// - grocycode_len (1)
/// - product_name_len (1)
/// Plus UTF-8 payload bytes for strings.
pub const CONFIG_WIRE_MAX_LEN: usize = 9 + MAX_GROCYCODE_LEN + MAX_PRODUCT_NAME_LEN;

/// Encode [`InventoryMessage`] into a compact fixed-length wire payload.
pub fn encode_inventory(msg: &InventoryMessage) -> [u8; INVENTORY_WIRE_LEN] {
    let mut out = [0_u8; INVENTORY_WIRE_LEN];

    out[0..4].copy_from_slice(&msg.revision.to_le_bytes());
    out[4..8].copy_from_slice(&msg.product_id.to_le_bytes());
    out[8] = msg.op as u8;
    out[9..13].copy_from_slice(&msg.stock_value.to_le_bytes());
    out[13] = msg.battery_pct;

    out
}

/// Decode an [`InventoryMessage`] from wire bytes.
pub fn decode_inventory(data: &[u8]) -> Option<InventoryMessage> {
    if data.len() != INVENTORY_WIRE_LEN {
        return None;
    }

    let revision = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let product_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let op = InventoryOp::try_from(data[8]).ok()?;
    let stock_value = i32::from_le_bytes([data[9], data[10], data[11], data[12]]);
    let battery_pct = data[13];

    Some(InventoryMessage {
        revision,
        product_id,
        op,
        stock_value,
        battery_pct,
    })
}

/// Encode [`ConfigMessage`] into a variable-length wire payload.
pub fn encode_config(msg: &ConfigMessage) -> Option<Vec<u8, CONFIG_WIRE_MAX_LEN>> {
    let grocy_bytes = msg.grocycode.as_bytes();
    let product_bytes = msg.product_name.as_bytes();

    let mut out: Vec<u8, CONFIG_WIRE_MAX_LEN> = Vec::new();

    out.extend_from_slice(&msg.revision.to_le_bytes()).ok()?;
    out.push(msg.role as u8).ok()?;
    out.extend_from_slice(&msg.publish_interval_secs.to_le_bytes())
        .ok()?;
    out.push(grocy_bytes.len() as u8).ok()?;
    out.push(product_bytes.len() as u8).ok()?;
    out.extend_from_slice(grocy_bytes).ok()?;
    out.extend_from_slice(product_bytes).ok()?;

    Some(out)
}

/// Decode a [`ConfigMessage`] from wire bytes.
pub fn decode_config(data: &[u8]) -> Option<ConfigMessage> {
    if data.len() < 9 {
        return None;
    }

    let revision = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let role = MeshNodeRole::try_from(data[4]).ok()?;
    let publish_interval_secs = u16::from_le_bytes([data[5], data[6]]);

    let grocy_len = data[7] as usize;
    let product_len = data[8] as usize;

    if grocy_len > MAX_GROCYCODE_LEN || product_len > MAX_PRODUCT_NAME_LEN {
        return None;
    }

    let expected = 9usize.checked_add(grocy_len)?.checked_add(product_len)?;
    if data.len() != expected {
        return None;
    }

    let grocy_start = 9;
    let product_start = grocy_start + grocy_len;

    let grocycode_str = core::str::from_utf8(&data[grocy_start..product_start]).ok()?;
    let product_name_str = core::str::from_utf8(&data[product_start..]).ok()?;

    let mut grocycode = String::<MAX_GROCYCODE_LEN>::new();
    grocycode.push_str(grocycode_str).ok()?;

    let mut product_name = String::<MAX_PRODUCT_NAME_LEN>::new();
    product_name.push_str(product_name_str).ok()?;

    Some(ConfigMessage {
        revision,
        role,
        publish_interval_secs,
        grocycode,
        product_name,
    })
}

/// Runtime helper for publishing inventory updates from a tag node.
///
/// Revisions are monotonic per boot unless caller restores persisted state.
/// For production mesh replay protection, persist and restore revision counters.
#[derive(Debug, Clone)]
pub struct MeshTagPublisher {
    product_id: u32,
    next_revision: u32,
}

impl MeshTagPublisher {
    /// Create a publisher state for one product ID.
    pub const fn new(product_id: u32) -> Self {
        Self {
            product_id,
            next_revision: 1,
        }
    }

    /// Build and encode a stock-delta publish payload.
    pub fn build_delta_frame(&mut self, delta: i32, battery_pct: u8) -> [u8; INVENTORY_WIRE_LEN] {
        let msg = InventoryMessage {
            revision: self.next_revision,
            product_id: self.product_id,
            op: InventoryOp::StockDelta,
            stock_value: delta,
            battery_pct,
        };
        self.next_revision = self.next_revision.saturating_add(1);
        encode_inventory(&msg)
    }

    /// Build and encode an absolute-stock publish payload.
    pub fn build_absolute_frame(
        &mut self,
        stock_count: i32,
        battery_pct: u8,
    ) -> [u8; INVENTORY_WIRE_LEN] {
        let msg = InventoryMessage {
            revision: self.next_revision,
            product_id: self.product_id,
            op: InventoryOp::StockAbsolute,
            stock_value: stock_count,
            battery_pct,
        };
        self.next_revision = self.next_revision.saturating_add(1);
        encode_inventory(&msg)
    }

    /// Parse a product ID from `grcy-p-{id}`.
    pub fn parse_product_id(grocycode: &str) -> Option<u32> {
        grocycode.strip_prefix("grcy-p-")?.parse::<u32>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_roundtrip() {
        let msg = InventoryMessage {
            revision: 12,
            product_id: 42,
            op: InventoryOp::StockDelta,
            stock_value: -1,
            battery_pct: 87,
        };

        let encoded = encode_inventory(&msg);
        let decoded = decode_inventory(&encoded).unwrap();

        assert_eq!(decoded, msg);
    }

    #[test]
    fn inventory_decode_rejects_bad_len() {
        assert!(decode_inventory(&[]).is_none());
        assert!(decode_inventory(&[0_u8; INVENTORY_WIRE_LEN - 1]).is_none());
    }

    #[test]
    fn config_roundtrip() {
        let mut grocycode = String::<MAX_GROCYCODE_LEN>::new();
        grocycode.push_str("grcy-p-7").unwrap();

        let mut product_name = String::<MAX_PRODUCT_NAME_LEN>::new();
        product_name.push_str("Oat Milk").unwrap();

        let msg = ConfigMessage {
            revision: 99,
            role: MeshNodeRole::TagLpn,
            publish_interval_secs: 15,
            grocycode,
            product_name,
        };

        let encoded = encode_config(&msg).unwrap();
        let decoded = decode_config(&encoded).unwrap();

        assert_eq!(decoded, msg);
    }

    #[test]
    fn config_decode_rejects_length_mismatch() {
        let data = [0_u8, 0, 0, 0, 0, 0, 0, 1, 0];
        assert!(decode_config(&data).is_none());
    }

    #[test]
    fn publisher_emits_incrementing_revisions() {
        let mut publisher = MeshTagPublisher::new(42);
        let f1 = publisher.build_delta_frame(1, 80);
        let f2 = publisher.build_absolute_frame(7, 79);

        assert_eq!(decode_inventory(&f1).unwrap().revision, 1);
        assert_eq!(decode_inventory(&f2).unwrap().revision, 2);
    }

    #[test]
    fn parse_product_id_from_grocycode() {
        assert_eq!(MeshTagPublisher::parse_product_id("grcy-p-7"), Some(7));
        assert_eq!(MeshTagPublisher::parse_product_id("grcy-lo-7"), None);
        assert_eq!(MeshTagPublisher::parse_product_id(""), None);
    }
}
