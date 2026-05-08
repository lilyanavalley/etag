//! Hardware pin assignments and compile-time constants for the etag board.
//!
//! Adjust the pin numbers here when porting to a different board layout.
//! All pin names use the nRF52840 P0/P1 port notation.

// ── ePaper display (SSD1680 controller, Waveshare 2.9" BW 296×128) ───────────
//
//  ePaper pin  │ nRF52840 pin │ Function
//  ────────────┼──────────────┼──────────────────────────────────────────────
//  SCK         │ P0.03        │ SPI clock
//  MOSI / SDI  │ P0.04        │ SPI data (display is write-only)
//  CS          │ P0.05        │ Active-low chip select (GPIO, manual)
//  DC          │ P0.06        │ Data / Command select  (high = data)
//  RST         │ P0.07        │ Active-low hardware reset
//  BUSY        │ P0.08        │ High while display is refreshing (input)
//  GND / 3.3V  │ supply rails │ Power
//
// The display uses SPI mode 0 (CPOL=0, CPHA=0) at up to 10 MHz.

/// nRF52840 SPI instance used for the display.
/// Maps to SPIM0_SPIS0_TWIM0_TWIS0_SPI0_TWI0 interrupt.
pub const DISP_SPI_INSTANCE: &str = "SPI0";

/// SPI clock frequency for the ePaper display (4 MHz is safe for long cables).
pub const DISP_SPI_FREQ_HZ: u32 = 4_000_000;

/// Physical display pixel dimensions.
pub const DISP_WIDTH_PX: usize = 296;
pub const DISP_HEIGHT_PX: usize = 128;

/// Framebuffer size in bytes (1 bit per pixel, packed MSB-first).
pub const DISP_BUF_BYTES: usize = DISP_WIDTH_PX * DISP_HEIGHT_PX / 8; // 4 736

// ── Buttons ───────────────────────────────────────────────────────────────────
//
//  The buttons are connected between the pin and GND; an internal pull-up
//  resistor is enabled so the idle state is HIGH and a press pulls the pin LOW.
//
//  Button │ nRF52840 pin │ Action
//  ───────┼──────────────┼────────────────────────────────
//  BTN_UP │ P0.11        │ Increment stock count by one
//  BTN_DN │ P0.12        │ Decrement stock count by one
//
/// Debounce window after a button edge is detected.
pub const BUTTON_DEBOUNCE_MS: u64 = 50;

// ── Battery monitor ───────────────────────────────────────────────────────────
//
//  A resistor divider (e.g. 1 MΩ + 1 MΩ) halves the LiPo cell voltage before
//  the SAADC input.  Adjust VBAT_DIVIDER_RATIO if your divider differs.
//
//  Battery ADC pin: P0.02 / AIN0
//
/// Voltage divider attenuation factor: the ratio of (pin voltage) to (battery voltage).
///
/// With a 1 MΩ top resistor and 1 MΩ bottom resistor, the ADC pin sees exactly
/// half the battery voltage, so the attenuation is 0.5.
/// To recover the battery voltage: `v_batt = v_pin / VBAT_ATTENUATION`.
/// Adjust this constant if your resistor values differ.
pub const VBAT_ATTENUATION: f32 = 0.5;

/// Full-charge voltage of a single-cell LiPo battery (volts).
pub const VBAT_FULL_V: f32 = 4.2;

/// Cut-off / empty voltage for a single-cell LiPo battery (volts).
pub const VBAT_EMPTY_V: f32 = 3.0;

/// SAADC reference voltage (internal 0.6 V × gain 1/6 = 3.6 V effective range).
pub const SAADC_VREF: f32 = 3.6;

/// SAADC 12-bit resolution maximum value.
pub const SAADC_MAX: f32 = 4096.0;

// ── Grocy / display layout ────────────────────────────────────────────────────

/// Maximum length (bytes) of a Grocy product name stored on the device.
pub const MAX_PRODUCT_NAME_LEN: usize = 32;

/// Maximum length (bytes) of a Grocycode string (e.g. "grcy-p-12345").
pub const MAX_GROCYCODE_LEN: usize = 20;

/// QR code area occupies the left portion of the display (pixels).
pub const QR_AREA_PX: usize = 128;

/// Text area for product name / stock count (pixels wide).
pub const TEXT_AREA_PX: usize = DISP_WIDTH_PX - QR_AREA_PX; // 168

/// Minimum stock count (clamped at display / BLE update).
pub const STOCK_MIN: i32 = 0;

/// Maximum stock count representable on the display.
pub const STOCK_MAX: i32 = 9_999;

// ── BLE (optional, requires nrf-softdevice + S140) ───────────────────────────
//
//  The etag acts as a BLE Peripheral exposing the custom "Grocy Tag Service".
//  An external node (e.g. Raspberry Pi + Node.js) connects as Central, reads
//  product configuration and forwards stock changes to the Grocy REST API.
//
//  Service UUID  : 4fa0e000-1081-4329-9b34-1c4bfd86c2f4
//  Characteristic UUIDs:
//    grocycode   : 4fa0e001-…  (read,         UTF-8, max 20 bytes)
//    product_name: 4fa0e002-…  (read/write,   UTF-8, max 32 bytes)
//    stock_count : 4fa0e003-…  (read/write/notify, i32 little-endian)
//    battery_pct : 4fa0e004-…  (read/notify,  u8,  0-100)
