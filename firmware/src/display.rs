//! SSD1680 ePaper display driver for the Waveshare 2.9" Black/White display.
//!
//! # Hardware characteristics
//! - Resolution : 296 × 128 pixels, 1 bit per pixel (0 = black, 1 = white)
//! - Controller : SSD1680 (used in Waveshare GDEW029T5 / 2.9" V2 module)
//! - Interface  : 4-wire SPI (MOSI, SCK, CS, DC) + RST + BUSY GPIO signals
//! - Refresh    : full refresh takes ~2 s; partial refresh ~0.3 s but leaves ghosting
//!
//! # Display layout (this project)
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │  QR code area (128 × 128 px)     │  Product info area  (168 × 128 px)   │
//! │                                  │  Product name  (top)                  │
//! │  ┌──────────────────┐            │  Stock count   (large, centre)        │
//! │  │  QR Code         │            │  Battery level (small, bottom-right)  │
//! │  └──────────────────┘            │                                       │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Memory
//! The full 296 × 128 framebuffer (`4 736 bytes`) is kept as a `static mut`
//! array in the caller; this module operates on a mutable `&mut [u8]` slice.

use embassy_nrf::{
    gpio::{Input, Output},
    spim::Spim,
};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, ascii::FONT_6X13, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use etag::config::{DISP_BUF_BYTES, DISP_HEIGHT_PX, DISP_WIDTH_PX, QR_AREA_PX};
use etag::qr::QrCode;

// ─────────────────────────────────────────────────────────────────────────────
// SSD1680 command constants
// ─────────────────────────────────────────────────────────────────────────────

const CMD_DRIVER_OUTPUT_CTRL: u8 = 0x01;
const CMD_DATA_ENTRY_MODE: u8 = 0x11;
const CMD_SW_RESET: u8 = 0x12;
const CMD_TEMP_SENSOR_CTRL: u8 = 0x18;
const CMD_DISPLAY_UPDATE_CTRL2: u8 = 0x22;
const CMD_WRITE_RAM_BW: u8 = 0x24;
const CMD_MASTER_ACTIVATION: u8 = 0x20;
const CMD_BORDER_WAVEFORM: u8 = 0x3C;
const CMD_SET_RAM_X_ADDR: u8 = 0x44;
const CMD_SET_RAM_Y_ADDR: u8 = 0x45;
const CMD_SET_RAM_X_COUNTER: u8 = 0x4E;
const CMD_SET_RAM_Y_COUNTER: u8 = 0x4F;

/// Display width in bytes (1 bit per pixel, 296 / 8 = 37 bytes per row).
const BYTES_PER_ROW: usize = DISP_WIDTH_PX / 8;

// ─────────────────────────────────────────────────────────────────────────────
// EpdDisplay driver
// ─────────────────────────────────────────────────────────────────────────────

/// Driver for the SSD1680-based 2.9" BW ePaper display.
///
/// All SPI operations are `async` (backed by `embassy-nrf` SPIM).  Call
/// [`EpdDisplay::init`] once after power-on, then render to the framebuffer
/// with the helper methods and flush via [`EpdDisplay::full_update`].
pub struct EpdDisplay<'d, T>
where
    T: embassy_nrf::spim::Instance,
{
    spi: Spim<'d, T>,
    dc: Output<'d>,
    cs: Output<'d>,
    rst: Output<'d>,
    busy: Input<'d>,
    /// Framebuffer: 1-bit packed, 0 = black, 1 = white, MSB-first.
    framebuf: [u8; DISP_BUF_BYTES],
}

