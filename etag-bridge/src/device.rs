//! Device registry — tracks every discovered etag device and its live stats.
//!
//! The [`DeviceRegistry`] is a thread-safe, `Arc`-wrapped map of BLE address →
//! [`TagInfo`].  Multiple Tokio tasks (BLE scanner, per-device GATT task, stats
//! reporter) all share a single `Arc<DeviceRegistry>`.
//!
//! # What is tracked
//!
//! | Field          | Source                          | Notes                        |
//! |----------------|---------------------------------|------------------------------|
//! | `address`      | BLE advertisement               | "AA:BB:CC:DD:EE:FF" format   |
//! | `name`         | BLE advertisement               | Device name if broadcast     |
//! | `grocycode`    | GATT `grocycode` characteristic | e.g. `"grcy-p-42"`           |
//! | `product_id`   | Parsed from grocycode           | `u32` Grocy product ID       |
//! | `product_name` | GATT `product_name` char.       | Human-readable label         |
//! | `stock_count`  | GATT `stock_count` char./notify | Current on-shelf quantity    |
//! | `battery_pct`  | GATT `battery_pct` char./notify | 0–100 %                      |
//! | `rssi`         | BLE advertisement / scan update | Signal strength in dBm       |
//! | `first_seen`   | First advertisement received    | UTC timestamp                |
//! | `last_seen`    | Most recent BLE event           | UTC timestamp                |
//! | `connected`    | GATT connection state           | `true` while GATT is open    |

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{debug, info};

// ─────────────────────────────────────────────────────────────────────────────
// TagInfo — per-device snapshot
// ─────────────────────────────────────────────────────────────────────────────

/// All tracked information for a single etag device.
///
/// All fields are `Option` where the value may not yet have been read from the
/// device; callers should treat `None` as "not yet known".
#[derive(Debug, Clone)]
pub struct TagInfo {
    /// BLE device address (e.g. `"AA:BB:CC:DD:EE:FF"`).
    pub address: String,

    /// Device name broadcast in BLE advertisement packets, if available.
    pub name: Option<String>,

    /// Grocycode read from the GATT characteristic (e.g. `"grcy-p-42"`).
    pub grocycode: Option<String>,

    /// Grocy product ID parsed from the grocycode.
    ///
    /// `Some(42)` for grocycode `"grcy-p-42"`, `None` if unparseable.
    pub product_id: Option<u32>,

    /// Human-readable product name read from the `product_name` characteristic.
    pub product_name: Option<String>,

    /// Current in-stock quantity (last value seen via GATT read or notification).
    pub stock_count: Option<i32>,

    /// Battery percentage (0–100) read from the `battery_pct` characteristic.
    pub battery_pct: Option<u8>,

    /// Last received RSSI in dBm.  More negative = weaker signal.
    ///
    /// Typical values: `-40` (very close) … `-90` (borderline range).
    pub rssi: Option<i16>,

    /// UTC timestamp when this device was first seen.
    pub first_seen: DateTime<Utc>,

    /// UTC timestamp of the most recent BLE advertisement or characteristic update.
    pub last_seen: DateTime<Utc>,

    /// Whether the bridge currently has an active GATT connection to this device.
    pub connected: bool,
}

impl TagInfo {
    fn new(address: String) -> Self {
        let now = Utc::now();
        Self {
            address,
            name: None,
            grocycode: None,
            product_id: None,
            product_name: None,
            stock_count: None,
            battery_pct: None,
            rssi: None,
            first_seen: now,
            last_seen: now,
            connected: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DeviceRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Thread-safe registry of all discovered etag devices.
///
/// The registry is keyed by BLE address string so it is independent of
/// btleplug's platform-specific `PeripheralId` type.
///
/// Wrap in an [`Arc`] before sharing across Tokio tasks:
/// ```rust,ignore
/// let registry = Arc::new(DeviceRegistry::new());
/// ```
#[derive(Debug, Clone, Default)]
pub struct DeviceRegistry {
    inner: Arc<RwLock<HashMap<String, TagInfo>>>,
}

impl DeviceRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update the entry for `address`, bumping `last_seen` and
    /// recording the latest advertisement name and RSSI.
    ///
    /// If this is the first time the address is seen, an info log is emitted.
    pub async fn upsert_seen(&self, address: &str, name: Option<String>, rssi: Option<i16>) {
        let mut map = self.inner.write().await;
        let entry = map.entry(address.to_owned()).or_insert_with(|| {
            info!(address, "New etag device discovered");
            TagInfo::new(address.to_owned())
        });
        entry.last_seen = Utc::now();
        if name.is_some() {
            entry.name = name;
        }
        if let Some(r) = rssi {
            entry.rssi = Some(r);
            debug!(address, rssi = r, "RSSI updated");
        }
    }

    /// Store the grocycode and update the parsed product ID.
    pub async fn update_grocycode(&self, address: &str, code: &str) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(address) {
            entry.grocycode = Some(code.to_owned());
            entry.product_id = parse_product_id(code);
            entry.last_seen = Utc::now();
        }
    }

    /// Store the product name for a device.
    pub async fn update_product_name(&self, address: &str, name: &str) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(address) {
            entry.product_name = Some(name.to_owned());
            entry.last_seen = Utc::now();
        }
    }

