//! BLE Mesh foundation runtime for the bridge.
//!
//! This module intentionally keeps transport ingestion decoupled from business
//! logic so future BlueZ/meshctl/DBus integration can feed events into the same
//! idempotent processing path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, info, warn};

use crate::AppState;

/// Result of applying one mesh inventory event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Event dropped because its revision is stale/duplicate.
    IgnoredDuplicate,
    /// First known value for this node; baseline set, no Grocy delta sync yet.
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
    /// Absolute stock count.
    pub stock_count: i32,
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

/// Apply a mesh inventory event with dedup/idempotency checks.
///
/// On first-seen values for a node, this records a baseline without issuing a
/// Grocy delta request to avoid accidental bootstrap mutations.
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

    let previous = state
        .registry
        .update_stock(&synthetic_addr, event.stock_count)
        .await;

    if let Some(pct) = event.battery_pct {
        state.registry.update_battery(&synthetic_addr, pct).await;
    }

    let Some(previous) = previous else {
        return Ok(ApplyOutcome::BaselineApplied);
    };

    if previous == event.stock_count {
        return Ok(ApplyOutcome::BaselineApplied);
    }

    state
        .grocy
        .sync_stock(event.product_id, previous, event.stock_count)
        .await?;

    Ok(ApplyOutcome::SyncedToGrocy)
}

/// Mesh-mode runtime loop (Phase-0 foundation).
///
/// This mode keeps the process alive and reports status while transport ingest
/// integration is implemented in subsequent phases.
pub async fn run(state: Arc<AppState>) -> Result<()> {
    warn!("Mesh mode enabled (Phase-0): ingestion not yet implemented");

    let mut ticker = time::interval(Duration::from_secs(state.config.mesh_poll_interval_secs));
    ticker.tick().await;

    loop {
        ticker.tick().await;
        let known = state.registry.count().await;
        info!(known_devices = known, "mesh-mode heartbeat");
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
}
