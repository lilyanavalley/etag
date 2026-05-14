//! BLE central — scans for etag devices and manages GATT connections.
//!
//! # Overview
//!
//! ```text
//!  run()
//!  ├── init BLE adapter (btleplug / BlueZ on Linux)
//!  ├── spawn stats_reporter task
//!  └── scan loop
//!       ├── start_scan (filter: etag service UUID)
//!       ├── event loop (scan_duration seconds)
//!       │    DeviceDiscovered / DeviceUpdated
//!       │      → upsert device in DeviceRegistry
//!       │      → if new: spawn manage_device task
//!       │    DeviceConnected / DeviceDisconnected
//!       │      → update connected flag in registry
//!       ├── stop_scan
//!       └── pause scan_interval seconds, then repeat
//!
//!  manage_device(peripheral)    ← one task per device
//!  ├── connect()
//!  ├── discover_services()
//!  ├── read grocycode, product_name, stock_count, battery_pct
//!  ├── subscribe to stock_count + battery_pct notifications
//!  └── notification loop
//!       stock_count change → GrocyClient::sync_stock()
//!       battery_pct change → DeviceRegistry::update_battery()
//!                            low-battery warning if ≤ 10 %
//! ```
//!
//! # Reconnection
//!
//! When a device task exits (GATT error or disconnection), the device is
//! removed from the "managed" set so that the next scan pass will spawn a new
//! task for it.
//!
//! # BLE GATT service UUIDs (etag firmware)
//!
//! ```text
//! Service      4fa0e000-1081-4329-9b34-1c4bfd86c2f4
//! grocycode    4fa0e001-1081-4329-9b34-1c4bfd86c2f4  (read,              UTF-8)
//! product_name 4fa0e002-1081-4329-9b34-1c4bfd86c2f4  (read/write,        UTF-8)
//! stock_count  4fa0e003-1081-4329-9b34-1c4bfd86c2f4  (read/write/notify, i32 LE)
//! battery_pct  4fa0e004-1081-4329-9b34-1c4bfd86c2f4  (read/notify,       u8)
//! ```

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::stream::StreamExt;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// etag GATT UUIDs
// ─────────────────────────────────────────────────────────────────────────────

/// Custom BLE service UUID broadcast by every etag device.
const ETAG_SERVICE_UUID: Uuid = Uuid::from_u128(0x4fa0e000_1081_4329_9b34_1c4bfd86c2f4);

/// `grocycode` characteristic — UTF-8 Grocycode string (e.g. `"grcy-p-42"`).
const CHAR_GROCYCODE: Uuid = Uuid::from_u128(0x4fa0e001_1081_4329_9b34_1c4bfd86c2f4);

/// `product_name` characteristic — UTF-8 human-readable product label.
const CHAR_PRODUCT_NAME: Uuid = Uuid::from_u128(0x4fa0e002_1081_4329_9b34_1c4bfd86c2f4);

/// `stock_count` characteristic — little-endian `i32` current stock quantity.
const CHAR_STOCK_COUNT: Uuid = Uuid::from_u128(0x4fa0e003_1081_4329_9b34_1c4bfd86c2f4);

