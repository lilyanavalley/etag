//! Grocy ERP data types, Grocycode helpers, and BLE GATT service definition.
//!
//! # Grocy integration model
//!
//! ```text
//!  ┌──────────────┐  BLE (GATT)  ┌────────────────────┐  HTTP  ┌─────────┐
//!  │  etag device │ ◄──────────► │  External BLE node  │ ──────►│  Grocy  │
//!  │  (nRF52840)  │              │  (RPi / PC)         │        │  ERP    │
//!  └──────────────┘              └────────────────────┘        └─────────┘
//! ```
//!
//! The etag device exposes a **Grocy Tag GATT Service** (custom 128-bit UUID).
//! An external BLE central (Raspberry Pi, PC, …) acts as a bridge between the
//! tag and the Grocy REST API:
//!
//! - It **writes** `grocycode` and `product_name` to the tag during provisioning.
//! - It **notifies** the tag when the remote stock count changes.
//! - When the user presses a button, the tag **writes** a new `stock_count` and
//!   the bridge forwards the change to Grocy via `/api/stock/products/{id}/add`
//!   or `/api/stock/products/{id}/consume`.
//!
//! # Grocycode format
//!
//! The QR code displayed on the tag encodes the **Grocycode** — a short
//! product-barcode-style identifier used by Grocy:
//!
//! ```text
//! grcy-p-{product_id}          (product)
//! grcy-lo-{location_id}        (location – not used here)
//! ```
//!
//! Example: product ID 42 → `"grcy-p-42"` (9 bytes → fits in QR version 1).
//!
//! # BLE GATT service UUIDs
//!
//! ```
//! Service      4fa0e000-1081-4329-9b34-1c4bfd86c2f4
//! grocycode    4fa0e001-1081-4329-9b34-1c4bfd86c2f4  (read, UTF-8, max 20 B)
//! product_name 4fa0e002-1081-4329-9b34-1c4bfd86c2f4  (read/write, max 32 B)
//! stock_count  4fa0e003-1081-4329-9b34-1c4bfd86c2f4  (read/write/notify, i32 LE)
//! battery_pct  4fa0e004-1081-4329-9b34-1c4bfd86c2f4  (read/notify, u8)
//! ```
//!
//! # Enabling BLE
//!
//! To add BLE support, add `nrf-softdevice` to `Cargo.toml`:
//!
//! ```toml
//! nrf-softdevice   = { version = "0.1", features = [
//!     "ble-peripheral", "ble-gatt-server", "s140", "nrf52840", "defmt"
//! ]}
//! nrf-softdevice-s140 = "0.1"
//! ```
//!
//! Then follow the nrf-softdevice examples to advertise and host the service,
//! using the GATT attribute definitions below as a guide.

use core::fmt;
use heapless::String;

use crate::config::{MAX_GROCYCODE_LEN, MAX_PRODUCT_NAME_LEN, STOCK_MAX, STOCK_MIN};

// ─────────────────────────────────────────────────────────────────────────────
// Core data types
// ─────────────────────────────────────────────────────────────────────────────

/// All persistent state associated with one inventory tag.
#[derive(Clone, PartialEq, Eq)]
pub struct TagState {
    /// Grocy product identifier (e.g. `"grcy-p-42"`).
    pub grocycode: String<MAX_GROCYCODE_LEN>,
    /// Human-readable product name (e.g. `"Oat Milk"`).
    pub product_name: String<MAX_PRODUCT_NAME_LEN>,
    /// Current in-stock quantity (clamped to [0, STOCK_MAX]).
    pub stock_count: i32,
}

impl TagState {
    /// Create a default [`TagState`] with empty strings and zero stock.
    pub const fn new() -> Self {
        Self {
            grocycode: String::new(),
            product_name: String::new(),
            stock_count: 0,
        }
    }

    /// Increment stock by one (clamped at [`STOCK_MAX`]).
    pub fn increment(&mut self) {
        if self.stock_count < STOCK_MAX {
            self.stock_count += 1;
        }
    }