impl<'d, T> EpdDisplay<'d, T>
where
    T: embassy_nrf::spim::Instance,
{
    /// Create a new display driver instance.
    pub fn new(
        spi: Spim<'d, T>,
        dc: Output<'d>,
        cs: Output<'d>,
        rst: Output<'d>,
        busy: Input<'d>,
    ) -> Self {
        Self {
            spi,
            dc,
            cs,
            rst,
            busy,
            framebuf: [0xFF; DISP_BUF_BYTES], // start fully white
        }
    }

    // ── Hardware control ──────────────────────────────────────────────────────

    /// Perform a hardware reset: pull RST low for 10 ms, then release.
    pub async fn hw_reset(&mut self) {
        self.rst.set_low();
        Timer::after(Duration::from_millis(10)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(10)).await;
    }

    /// Block until the BUSY pin goes low (display is ready).
    pub async fn wait_idle(&mut self) {
        while self.busy.is_high() {
            Timer::after(Duration::from_millis(10)).await;
        }
    }

    /// Send one command byte (DC low) to the display.
    async fn send_cmd(&mut self, cmd: u8) {
        self.dc.set_low();
        self.cs.set_low();
        self.spi.write(&[cmd]).await.ok();
        self.cs.set_high();
    }

    /// Send data bytes (DC high) to the display.
    async fn send_data(&mut self, data: &[u8]) {
        self.dc.set_high();
        self.cs.set_low();
        self.spi.write(data).await.ok();
        self.cs.set_high();
    }

    // ── Initialisation ────────────────────────────────────────────────────────

    /// Initialise the display for full-screen black/white operation.
    ///
    /// Must be called once after power-on before any rendering.
    pub async fn init(&mut self) {
        self.hw_reset().await;
        self.wait_idle().await;

        // Software reset
        self.send_cmd(CMD_SW_RESET).await;
        self.wait_idle().await;

        // Driver output control: 296 lines (0x127), gate scanning direction 0x00
        self.send_cmd(CMD_DRIVER_OUTPUT_CTRL).await;
        self.send_data(&[
            ((DISP_HEIGHT_PX - 1) & 0xFF) as u8,
            (((DISP_HEIGHT_PX - 1) >> 8) & 0x01) as u8,
            0x00,
        ])
        .await;

        // Data entry mode: X-increment, Y-increment (normal scan)
        self.send_cmd(CMD_DATA_ENTRY_MODE).await;
        self.send_data(&[0x03]).await;

        // Set RAM X address range: 0 to (width/8 - 1) = 0 to 36
        self.send_cmd(CMD_SET_RAM_X_ADDR).await;
        self.send_data(&[0x00, (BYTES_PER_ROW - 1) as u8]).await;

        // Set RAM Y address range: 0 to (height - 1)
        self.send_cmd(CMD_SET_RAM_Y_ADDR).await;
        self.send_data(&[
            0x00,
            0x00,
            ((DISP_HEIGHT_PX - 1) & 0xFF) as u8,
            (((DISP_HEIGHT_PX - 1) >> 8) & 0x01) as u8,
        ])
        .await;

        // Border waveform: follow LUT for VBD
        self.send_cmd(CMD_BORDER_WAVEFORM).await;
        self.send_data(&[0x01]).await;

        // Use internal temperature sensor
        self.send_cmd(CMD_TEMP_SENSOR_CTRL).await;
        self.send_data(&[0x80]).await;

        // Reset RAM address counters
        self.set_ram_address_counter(0, 0).await;

        defmt::info!("EPD: initialised");
    }

    /// Set the RAM X/Y address counters before a write.
    async fn set_ram_address_counter(&mut self, x_byte: u8, y: u16) {
        self.send_cmd(CMD_SET_RAM_X_COUNTER).await;
        self.send_data(&[x_byte]).await;

        self.send_cmd(CMD_SET_RAM_Y_COUNTER).await;
        self.send_data(&[(y & 0xFF) as u8, ((y >> 8) & 0x01) as u8])
            .await;
    }

    // ── Framebuffer helpers ───────────────────────────────────────────────────

    /// Set one pixel in the framebuffer.
    ///
    /// - `dark = true`  → black pixel (bit = 0)
    /// - `dark = false` → white pixel (bit = 1)
    #[inline]
    pub fn set_pixel(&mut self, x: usize, y: usize, dark: bool) {
        if x >= DISP_WIDTH_PX || y >= DISP_HEIGHT_PX {
            return;
        }
        let byte_idx = y * BYTES_PER_ROW + x / 8;
        let bit = 7 - (x % 8); // bit 7 = leftmost pixel in byte (MSB = leftmost)
        if dark {
            self.framebuf[byte_idx] &= !(1 << bit);
        } else {
            self.framebuf[byte_idx] |= 1 << bit;
        }
    }

    /// Fill the entire framebuffer with white (`0xFF`).
    #[inline]
    pub fn clear_white(&mut self) {
        self.framebuf.fill(0xFF);
    }

    /// Fill the entire framebuffer with black (`0x00`).
    #[allow(dead_code)]
    #[inline]
    pub fn clear_black(&mut self) {
        self.framebuf.fill(0x00);
    }

    // ── High-level rendering ──────────────────────────────────────────────────

    /// Render the complete inventory-tag layout into the framebuffer.
    ///
    /// - `qr`          – pre-generated QR code (uses the left `QR_AREA_PX` columns)
    /// - `product_name` – short product name shown top-right
    /// - `stock`        – current stock count shown large in the centre-right
    /// - `battery_pct`  – battery percentage (0-100) shown small bottom-right
    pub fn render(
        &mut self,
        qr: &QrCode,
        product_name: &str,
        stock: i32,
        battery_pct: u8,
    ) {
        self.clear_white();
        self.draw_qr(qr);
        self.draw_info(product_name, stock, battery_pct);
    }

    /// Blit a QR code into the left QR_AREA_PX × QR_AREA_PX section.
    fn draw_qr(&mut self, qr: &QrCode) {
        let qr_size = qr.size();
        // Choose the largest integer scale that fits inside QR_AREA_PX.
        let scale = (QR_AREA_PX / qr_size).max(1);
        let total = scale * qr_size;
        let offset_x = (QR_AREA_PX - total) / 2;
        let offset_y = (DISP_HEIGHT_PX - total) / 2;

        for row in 0..qr_size {
            for col in 0..qr_size {
                let dark = qr.module(row, col);
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = offset_x + col * scale + sx;
                        let py = offset_y + row * scale + sy;
                        self.set_pixel(px, py, dark);
                    }
                }
            }
        }
    }

    /// Render product name, stock count, and battery indicator on the right side.
    fn draw_info(&mut self, product_name: &str, stock: i32, battery_pct: u8) {
        // We use embedded-graphics by implementing DrawTarget on a temporary
        // framebuffer wrapper.  The wrapper maps pixel draws into `self.framebuf`.
        let mut fb = FramebufDrawTarget {
            buf: &mut self.framebuf,
        };
        let display_size = Size::new(DISP_WIDTH_PX as u32, DISP_HEIGHT_PX as u32);

        // ── Product name (top-right, small font) ──────────────────────────────
        let name_style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
        let name_x = (QR_AREA_PX + 4) as i32;
        // Truncate to 24 *characters* (not bytes) to avoid splitting a multi-byte
        // UTF-8 sequence.
        let name_end = product_name
            .char_indices()
            .nth(24)
            .map(|(i, _)| i)
            .unwrap_or(product_name.len());
        let _ = Text::with_alignment(
            &product_name[..name_end],
            Point::new(name_x, 14),
            name_style,
            Alignment::Left,
        )
        .draw(&mut fb);

        // ── Stock count (centre-right, large font) ────────────────────────────
        let mut count_str = heapless::String::<8>::new();
        let _ = core::fmt::write(&mut count_str, format_args!("{}", stock));

        let count_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        let info_centre_x = (QR_AREA_PX as u32 + display_size.width) / 2;
        let _ = Text::with_alignment(
            count_str.as_str(),
            Point::new(info_centre_x as i32, (DISP_HEIGHT_PX / 2 + 10) as i32),
            count_style,
            Alignment::Center,
        )
        .draw(&mut fb);

        // ── "Stock:" label ────────────────────────────────────────────────────
        let label_style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
        let _ = Text::with_alignment(
            "Stock:",
            Point::new(info_centre_x as i32, (DISP_HEIGHT_PX / 2 - 8) as i32),
            label_style,
            Alignment::Center,
        )
        .draw(&mut fb);

        // ── Divider line between QR area and text area ────────────────────────
        let line_style = PrimitiveStyleBuilder::new()
            .stroke_color(BinaryColor::Off)
            .stroke_width(1)
            .build();
        let _ = embedded_graphics::primitives::Line::new(
            Point::new(QR_AREA_PX as i32, 0),
            Point::new(QR_AREA_PX as i32, DISP_HEIGHT_PX as i32 - 1),
        )
        .into_styled(line_style)
        .draw(&mut fb);

        // ── Battery indicator (bottom-right, small) ───────────────────────────
        let mut bat_str = heapless::String::<8>::new();
        let _ = core::fmt::write(&mut bat_str, format_args!("Bat:{}%", battery_pct));
        let bat_style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
        let _ = Text::with_alignment(
            bat_str.as_str(),
            Point::new(
                (DISP_WIDTH_PX - 4) as i32,
                (DISP_HEIGHT_PX - 4) as i32,
            ),
            bat_style,
            Alignment::Right,
        )
        .draw(&mut fb);

        // ── Border rectangle around the entire display ────────────────────────
        let border_style = PrimitiveStyleBuilder::new()
            .stroke_color(BinaryColor::Off)
            .stroke_width(1)
            .build();
        let _ = Rectangle::new(Point::zero(), display_size)
            .into_styled(border_style)
            .draw(&mut fb);
    }

    // ── Display update ────────────────────────────────────────────────────────

    /// Flush the framebuffer to the display and trigger a full refresh.
    ///
    /// This function blocks (async-awaits) until the display finishes refreshing
    /// (~2 seconds for a full update).
    pub async fn full_update(&mut self) {
        // Reset address counters to start of RAM.
        self.set_ram_address_counter(0, 0).await;

        // Write B/W RAM.
        self.send_cmd(CMD_WRITE_RAM_BW).await;
        // We must send the framebuffer row-by-row because we cannot borrow
        // `self.framebuf` and call `send_data` (which also borrows `self`) at
        // the same time.  Instead, send the entire buffer in one call by splitting
        // the borrow.
        self.dc.set_high();
        self.cs.set_low();
        // SAFETY: We hold `&mut self` so no other code can access `framebuf`.
        let fb_ptr = self.framebuf.as_ptr();
        let fb_len = self.framebuf.len();
        let fb_slice = unsafe { core::slice::from_raw_parts(fb_ptr, fb_len) };
        self.spi.write(fb_slice).await.ok();
        self.cs.set_high();

        // Trigger full update sequence.
        self.send_cmd(CMD_DISPLAY_UPDATE_CTRL2).await;
        self.send_data(&[0xF7]).await; // full update LUT
        self.send_cmd(CMD_MASTER_ACTIVATION).await;

        // Wait for refresh to complete.
        self.wait_idle().await;
        defmt::info!("EPD: full update done");
    }

    /// Put the display into deep sleep (lowest power mode, ~μA).
    ///
    /// A hardware reset followed by [`EpdDisplay::init`] is required to wake it.
    pub async fn deep_sleep(&mut self) {
        self.send_cmd(0x10).await; // Deep Sleep Mode 1
        self.send_data(&[0x01]).await;
        defmt::info!("EPD: deep sleep");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// embedded-graphics DrawTarget wrapper for the framebuffer
// ─────────────────────────────────────────────────────────────────────────────

/// A thin `DrawTarget` wrapper that renders into the flat framebuffer array.
struct FramebufDrawTarget<'a> {
    buf: &'a mut [u8; DISP_BUF_BYTES],
}

impl<'a> DrawTarget for FramebufDrawTarget<'a> {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let x = point.x as usize;
            let y = point.y as usize;
            if x >= DISP_WIDTH_PX || y >= DISP_HEIGHT_PX {
                continue;
            }
            let byte_idx = y * BYTES_PER_ROW + x / 8;
            let bit = 7 - (x % 8); // bit 7 = leftmost pixel in byte
            if color == BinaryColor::Off {
                // BinaryColor::Off = "background" / white for this B/W display
                // (inverted from the embedded-graphics convention where Off = black
                //  because the SSD1680 stores white as 1 and black as 0)
                self.buf[byte_idx] |= 1 << bit;
            } else {
                // BinaryColor::On = "foreground" / dark/black pixel
                self.buf[byte_idx] &= !(1 << bit);
            }
        }
        Ok(())
    }
}

impl<'a> OriginDimensions for FramebufDrawTarget<'a> {
    fn size(&self) -> Size {
        Size::new(DISP_WIDTH_PX as u32, DISP_HEIGHT_PX as u32)
    }
}