/// `battery_pct` characteristic — `u8` battery level 0–100 %.
const CHAR_BATTERY_PCT: Uuid = Uuid::from_u128(0x4fa0e004_1081_4329_9b34_1c4bfd86c2f4);

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Initialise the BLE adapter and run the scan + connection loop indefinitely.
///
/// Spawns a [`stats_reporter`] task immediately, then enters an outer loop that
/// repeatedly starts a BLE scan, processes discovered devices, and pauses
/// before the next pass.
///
/// Returns an error only if the BLE adapter cannot be initialised.
pub async fn run(state: Arc<AppState>) -> Result<()> {
    let manager = Manager::new()
        .await
        .context("Failed to initialise BLE manager — is Bluetooth enabled?")?;

    let adapters = manager
        .adapters()
        .await
        .context("Failed to enumerate BLE adapters")?;

    let central = adapters
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No BLE adapter found — check `bluetoothctl show`"))?;

    info!("BLE adapter ready");

    // Spawn the periodic stats reporter.
    {
        let state = Arc::clone(&state);
        tokio::spawn(stats_reporter(state));
    }

    // Set of BLE addresses currently managed by a device task.  When a device
    // task exits it removes itself, allowing re-discovery on the next scan pass.
    let managed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    // Filter: only report peripherals that advertise the etag service UUID so
    // the bridge does not attempt to connect to unrelated BLE devices.
    let scan_filter = ScanFilter {
        services: vec![ETAG_SERVICE_UUID],
    };

    loop {
        info!("Starting BLE scan");

        central
            .start_scan(scan_filter.clone())
            .await
            .context("Failed to start BLE scan")?;

        let scan_end = time::sleep(Duration::from_secs(state.config.scan_duration_secs));
        tokio::pin!(scan_end);

        let mut events = central
            .events()
            .await
            .context("Failed to obtain BLE event stream")?;

        // Process events until the scan window closes.
        loop {
            tokio::select! {
                _ = &mut scan_end => break,

                Some(event) = events.next() => {
                    handle_central_event(
                        &state,
                        &central,
                        &managed,
                        event,
                    ).await;
                }
            }
        }

        central
            .stop_scan()
            .await
            .context("Failed to stop BLE scan")?;

        let pause = Duration::from_secs(state.config.scan_interval_secs);
        if !pause.is_zero() {
            debug!(secs = state.config.scan_interval_secs, "Scan cooldown");
            time::sleep(pause).await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Central event handler
// ─────────────────────────────────────────────────────────────────────────────

/// Dispatch a single [`CentralEvent`] from the BLE adapter.
async fn handle_central_event(
    state: &Arc<AppState>,
    central: &Adapter,
    managed: &Arc<Mutex<HashSet<String>>>,
    event: CentralEvent,
) {
    match event {
        // A peripheral was found (or its advertisement data updated).
        CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {
            let peripheral = match central.peripheral(&id).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(?e, "Could not retrieve peripheral from adapter");
                    return;
                }
            };

            // Collect advertisement metadata.
            let props = peripheral.properties().await.ok().flatten();
            let addr  = peripheral.address().to_string();
            let name  = props.as_ref().and_then(|p| p.local_name.clone());
            let rssi  = props.as_ref().and_then(|p| p.rssi).map(|r| r as i16);

            state.registry.upsert_seen(&addr, name, rssi).await;

            // Spawn a management task if this device is not already being handled.
            let mut m = managed.lock().await;
            if !m.contains(&addr) {
                info!(address = %addr, "Scheduling GATT connection");
                m.insert(addr.clone());
                drop(m); // release lock before spawning

                let state   = Arc::clone(state);
                let managed = Arc::clone(managed);

                tokio::spawn(async move {
                    manage_device(Arc::clone(&state), peripheral, addr.clone()).await;
                    // Always remove from the managed set on exit so re-discovery works.
                    managed.lock().await.remove(&addr);
                });
            }
        }

        // GATT connection established.
        CentralEvent::DeviceConnected(id) => {
            if let Ok(p) = central.peripheral(&id).await {
                let addr = p.address().to_string();
                info!(address = %addr, "GATT connected");
                state.registry.set_connected(&addr, true).await;
            }
        }

        // GATT connection lost.
        CentralEvent::DeviceDisconnected(id) => {
            if let Ok(p) = central.peripheral(&id).await {
                let addr = p.address().to_string();
                info!(address = %addr, "GATT disconnected");
                state.registry.set_connected(&addr, false).await;
            }
        }

        _ => {} // Manufacturer / service data advertisements — not needed here.
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-device management task
// ─────────────────────────────────────────────────────────────────────────────

/// Connect to one etag device and handle it until it disconnects or errors.
///
/// This task runs for the lifetime of a single GATT connection.  When it exits,
/// the caller removes the device from the `managed` set so that the scan loop
/// can respawn the task on the next pass.
async fn manage_device(state: Arc<AppState>, peripheral: Peripheral, addr: String) {
    let span = tracing::info_span!("device", address = %addr);
    let _enter = span.enter();

    if let Err(e) = connect_and_sync(&state, &peripheral, &addr).await {
        error!(?e, "Device task exited with error");
        state.registry.set_connected(&addr, false).await;
    }
}

/// Connect → discover → read → subscribe → notification loop.
///
/// Returns `Ok(())` when the notification stream ends (device disconnected
/// gracefully) or an error on any GATT failure.
async fn connect_and_sync(
    state: &Arc<AppState>,
    peripheral: &Peripheral,
    addr: &str,
) -> Result<()> {
    // ── Connect ───────────────────────────────────────────────────────────────
    info!("Connecting…");
    peripheral
        .connect()
        .await
        .context("GATT connect failed")?;

    // Refresh RSSI now that we have a connection.
    if let Ok(Some(props)) = peripheral.properties().await {
        if let Some(rssi) = props.rssi {
            state.registry.update_rssi(addr, rssi as i16).await;
        }
    }

    // ── Service / characteristic discovery ───────────────────────────────────
    peripheral
        .discover_services()
        .await
        .context("Service discovery failed")?;

    let chars = peripheral.characteristics();

    // Helper closure: look up a characteristic by UUID, warn if absent.
    let find_char = |uuid: Uuid| -> Option<btleplug::api::Characteristic> {
        let c = chars.iter().find(|c| c.uuid == uuid).cloned();
        if c.is_none() {
            warn!(%uuid, "Characteristic not found — device may run older firmware");
        }
        c
    };

    // ── Initial characteristic reads ──────────────────────────────────────────
    if let Some(c) = find_char(CHAR_GROCYCODE) {
        let data = peripheral.read(&c).await.context("Read grocycode")?;
        let code = String::from_utf8_lossy(&data).into_owned();
        info!(grocycode = %code, "Grocycode read");
        state.registry.update_grocycode(addr, &code).await;
    }

    if let Some(c) = find_char(CHAR_PRODUCT_NAME) {
        let data = peripheral.read(&c).await.context("Read product_name")?;
        let name = String::from_utf8_lossy(&data).into_owned();
        info!(product_name = %name, "Product name read");
        state.registry.update_product_name(addr, &name).await;
    }

    if let Some(c) = find_char(CHAR_STOCK_COUNT) {
        let data = peripheral.read(&c).await.context("Read stock_count")?;
        if let Some(count) = decode_stock_count(&data) {
            info!(stock_count = count, "Stock count read");
            state.registry.update_stock(addr, count).await;
        }
    }

    if let Some(c) = find_char(CHAR_BATTERY_PCT) {
        let data = peripheral.read(&c).await.context("Read battery_pct")?;
        if let Some(&pct) = data.first() {
            info!(battery_pct = pct, "Battery percentage read");
            state.registry.update_battery(addr, pct).await;
        }
    }

    // ── Subscribe to notifications ────────────────────────────────────────────
    let stock_char   = find_char(CHAR_STOCK_COUNT);
    let battery_char = find_char(CHAR_BATTERY_PCT);

    if let Some(ref c) = stock_char {
        peripheral
            .subscribe(c)
            .await
            .context("Subscribe to stock_count")?;
        info!("Subscribed to stock_count notifications");
    }

    if let Some(ref c) = battery_char {
        peripheral
            .subscribe(c)
            .await
            .context("Subscribe to battery_pct")?;
        info!("Subscribed to battery_pct notifications");
    }

    state.registry.set_connected(addr, true).await;

    // ── Notification loop ─────────────────────────────────────────────────────
    let mut notif_stream = peripheral
        .notifications()
        .await
        .context("Failed to obtain notification stream")?;

    info!("Waiting for notifications…");

    while let Some(notif) = notif_stream.next().await {
        on_notification(state, addr, notif.uuid, notif.value).await;
    }

    info!("Notification stream ended");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Notification handler
// ─────────────────────────────────────────────────────────────────────────────

/// Decode and act on a single incoming GATT notification from the device.
///
/// - `stock_count` notification → update registry, call [`GrocyClient::sync_stock`].
/// - `battery_pct` notification → update registry, emit a low-battery warning at ≤ 10 %.
async fn on_notification(state: &Arc<AppState>, addr: &str, uuid: Uuid, value: Vec<u8>) {
    if uuid == CHAR_STOCK_COUNT {
        let new_count = match decode_stock_count(&value) {
            Some(c) => c,
            None => {
                warn!(
                    addr,
                    len = value.len(),
                    "stock_count notification has unexpected length — expected 4 bytes"
                );
                return;
            }
        };

        debug!(addr, new_count, "stock_count notification received");

        let prev = state.registry.update_stock(addr, new_count).await;

        // Forward the change to Grocy.
        match state.registry.get(addr).await {
            Some(info) => match info.product_id {
                Some(product_id) => {
                    let previous = prev.unwrap_or(new_count); // fallback → no-op delta
                    if let Err(e) = state.grocy.sync_stock(product_id, previous, new_count).await {
                        error!(?e, product_id, addr, "Grocy sync failed");
                    }
                }
                None => {
                    warn!(addr, "No product_id known for device — cannot sync to Grocy");
                }
            },
            None => {
                warn!(addr, "Device not in registry — cannot sync to Grocy");
            }
        }
    } else if uuid == CHAR_BATTERY_PCT {
        if let Some(&pct) = value.first() {
            debug!(addr, battery_pct = pct, "battery_pct notification received");
            state.registry.update_battery(addr, pct).await;
            if pct <= 10 {
                warn!(addr, battery_pct = pct, "⚠ Low battery — consider replacing soon");
            }
        }
    } else {
        debug!(addr, %uuid, "Notification for unrecognised characteristic — ignoring");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stats reporter
// ─────────────────────────────────────────────────────────────────────────────

/// Periodically emit a human-readable summary of all known devices to the log.
///
/// Runs as an independent Tokio task for the lifetime of the process.
async fn stats_reporter(state: Arc<AppState>) {
    let interval = Duration::from_secs(state.config.stats_interval_secs);
    let mut ticker = time::interval(interval);
    ticker.tick().await; // skip the first immediate tick

    loop {
        ticker.tick().await;

        let devices = state.registry.get_all().await;
        if devices.is_empty() {
            info!("No etag devices seen yet");
            continue;
        }

        info!("─── etag device summary ({} device(s)) ───", devices.len());

        for d in &devices {
            let product = d
                .product_name
                .as_deref()
                .or(d.grocycode.as_deref())
                .unwrap_or("(unknown)");

            let stock   = d.stock_count .map(|s| s.to_string())      .unwrap_or_else(|| "?".to_owned());
            let battery = d.battery_pct .map(|b| format!("{b}%"))    .unwrap_or_else(|| "?".to_owned());
            let rssi    = d.rssi        .map(|r| format!("{r} dBm"))  .unwrap_or_else(|| "?".to_owned());

            let age_secs = chrono::Utc::now()
                .signed_duration_since(d.last_seen)
                .num_seconds();

            let conn_icon = if d.connected { "●" } else { "○" };

            // Log structured fields AND a human-readable message in one call so
            // both machine-parseable log shippers and human readers are happy.
            info!(
                address        = %d.address,
                product,
                stock          = %stock,
                battery        = %battery,
                rssi           = %rssi,
                connected      = d.connected,
                last_seen_secs = age_secs,
                "{conn_icon} {} | {} | stock={stock} | bat={battery} | rssi={rssi} | last seen {age_secs}s ago",
                d.address,
                product,
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Decode a 4-byte little-endian `i32` from a characteristic value payload.
///
/// Returns `None` if the slice is shorter than 4 bytes.
fn decode_stock_count(data: &[u8]) -> Option<i32> {
    if data.len() < 4 {
        return None;
    }
    Some(i32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_stock_count_valid() {
        assert_eq!(decode_stock_count(&[0x00, 0x00, 0x00, 0x00]), Some(0));
        assert_eq!(decode_stock_count(&[0x0A, 0x00, 0x00, 0x00]), Some(10));
        assert_eq!(decode_stock_count(&[0xFF, 0xFF, 0xFF, 0x7F]), Some(i32::MAX));
        // negative value (i32 -1 in LE)
        assert_eq!(decode_stock_count(&[0xFF, 0xFF, 0xFF, 0xFF]), Some(-1));
    }

    #[test]
    fn decode_stock_count_too_short() {
        assert_eq!(decode_stock_count(&[]), None);
        assert_eq!(decode_stock_count(&[0x01, 0x02, 0x03]), None);
    }

    #[test]
    fn uuid_constants_are_correct() {
        // Spot-check: the last nibble of each UUID should match its suffix.
        assert_eq!(ETAG_SERVICE_UUID.to_string(), "4fa0e000-1081-4329-9b34-1c4bfd86c2f4");
        assert_eq!(CHAR_GROCYCODE.to_string(),    "4fa0e001-1081-4329-9b34-1c4bfd86c2f4");
        assert_eq!(CHAR_STOCK_COUNT.to_string(),  "4fa0e003-1081-4329-9b34-1c4bfd86c2f4");
        assert_eq!(CHAR_BATTERY_PCT.to_string(),  "4fa0e004-1081-4329-9b34-1c4bfd86c2f4");
    }
}