    /// Decrement stock by one (clamped at [`STOCK_MIN`]).
    pub fn decrement(&mut self) {
        if self.stock_count > STOCK_MIN {
            self.stock_count -= 1;
        }
    }
}

impl Default for TagState {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Grocycode helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Build a Grocycode string for a given product ID.
///
/// Returns `None` if the resulting string would exceed [`MAX_GROCYCODE_LEN`].
///
/// # Example
/// ```
/// let code = grocycode_for_product(42).unwrap();
/// assert_eq!(code.as_str(), "grcy-p-42");
/// ```
pub fn grocycode_for_product(product_id: u32) -> Option<String<MAX_GROCYCODE_LEN>> {
    let mut s = String::new();
    fmt::write(&mut s, format_args!("grcy-p-{}", product_id)).ok()?;
    Some(s)
}

// ─────────────────────────────────────────────────────────────────────────────
// BLE event types (used whether or not BLE is compiled in)
// ─────────────────────────────────────────────────────────────────────────────

/// Events delivered to the application from the BLE layer.
#[derive(Debug, Clone)]
pub enum BleEvent {
    /// A BLE central has connected.
    Connected,
    /// The BLE central has disconnected.
    Disconnected,
    /// The central has written a new product name.
    ProductNameChanged(String<MAX_PRODUCT_NAME_LEN>),
    /// The central has written a new Grocycode.
    GrocycodeChanged(String<MAX_GROCYCODE_LEN>),
    /// The central has written a new stock count (e.g. after Grocy sync).
    StockCountChanged(i32),
}

// ─────────────────────────────────────────────────────────────────────────────
// BLE advertisement / GATT definitions (documentation only)
// ─────────────────────────────────────────────────────────────────────────────
//
// When nrf-softdevice is enabled, replace this section with the actual GATT
// server definition using the `#[nrf_softdevice::gatt_server]` macro.
//
// Example:
//
// ```rust
// use nrf_softdevice::ble::gatt_server;
//
// #[nrf_softdevice::gatt_server]
// pub struct GrocyTagServer {
//     pub service: GrocyTagService,
// }
//
// #[nrf_softdevice::gatt_service(uuid = "4fa0e000-1081-4329-9b34-1c4bfd86c2f4")]
// pub struct GrocyTagService {
//     #[characteristic(uuid = "4fa0e001-…", read)]
//     pub grocycode: heapless::Vec<u8, 20>,
//
//     #[characteristic(uuid = "4fa0e002-…", read, write)]
//     pub product_name: heapless::Vec<u8, 32>,
//
//     #[characteristic(uuid = "4fa0e003-…", read, write, notify)]
//     pub stock_count: i32,
//
//     #[characteristic(uuid = "4fa0e004-…", read, notify)]
//     pub battery_pct: u8,
// }
// ```
//
// Then in `main`, spawn the softdevice task and the GATT event loop.  See the
// nrf-softdevice README and examples for full setup instructions, including
// how to flash the S140 SoftDevice hex alongside the application.

/// BLE device name broadcast in advertisement packets.
pub const BLE_DEVICE_NAME: &str = "etag";

/// BLE advertisement appearance value: Electronic Label.
pub const BLE_APPEARANCE: u16 = 0x0200;

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grocycode_format() {
        let code = grocycode_for_product(1).unwrap();
        assert_eq!(code.as_str(), "grcy-p-1");
        let code = grocycode_for_product(99999).unwrap();
        assert_eq!(code.as_str(), "grcy-p-99999");
    }

    #[test]
    fn stock_clamp() {
        let mut state = TagState::new();
        state.stock_count = STOCK_MAX;
        state.increment(); // should not exceed STOCK_MAX
        assert_eq!(state.stock_count, STOCK_MAX);

        state.stock_count = STOCK_MIN;
        state.decrement(); // should not go below STOCK_MIN
        assert_eq!(state.stock_count, STOCK_MIN);
    }

    #[test]
    fn stock_increment_decrement() {
        let mut state = TagState::new();
        state.increment();
        state.increment();
        assert_eq!(state.stock_count, 2);
        state.decrement();
        assert_eq!(state.stock_count, 1);
    }
}
