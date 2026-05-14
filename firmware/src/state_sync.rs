//! Shared runtime update channels between external tasks (BLE/bridge) and main loop.
//!
//! External producers enqueue [`etag::grocy::BleEvent`] updates into an ingress
//! queue. A dedicated pump task forwards them into the main-loop queue that the
//! UI/render loop drains and applies.

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::{Channel, TryReceiveError, TrySendError},
};
use heapless::String;

use etag::{
    config::{MAX_GROCYCODE_LEN, MAX_PRODUCT_NAME_LEN},
    grocy::BleEvent,
};

const EXTERNAL_EVENT_QUEUE_LEN: usize = 8;
const BRIDGE_EVENT_QUEUE_LEN: usize = 8;

static EXTERNAL_EVENTS: Channel<CriticalSectionRawMutex, BleEvent, EXTERNAL_EVENT_QUEUE_LEN> =
    Channel::new();
static BRIDGE_EVENTS: Channel<CriticalSectionRawMutex, BleEvent, BRIDGE_EVENT_QUEUE_LEN> =
    Channel::new();

/// Raw external payload for runtime state updates.
///
/// Any `Some(...)` field is converted into one [`BleEvent`] and enqueued in
/// order using [`publish_bridge_update_best_effort`].
#[derive(Debug, Clone, Copy, Default)]
pub struct BridgeUpdate<'a> {
    pub connected: Option<bool>,
    pub product_name: Option<&'a str>,
    pub grocycode: Option<&'a str>,
    pub stock_count: Option<i32>,
}

/// Errors returned while translating or enqueueing bridge updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeUpdateError {
    ProductNameTooLong,
    GrocycodeTooLong,
    QueueFull,
}

/// Outcome of [`publish_bridge_update_best_effort`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BridgeUpdateBestEffortReport {
    pub published: u8,
    pub skipped_too_long: u8,
    pub skipped_queue_full: u8,
}

/// Enqueue an event from the BLE/bridge integration side.
///
/// This is the main producer API for external sync tasks.
#[allow(dead_code)]
pub fn publish_bridge_event(event: BleEvent) -> Result<(), BleEvent> {
    match BRIDGE_EVENTS.try_send(event) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(event)) => Err(event),
    }
}

/// Convert a raw bridge update into one or more [`BleEvent`] values.
///
/// Returns the number of events enqueued. The function fails fast if any field
/// cannot be represented in the firmware's fixed-capacity strings or if the
/// queue is full.
#[allow(dead_code)]
pub fn publish_bridge_update(update: BridgeUpdate<'_>) -> Result<u8, BridgeUpdateError> {
    let mut published = 0u8;

    if let Some(connected) = update.connected {
        let event = if connected {
            BleEvent::Connected
        } else {
            BleEvent::Disconnected
        };
        publish_bridge_event(event).map_err(|_| BridgeUpdateError::QueueFull)?;
        published = published.saturating_add(1);
    }

    if let Some(name) = update.product_name {
        let mut product_name = String::<MAX_PRODUCT_NAME_LEN>::new();
        product_name
            .push_str(name)
            .map_err(|_| BridgeUpdateError::ProductNameTooLong)?;
        publish_bridge_event(BleEvent::ProductNameChanged(product_name))
            .map_err(|_| BridgeUpdateError::QueueFull)?;
        published = published.saturating_add(1);
    }

    if let Some(code) = update.grocycode {
        let mut grocycode = String::<MAX_GROCYCODE_LEN>::new();
        grocycode
            .push_str(code)
            .map_err(|_| BridgeUpdateError::GrocycodeTooLong)?;
        publish_bridge_event(BleEvent::GrocycodeChanged(grocycode))
            .map_err(|_| BridgeUpdateError::QueueFull)?;
        published = published.saturating_add(1);
    }

    if let Some(stock) = update.stock_count {
        publish_bridge_event(BleEvent::StockCountChanged(stock))
            .map_err(|_| BridgeUpdateError::QueueFull)?;
        published = published.saturating_add(1);
    }

    Ok(published)
}

/// Best-effort bridge payload publisher (recommended default).
///
/// Invalid fields (too-long strings) and queue backpressure are logged and
/// skipped, while other valid fields continue to be published.
#[allow(dead_code)]
pub fn publish_bridge_update_best_effort(update: BridgeUpdate<'_>) -> BridgeUpdateBestEffortReport {
    let mut report = BridgeUpdateBestEffortReport::default();

    if let Some(connected) = update.connected {
        let event = if connected {
            BleEvent::Connected
        } else {
            BleEvent::Disconnected
        };
        if publish_bridge_event(event).is_ok() {
            report.published = report.published.saturating_add(1);
        } else {
            report.skipped_queue_full = report.skipped_queue_full.saturating_add(1);
        }
    }

    if let Some(name) = update.product_name {
        let mut product_name = String::<MAX_PRODUCT_NAME_LEN>::new();
        match product_name.push_str(name) {
            Ok(()) => {
                if publish_bridge_event(BleEvent::ProductNameChanged(product_name)).is_ok() {
                    report.published = report.published.saturating_add(1);
                } else {
                    report.skipped_queue_full = report.skipped_queue_full.saturating_add(1);
                }
            }
            Err(_) => {
                report.skipped_too_long = report.skipped_too_long.saturating_add(1);
            }
        }
    }

    if let Some(code) = update.grocycode {
        let mut grocycode = String::<MAX_GROCYCODE_LEN>::new();
        match grocycode.push_str(code) {
            Ok(()) => {
                if publish_bridge_event(BleEvent::GrocycodeChanged(grocycode)).is_ok() {
                    report.published = report.published.saturating_add(1);
                } else {
                    report.skipped_queue_full = report.skipped_queue_full.saturating_add(1);
                }
            }
            Err(_) => {
                report.skipped_too_long = report.skipped_too_long.saturating_add(1);
            }
        }
    }

    if let Some(stock) = update.stock_count {
        if publish_bridge_event(BleEvent::StockCountChanged(stock)).is_ok() {
            report.published = report.published.saturating_add(1);
        } else {
            report.skipped_queue_full = report.skipped_queue_full.saturating_add(1);
        }
    }

    if report.skipped_too_long > 0 || report.skipped_queue_full > 0 {
        defmt::warn!(
            "Bridge update partial: published={}, too_long={}, queue_full={}",
            report.published,
            report.skipped_too_long,
            report.skipped_queue_full
        );
    }

    report
}

/// Wait for the next external BLE/bridge event.
pub async fn wait_bridge_event() -> BleEvent {
    BRIDGE_EVENTS.receive().await
}

/// Enqueue a state update from an external producer task.
///
/// If the queue is full, the event is dropped and the original event is
/// returned to the caller.
#[allow(dead_code)]
pub fn publish_external_event(event: BleEvent) -> Result<(), BleEvent> {
    match EXTERNAL_EVENTS.try_send(event) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(event)) => Err(event),
    }
}

/// Try to dequeue one external event, if any.
pub fn try_receive_external_event() -> Option<BleEvent> {
    match EXTERNAL_EVENTS.try_receive() {
        Ok(event) => Some(event),
        Err(TryReceiveError::Empty) => None,
    }
}
