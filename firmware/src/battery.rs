//! Battery voltage monitoring via the nRF52840 SAADC.
//!
//! A resistor divider (e.g. 1 MΩ top + 1 MΩ bottom) halves the LiPo cell
//! voltage before it reaches the SAADC input on P0.02 / AIN0.  Adjust
//! `VBAT_DIVIDER_RATIO` in `config.rs` if your divider differs.
//!
//! # Wiring
//! ```text
//! V_BATT ──┤ R1 (1 MΩ) ├──┬── P0.02 / AIN0
//!                          │
//!                    R2 (1 MΩ)
//!                          │
//!                         GND
//! ```
//!
//! # Usage
//! ```no_run
//! let pct = battery::read_percent(&mut saadc).await;
//! defmt::info!("Battery: {}%", pct);
//! ```

use embassy_nrf::{
    peripherals,
    saadc::{self, AnyInput, ChannelConfig, Config, Saadc},
    Peripheral,
};

use etag::config::{SAADC_MAX, SAADC_VREF, VBAT_ATTENUATION, VBAT_EMPTY_V, VBAT_FULL_V};

/// Read the battery percentage (0 – 100).
///
/// `saadc` must be a configured `Saadc<1>` instance (single channel).
/// The function performs a single SAADC conversion and returns the result as
/// an integer percentage clamped to [0, 100].
pub async fn read_percent(saadc: &mut Saadc<'_, 1>) -> u8 {
    let mut buf = [0i16; 1];
    saadc.sample(&mut buf).await;

    // Convert raw ADC reading to voltage at the pin.
    let raw = buf[0].max(0) as f32;
    let v_pin = (raw / SAADC_MAX) * SAADC_VREF;

    // Account for the resistor divider to get actual battery voltage.
    // v_batt = v_pin / VBAT_ATTENUATION  (e.g. divide by 0.5 = multiply by 2)
    let v_batt = v_pin / VBAT_ATTENUATION;

    // Map voltage to 0-100 %.
    let pct = ((v_batt - VBAT_EMPTY_V) / (VBAT_FULL_V - VBAT_EMPTY_V) * 100.0) as i32;
    pct.clamp(0, 100) as u8
}

/// Create a configured [`Saadc`] instance for the battery monitor pin.
///
/// Call this once in `main` and pass the result to [`read_percent`].
pub fn create_saadc<'d>(
    saadc_peri: impl Peripheral<P = peripherals::SAADC> + 'd,
    irqs: impl embassy_nrf::interrupt::typelevel::Binding<
        embassy_nrf::interrupt::typelevel::SAADC,
        saadc::InterruptHandler,
    > + 'd,
    pin: AnyInput,
) -> Saadc<'d, 1> {
    let config = Config::default();
    let channel_config = ChannelConfig::single_ended(pin);
    Saadc::new(saadc_peri, irqs, config, [channel_config])
}
