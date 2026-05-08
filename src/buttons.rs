//! Debounced button handling for the two stock-control buttons.
//!
//! # Wiring
//! Both buttons are wired between their GPIO pin and GND.  An internal pull-up
//! resistor is enabled, so the resting state is HIGH and a press pulls the
//! pin LOW.
//!
//! | Button | Pin   | Action                 |
//! |--------|-------|------------------------|
//! | BTN_UP | P0.11 | Increment stock by one |
//! | BTN_DN | P0.12 | Decrement stock by one |
//!
//! # Usage
//! Call [`wait_for_button`] from an async Embassy task to suspend until either
//! button is pressed.

use embassy_nrf::gpio::Input;
use embassy_time::{Duration, Timer};

use etag::config::BUTTON_DEBOUNCE_MS;

/// Possible button events.
#[derive(Debug, defmt::Format, Clone, Copy, PartialEq, Eq)]
pub enum ButtonEvent {
    /// Increment stock count.
    Increment,
    /// Decrement stock count.
    Decrement,
}

/// Wait for either button to be pressed and return the corresponding event.
///
/// Includes a debounce delay of [`BUTTON_DEBOUNCE_MS`] milliseconds: after
/// detecting a falling edge the function waits briefly and confirms the pin
/// is still low before returning.
pub async fn wait_for_button(
    btn_up: &mut Input<'_>,
    btn_dn: &mut Input<'_>,
) -> ButtonEvent {
    loop {
        // Wait until either pin falls from HIGH → LOW.
        embassy_futures::select::select(
            btn_up.wait_for_falling_edge(),
            btn_dn.wait_for_falling_edge(),
        )
        .await;

        // Debounce: short delay then re-sample.
        Timer::after(Duration::from_millis(BUTTON_DEBOUNCE_MS)).await;

        if btn_up.is_low() {
            defmt::debug!("BTN_UP pressed");
            // Wait for release before returning.
            btn_up.wait_for_high().await;
            return ButtonEvent::Increment;
        }
        if btn_dn.is_low() {
            defmt::debug!("BTN_DN pressed");
            btn_dn.wait_for_high().await;
            return ButtonEvent::Decrement;
        }
        // Ghost edge — loop and wait again.
    }
}
