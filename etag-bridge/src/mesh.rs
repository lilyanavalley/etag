//! BLE Mesh foundation runtime for the bridge.
//!
//! This module keeps transport ingestion decoupled from business logic so future
//! BlueZ/meshctl/DBus integration can feed events into the same idempotent
//! processing path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, info, warn};

use crate::AppState;

/// Mesh wire operation kind for inventory updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshInventoryOp {
    /// Delta update from local button interaction.
    StockDelta,
    /// Absolute stock value update.
    StockAbsolute,
}

impl TryFrom<u8> for MeshInventoryOp {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::StockDelta),
            1 => Ok(Self::StockAbsolute),
            _ => Err(()),
        }
    }
}

/// Result of applying one mesh inventory event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Event dropped because its revision is stale/duplicate.
    IgnoredDuplicate,
    /// First known absolute value for this node; baseline set, no Grocy sync yet.
    BaselineApplied,
    /// New value accepted and synced to Grocy.
    SyncedToGrocy,
}

/// Bridge-facing representation of a mesh inventory event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshInventoryEvent {
    /// Mesh unicast address of the source node.
    pub node_unicast: u16,
    /// Monotonic per-node revision.
    pub revision: u32,
    /// Grocy product ID associated with this node.
    pub product_id: u32,
    /// Inventory operation kind.
    pub op: MeshInventoryOp,
    /// Stock value (`delta` or `absolute` depending on `op`).
    pub stock_value: i32,
    /// Optional battery telemetry.
    pub battery_pct: Option<u8>,
}

/// Per-node revision tracker used for idempotency and duplicate rejection.
#[derive(Debug, Default)]
pub struct MeshRevisionGuard {
    inner: RwLock<HashMap<String, u32>>,
}

impl MeshRevisionGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept only strictly increasing revisions for `node_key`.
    ///
    /// Returns `false` for duplicate or stale revisions (`revision <= last_seen`).
    /// On acceptance, the latest revision is stored internally and `true` is returned.
    async fn accept(&self, node_key: &str, revision: u32) -> bool {
        let mut map = self.inner.write().await;
        match map.get(node_key).copied() {
            Some(prev) if revision <= prev => false,
            _ => {
                map.insert(node_key.to_owned(), revision);
                true
            }
        }
    }
}

/// Decode one UDP mesh-ingest packet.
///
/// Packet format (little-endian):
/// - `node_unicast: u16`
/// - `revision: u32`
/// - `product_id: u32`
/// - `op: u8` (`0=delta`, `1=absolute`)
/// - `stock_value: i32`
/// - `battery_pct: u8` (`0..=100`, else treated as unknown)
pub fn decode_inventory_packet(data: &[u8]) -> Option<MeshInventoryEvent> {
    const PACKET_LEN: usize = 16;
    if data.len() != PACKET_LEN {
        return None;
    }

    let node_unicast = u16::from_le_bytes([data[0], data[1]]);
    let revision = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let product_id = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
    let op = MeshInventoryOp::try_from(data[10]).ok()?;
    let stock_value = i32::from_le_bytes([data[11], data[12], data[13], data[14]]);
    let battery_pct = (data[15] <= 100).then_some(data[15]);

    Some(MeshInventoryEvent {
        node_unicast,
        revision,
        product_id,
        op,
        stock_value,
        battery_pct,
    })
}

