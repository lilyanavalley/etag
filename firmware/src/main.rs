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

use defmt_rtt as _; // global logger
use embassy_executor::Spawner;
use embassy_nrf::{
    bind_interrupts,
    gpio::{Input, Level, Output, OutputDrive, Pull},
    peripherals,
    saadc::{self, AnyInput},
    spim::{self, Spim},
};
use embassy_time::{Duration, Timer};
use panic_probe as _; // panic handler

// Pure-logic modules come from the companion library crate (`src/lib.rs`).
use etag::grocy::{grocycode_for_product, TagState};
use etag::mesh::MeshTagPublisher;
use etag::qr::QrCode;

use buttons::ButtonEvent;
use display::EpdDisplay;

// ─────────────────────────────────────────────────────────────────────────────
// Interrupt bindings required by Embassy drivers
// ─────────────────────────────────────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    // TWISPI0 = SPIM0/TWIM0 shared peripheral on nRF52840 (embassy-nrf 0.2 naming)
    SPIM0_SPIS0_TWIM0_TWIS0_SPI0_TWI0 => spim::InterruptHandler<peripherals::TWISPI0>;
    // SAADC
    SAADC => saadc::InterruptHandler;
    // Note: GPIOTE is initialised automatically by embassy_nrf::init()
});

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // ── Initialise nRF52840 peripherals ──────────────────────────────────────
    let p = embassy_nrf::init(Default::default());
    defmt::info!("etag starting up");

    // ── SPI for ePaper display ────────────────────────────────────────────────
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spim::Frequency::M4;
    spi_config.mode = spim::MODE_0;

    let spi: Spim<peripherals::TWISPI0> = Spim::new_txonly(
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
    let product_id = MeshTagPublisher::parse_product_id(state.grocycode.as_str()).unwrap_or(1);
    let mut mesh_publisher = MeshTagPublisher::new(product_id);

    // ── Initialise display ────────────────────────────────────────────────────
    display.init().await;
    defmt::info!("Display initialised");

    // ── Main event loop ───────────────────────────────────────────────────────
    loop {
        // Read battery level.
        let bat_pct = battery::read_percent(&mut saadc).await;
        defmt::info!("Battery: {}%", bat_pct);

        // Generate QR code for the current Grocycode.
        let grocycode_bytes = state.grocycode.as_bytes();
        match QrCode::encode(grocycode_bytes) {
            Some(qr) => {
                // Render the full display layout.
                display.render(&qr, state.product_name.as_str(), state.stock_count, bat_pct);
                // Flush framebuffer → ePaper (blocking ~2 s).
                display.full_update().await;
            }
            None => {
                defmt::error!("QR encode failed (grocycode too long?)");
            }
        }

        // Wait for a button press.
        let event = buttons::wait_for_button(&mut btn_up, &mut btn_dn).await;
        match event {
            ButtonEvent::Increment => {
                state.increment();
                defmt::info!("Stock → {}", state.stock_count);
                let _frame = mesh_publisher.build_delta_frame(1, bat_pct);
                defmt::debug!(
                    "mesh publish delta product={} delta={} battery={}",
                    product_id,
                    1,
                    bat_pct
                );
            }
            ButtonEvent::Decrement => {
                state.decrement();
                defmt::info!("Stock → {}", state.stock_count);
                let _frame = mesh_publisher.build_delta_frame(-1, bat_pct);
                defmt::debug!(
                    "mesh publish delta product={} delta={} battery={}",
                    product_id,
                    -1,
                    bat_pct
                );
            }
        }

        // Small delay to allow any BLE notification to be sent (future use).
        Timer::after(Duration::from_millis(50)).await;
    }
}