    /// Update the stock count and return the **previous** value.
    ///
    /// The returned previous value is used by the BLE task to compute the delta
    /// for Grocy API calls.  Returns `None` if the device was not in the registry.
    pub async fn update_stock(&self, address: &str, count: i32) -> Option<i32> {
        let mut map = self.inner.write().await;
        map.get_mut(address).map(|entry| {
            let prev = entry.stock_count;
            entry.stock_count = Some(count);
            entry.last_seen = Utc::now();
            prev
        })?
    }

    /// Update the battery percentage for a device.
    pub async fn update_battery(&self, address: &str, pct: u8) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(address) {
            entry.battery_pct = Some(pct);
            entry.last_seen = Utc::now();
        }
    }

    /// Update the RSSI reading for a device (called by the GATT task after
    /// reading peripheral properties while connected).
    pub async fn update_rssi(&self, address: &str, rssi: i16) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(address) {
            entry.rssi = Some(rssi);
            entry.last_seen = Utc::now();
        }
    }

    /// Mark a device as connected or disconnected.
    pub async fn set_connected(&self, address: &str, connected: bool) {
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(address) {
            entry.connected = connected;
        }
    }

    /// Retrieve an immutable snapshot of a single device, or `None` if unknown.
    pub async fn get(&self, address: &str) -> Option<TagInfo> {
        self.inner.read().await.get(address).cloned()
    }

    /// Retrieve snapshots of all tracked devices, sorted by address.
    pub async fn get_all(&self) -> Vec<TagInfo> {
        let map = self.inner.read().await;
        let mut devices: Vec<_> = map.values().cloned().collect();
        devices.sort_by(|a, b| a.address.cmp(&b.address));
        devices
    }

    /// Return the total number of known devices.
    pub async fn count(&self) -> usize {
        self.inner.read().await.len()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Grocycode parsing helper
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a Grocy product ID from a Grocycode string.
///
/// # Examples
/// ```
/// # use etag_bridge::device::parse_product_id;
/// assert_eq!(parse_product_id("grcy-p-42"),    Some(42));
/// assert_eq!(parse_product_id("grcy-p-99999"), Some(99999));
/// assert_eq!(parse_product_id("grcy-lo-3"),    None);  // location, not product
/// assert_eq!(parse_product_id(""),             None);
/// ```
pub fn parse_product_id(grocycode: &str) -> Option<u32> {
    grocycode.strip_prefix("grcy-p-")?.parse().ok()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_product_id_valid() {
        assert_eq!(parse_product_id("grcy-p-1"), Some(1));
        assert_eq!(parse_product_id("grcy-p-99999"), Some(99999));
    }

    #[test]
    fn parse_product_id_invalid() {
        assert_eq!(parse_product_id("grcy-lo-3"), None);
        assert_eq!(parse_product_id(""), None);
        assert_eq!(parse_product_id("grcy-p-"), None);
        assert_eq!(parse_product_id("grcy-p-abc"), None);
    }

    #[tokio::test]
    async fn registry_upsert_and_update() {
        let reg = DeviceRegistry::new();

        reg.upsert_seen("AA:BB:CC:DD:EE:FF", Some("etag-1".to_owned()), Some(-65))
            .await;

        let info = reg.get("AA:BB:CC:DD:EE:FF").await.expect("device should exist");
        assert_eq!(info.name.as_deref(), Some("etag-1"));
        assert_eq!(info.rssi, Some(-65));
        assert!(!info.connected);

        reg.update_grocycode("AA:BB:CC:DD:EE:FF", "grcy-p-7").await;
        let info = reg.get("AA:BB:CC:DD:EE:FF").await.unwrap();
        assert_eq!(info.product_id, Some(7));

        // update_stock returns the previous value
        let prev = reg.update_stock("AA:BB:CC:DD:EE:FF", 5).await;
        assert_eq!(prev, None); // first update: no previous value

        let prev = reg.update_stock("AA:BB:CC:DD:EE:FF", 6).await;
        assert_eq!(prev, Some(5));
    }

    #[tokio::test]
    async fn registry_count_and_get_all() {
        let reg = DeviceRegistry::new();
        reg.upsert_seen("11:22:33:44:55:66", None, None).await;
        reg.upsert_seen("AA:BB:CC:DD:EE:FF", None, None).await;

        assert_eq!(reg.count().await, 2);

        let all = reg.get_all().await;
        assert_eq!(all.len(), 2);
        // sorted by address
        assert_eq!(all[0].address, "11:22:33:44:55:66");
        assert_eq!(all[1].address, "AA:BB:CC:DD:EE:FF");
    }
}