/// Apply a mesh inventory event with dedup/idempotency checks.
///
/// For `StockAbsolute`, first-seen values are treated as baseline and do not
/// mutate Grocy to avoid accidental bootstrap drifts.
pub async fn apply_inventory_event(
    state: &Arc<AppState>,
    guard: &MeshRevisionGuard,
    event: &MeshInventoryEvent,
) -> Result<ApplyOutcome> {
    let node_key = format!("mesh:{:04X}", event.node_unicast);

    if !guard.accept(&node_key, event.revision).await {
        debug!(
            node_key,
            revision = event.revision,
            "Ignoring duplicate mesh event"
        );
        return Ok(ApplyOutcome::IgnoredDuplicate);
    }

    let synthetic_addr = node_key;
    let synthetic_name = format!("mesh-node-{:04X}", event.node_unicast);

    state
        .registry
        .upsert_seen(&synthetic_addr, Some(synthetic_name), None)
        .await;

    state
        .registry
        .update_grocycode(&synthetic_addr, &format!("grcy-p-{}", event.product_id))
        .await;

    let prior_stock = state
        .registry
        .get(&synthetic_addr)
        .await
        .and_then(|d| d.stock_count)
        .unwrap_or(0);

    let resolved_stock = match event.op {
        MeshInventoryOp::StockDelta => prior_stock.saturating_add(event.stock_value),
        MeshInventoryOp::StockAbsolute => event.stock_value,
    };

    let previous = state
        .registry
        .update_stock(&synthetic_addr, resolved_stock)
        .await;

    if let Some(pct) = event.battery_pct {
        state.registry.update_battery(&synthetic_addr, pct).await;
    }

    let sync_previous = match event.op {
        MeshInventoryOp::StockDelta => match previous {
            Some(prev) => prev,
            None => {
                debug!(
                    node = %synthetic_addr,
                    assumed_baseline = prior_stock,
                    "first delta event assumes local baseline"
                );
                prior_stock
            }
        },
        MeshInventoryOp::StockAbsolute => match previous {
            Some(prev) => prev,
            None => return Ok(ApplyOutcome::BaselineApplied),
        },
    };

    if sync_previous == resolved_stock {
        return Ok(ApplyOutcome::BaselineApplied);
    }

    state
        .grocy
        .sync_stock(event.product_id, sync_previous, resolved_stock)
        .await?;

    Ok(ApplyOutcome::SyncedToGrocy)
}

/// Mesh-mode runtime loop (Phase-1 ingest foundation).
///
/// Listens for mesh inventory packets on UDP and applies idempotent ingest
/// handling before Grocy synchronization.
pub async fn run(state: Arc<AppState>) -> Result<()> {
    warn!(
        bind = %state.config.mesh_ingest_bind_addr,
        "Mesh mode enabled; waiting for UDP ingest packets"
    );

    let socket = UdpSocket::bind(&state.config.mesh_ingest_bind_addr)
        .await
        .with_context(|| {
            format!(
                "failed to bind mesh ingest UDP socket at {}",
                state.config.mesh_ingest_bind_addr
            )
        })?;

    info!(
        bind = %state.config.mesh_ingest_bind_addr,
        "mesh ingest UDP socket bound"
    );

    let guard = MeshRevisionGuard::new();
    let mut ticker = time::interval(Duration::from_secs(state.config.mesh_poll_interval_secs));
    let mut buf = [0_u8; 64];

    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let known = state.registry.count().await;
                info!(known_devices = known, "mesh-mode heartbeat");
            }
            recv = socket.recv_from(&mut buf) => {
                let (size, from) = recv.context("mesh UDP receive failed")?;
                let payload = &buf[..size];

                let Some(event) = decode_inventory_packet(payload) else {
                    warn!(bytes = size, from = %from, "Ignoring invalid mesh ingest packet");
                    continue;
                };

                let outcome = apply_inventory_event(&state, &guard, &event).await?;
                debug!(
                    from = %from,
                    node_unicast = event.node_unicast,
                    revision = event.revision,
                    ?outcome,
                    "mesh ingest packet applied"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revision_guard_rejects_duplicates() {
        let guard = MeshRevisionGuard::new();

        assert!(guard.accept("mesh:0001", 1).await);
        assert!(!guard.accept("mesh:0001", 1).await);
        assert!(!guard.accept("mesh:0001", 0).await);
        assert!(guard.accept("mesh:0001", 2).await);
    }

    #[test]
    fn decode_inventory_packet_roundtrip_layout() {
        let mut data = [0_u8; 16];
        data[0..2].copy_from_slice(&0x1234_u16.to_le_bytes());
        data[2..6].copy_from_slice(&7_u32.to_le_bytes());
        data[6..10].copy_from_slice(&42_u32.to_le_bytes());
        data[10] = 0;
        data[11..15].copy_from_slice(&(-2_i32).to_le_bytes());
        data[15] = 88;

        let event = decode_inventory_packet(&data).expect("valid packet");
        assert_eq!(event.node_unicast, 0x1234);
        assert_eq!(event.revision, 7);
        assert_eq!(event.product_id, 42);
        assert_eq!(event.op, MeshInventoryOp::StockDelta);
        assert_eq!(event.stock_value, -2);
        assert_eq!(event.battery_pct, Some(88));
    }

    #[test]
    fn decode_inventory_packet_rejects_invalid() {
        assert!(decode_inventory_packet(&[]).is_none());

        let mut data = [0_u8; 16];
        data[10] = 9;
        assert!(decode_inventory_packet(&data).is_none());
    }
}
