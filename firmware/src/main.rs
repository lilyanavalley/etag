//! # etag — ePaper inventory tag for Grocy ERP on nRF52840
//!
//! ## Overview
//! This firmware runs on an nRF52840 microcontroller connected to:
//!
//! - A **Waveshare 2.9" BW ePaper display** (SSD1680, 296 × 128 px) via SPI
//! - **Two tactile buttons** (increment / decrement stock count)
//! - A **LiPo battery** with a resistor-divider into the SAADC
//!
//! The display shows a QR code encoding the Grocy product code ("Grocycode")
//! alongside the current in-stock quantity and battery level.
//!
//! ## Application flow
//!
//! ```text
//!  power-on / wake-up
//!      │
//!      ▼
//!  init peripherals ──► display initialisation (2 s)
//!      │
//!      ▼
//!  render & refresh ◄─────────────────────────────┐
//!      │                                           │
//!      ▼                                           │
//!  wait for button press  ──── BTN_UP ──► stock++  │
//!      │                  └─── BTN_DN ──► stock--  │
//!      └────────────────────────────────────────────
//! ```
//!
//! BLE support (optional) is described in `src/grocy.rs`.
#![no_std]
#![no_main]

mod battery;
mod buttons;
mod display;
mod state_sync;

use defmt_rtt as _; // global logger
use embassy_executor::Spawner;
use embassy_nrf::{
    bind_interrupts,
    gpio::{Input, Level, Output, OutputDrive, Pull},
    peripherals,
    saadc::{self, AnyInput},
    spim::{self, Spim},
};
use embassy_time::Duration;
use panic_probe as _; // panic handler

// Pure-logic modules come from the companion library crate (`src/lib.rs`).
use etag::qr::QrCode;
use etag::{
    config::{STOCK_MAX, STOCK_MIN},
    grocy::{grocycode_for_product, BleEvent, TagState},
};

use buttons::ButtonEvent;
use display::EpdDisplay;

/// How often the main loop wakes to process non-button updates.
const MAIN_TICK_MS: u64 = 200;
/// Battery is sampled every N ticks to avoid unnecessary ADC wakeups.
const BATTERY_SAMPLE_TICKS: u8 = 25; // 25 * 200 ms = 5 s
/// Redraw only if battery percentage changed by at least this many points.
const BATTERY_REDRAW_DELTA_PCT: u8 = 2;

// ─────────────────────────────────────────────────────────────────────────────
// Interrupt bindings required by Embassy drivers
// ─────────────────────────────────────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    // TWISPI0 = SPIM0/TWIM0 shared peripheral on nRF52840 (embassy-nrf 0.10 naming)
    TWISPI0 => spim::InterruptHandler<peripherals::TWISPI0>;
    // SAADC
    SAADC => saadc::InterruptHandler;
    // Note: GPIOTE is initialised automatically by embassy_nrf::init()
});

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn bridge_event_pump_task() {
    loop {
        let event = state_sync::wait_bridge_event().await;
        if state_sync::publish_external_event(event).is_err() {
            defmt::warn!("External update queue full; dropping bridge event");
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // ── Initialise nRF52840 peripherals ──────────────────────────────────────
    let p = embassy_nrf::init(Default::default());
    defmt::info!("etag starting up");

    // Bridge/BLE updates should call `state_sync::publish_bridge_update_best_effort(...)`.
    // This task forwards ingress events into the main loop's apply queue.
    match bridge_event_pump_task() {
        Ok(token) => spawner.spawn(token),
        Err(_) => defmt::panic!("Failed to spawn bridge event pump task"),
    }

    // ── SPI for ePaper display ────────────────────────────────────────────────
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spim::Frequency::M4;
    spi_config.mode = spim::MODE_0;

    let spi: Spim = Spim::new_txonly(
        p.TWISPI0, Irqs, p.P0_03, // SCK
        p.P0_04, // MOSI
        spi_config,
    );

    let cs = Output::new(p.P0_05, Level::High, OutputDrive::Standard);
    let dc = Output::new(p.P0_06, Level::Low, OutputDrive::Standard);
    let rst = Output::new(p.P0_07, Level::High, OutputDrive::Standard);
    let busy = Input::new(p.P0_08, Pull::None);

    let mut display = EpdDisplay::new(spi, dc, cs, rst, busy);

    // ── Buttons ───────────────────────────────────────────────────────────────
    let mut btn_up = Input::new(p.P0_11, Pull::Up);
    let mut btn_dn = Input::new(p.P0_12, Pull::Up);

    // ── Battery SAADC ─────────────────────────────────────────────────────────
    let bat_pin: AnyInput = {
        use embassy_nrf::saadc::Input as SaadcInput;
        p.P0_02.degrade_saadc()
    };
    let mut saadc = battery::create_saadc(p.SAADC, Irqs, bat_pin);

    // ── Application state ─────────────────────────────────────────────────────
    // Default: product 1, starting stock 0.
    // In a real deployment the BLE central writes the correct values during
    // provisioning; for development we hard-code them here.
    let mut state = TagState::new();
    state.grocycode = grocycode_for_product(1).unwrap_or_default();
    state
        .product_name
        .push_str("Product")
        .unwrap_or_else(|_| defmt::warn!("Product name truncated (capacity exceeded)"));

    // Draw once at boot.
    let mut bat_pct = battery::read_percent(&mut saadc).await;
    let mut last_rendered_state = state.clone();
    defmt::info!("Battery: {}%", bat_pct);
    match QrCode::encode(state.grocycode.as_bytes()) {
        Some(qr) => {
            display.init().await;
            display.render(&qr, state.product_name.as_str(), state.stock_count, bat_pct);
            display.full_update().await;
            display.deep_sleep().await;
        }
        None => defmt::error!("QR encode failed at boot (grocycode too long?)"),
    }

    // ── Main event loop ───────────────────────────────────────────────────────
    let mut battery_tick_count: u8 = 0;
    loop {
        // Wake periodically so we can process non-button updates as well.
        if let Some(event) = buttons::wait_for_button_timeout(
            &mut btn_up,
            &mut btn_dn,
            Duration::from_millis(MAIN_TICK_MS),
        )
        .await
        {
            match event {
                ButtonEvent::Increment => {
                    state.increment();
                    defmt::info!("Stock → {}", state.stock_count);
                }
                ButtonEvent::Decrement => {
                    state.decrement();
                    defmt::info!("Stock → {}", state.stock_count);
                }
            }
        }

        // Apply queued external updates (BLE/bridge task -> main loop).
        if apply_pending_external_updates(&mut state) {
            defmt::info!("Applied external state update");
        }

        // Detect whether a redraw is needed due to state or battery changes.
        let mut needs_redraw = state != last_rendered_state;

        battery_tick_count = battery_tick_count.wrapping_add(1);
        if battery_tick_count >= BATTERY_SAMPLE_TICKS {
            battery_tick_count = 0;
            let new_bat_pct = battery::read_percent(&mut saadc).await;
            if new_bat_pct.abs_diff(bat_pct) >= BATTERY_REDRAW_DELTA_PCT {
                bat_pct = new_bat_pct;
                defmt::info!("Battery: {}%", bat_pct);
                needs_redraw = true;
            }
        }

        if needs_redraw {
            match QrCode::encode(state.grocycode.as_bytes()) {
                Some(qr) => {
                    display.init().await;
                    display.render(&qr, state.product_name.as_str(), state.stock_count, bat_pct);
                    display.full_update().await;
                    display.deep_sleep().await;
                    last_rendered_state = state.clone();
                }
                None => defmt::error!("QR encode failed (grocycode too long?)"),
            }
        }
    }
}

fn apply_pending_external_updates(_state: &mut TagState) -> bool {
    let mut changed = false;

    while let Some(event) = state_sync::try_receive_external_event() {
        match event {
            BleEvent::Connected | BleEvent::Disconnected => {}
            BleEvent::ProductNameChanged(new_name) => {
                if _state.product_name != new_name {
                    _state.product_name = new_name;
                    changed = true;
                }
            }
            BleEvent::GrocycodeChanged(new_code) => {
                if _state.grocycode != new_code {
                    _state.grocycode = new_code;
                    changed = true;
                }
            }
            BleEvent::StockCountChanged(new_stock) => {
                let clamped = new_stock.clamp(STOCK_MIN, STOCK_MAX);
                if _state.stock_count != clamped {
                    _state.stock_count = clamped;
                    changed = true;
                }
            }
        }
    }

    changed
}
