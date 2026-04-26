# etag — ePaper Inventory Tag for Grocy on nRF52840

An embedded Rust firmware for an **nRF52840**-based ePaper inventory tag that
integrates with the [Grocy](https://grocy.info/) ERP / stock-management
application.

Attach a tag to any storage shelf or container.  Two buttons let you increment
or decrement the in-stock count right at the shelf edge — just like the
ePaper price labels found in modern grocery stores.

---

## Hardware

| Component | Part / Notes |
|-----------|-------------|
| MCU | nRF52840 (e.g. Adafruit Feather nRF52840, Nordic DK, custom PCB) |
| Display | Waveshare 2.9" BW ePaper 296 × 128 px (SSD1680 controller) |
| Buttons | 2 × tactile switch, wired to GND |
| Battery | Single-cell LiPo (3.7 V), voltage-divider to SAADC |

### Pin assignment

| Function | nRF52840 pin |
|----------|-------------|
| SPI SCK  | P0.03 |
| SPI MOSI | P0.04 |
| EPD CS   | P0.05 |
| EPD DC   | P0.06 |
| EPD RST  | P0.07 |
| EPD BUSY | P0.08 |
| BTN_UP   | P0.11 (internal pull-up) |
| BTN_DN   | P0.12 (internal pull-up) |
| VBAT ADC | P0.02 / AIN0 |

> **Tip:** All pin assignments are in [`src/config.rs`](src/config.rs).

---

## Display layout

```
┌──────────────────────────────────────────────────────────────────┐
│  QR code (128 × 128 px)   │  Product name  (top)                │
│                            │                                      │
│  ┌────────────┐            │  Stock:                             │
│  │  QR Code   │            │     42          (large)             │
│  └────────────┘            │                                      │
│                            │  Bat: 87%      (bottom-right)       │
└──────────────────────────────────────────────────────────────────┘
```

The QR code encodes the **Grocycode** (e.g. `grcy-p-42`), the standard
product identifier used by Grocy.

---

## Software architecture

```
src/
  main.rs      — Embassy async entry point, main event loop
  config.rs    — Compile-time hardware constants and pin assignments
  display.rs   — SSD1680 async SPI driver + framebuffer rendering
  qr.rs        — No-std QR-code generator (byte mode, v1-v5, ECC-M)
  buttons.rs   — Debounced button handling
  battery.rs   — LiPo voltage monitoring via SAADC
  grocy.rs     — Grocy data types, Grocycode helpers, BLE GATT skeleton
```

### QR code generator (`src/qr.rs`)

A self-contained, **heap-free** QR code generator:

- Versions 1 – 5 (21 × 21 … 37 × 37 modules)
- Byte mode, ECC level M (≈ 15 % recovery)
- Maximum payload: **84 bytes** (version 5)
- All buffers are stack-allocated; no `alloc` or `Vec` required
- Includes GF(256) arithmetic and Reed-Solomon encoding

### Grocy integration (`src/grocy.rs`)

The tag exposes a custom **BLE GATT service** (UUID `4fa0e000-…`):

| Characteristic | UUID suffix | Properties | Type |
|---------------|------------|-----------|------|
| `grocycode`   | `…e001` | Read | UTF-8, max 20 B |
| `product_name`| `…e002` | Read / Write | UTF-8, max 32 B |
| `stock_count` | `…e003` | Read / Write / Notify | `i32` LE |
| `battery_pct` | `…e004` | Read / Notify | `u8` |

An external BLE central (Raspberry Pi, PC, …) bridges tag ↔ Grocy REST API.
Full BLE support requires the **nrf-softdevice** crate with the S140
SoftDevice — see `src/grocy.rs` for the GATT server definition template.

---

## Building

### Prerequisites

```bash
# Rust nightly toolchain is not required — stable works.
rustup target add thumbv7em-none-eabihf

# Linker helper (provides link.x)
cargo install flip-link       # optional but recommended

# Flash / run via probe-rs
cargo install probe-rs-tools
```

### Build

```bash
cargo build --release
```

The binary lands at `target/thumbv7em-none-eabihf/release/etag`.

### Flash

Connect a J-Link, CMSIS-DAP, or ST-Link debug probe and run:

```bash
cargo run --release
# equivalent to: probe-rs run --chip nRF52840_xxAA <binary>
```

defmt log output is streamed via RTT to your terminal.

---

## Running unit tests (on host)

The QR-code generator and Grocy data-type modules contain host-runnable tests:

```bash
# Tests compile for x86-64 (host) — no hardware needed.
cargo test --target x86_64-unknown-linux-gnu \
           --lib \
           -- --test-output immediate
```

---

## Configuration

Edit [`src/config.rs`](src/config.rs) to change:

- SPI pin assignments
- Button debounce time
- Battery voltage divider ratio
- Stock count limits
- Max product name / Grocycode lengths

---

## Adding BLE (optional)

1. Add `nrf-softdevice` and `nrf-softdevice-s140` to `Cargo.toml`.
2. Flash the S140 SoftDevice hex alongside the application binary.
3. Uncomment the GATT server definition in `src/grocy.rs` and wire it up
   in `src/main.rs` by spawning the softdevice task.
4. Update `memory.x` to start FLASH at `0x00027000` (after S140).

See the [nrf-softdevice README](https://github.com/embassy-rs/nrf-softdevice)
for detailed integration instructions.

---

## Future work

- [ ] BLE GATT server (nrf-softdevice)
- [ ] Low-power sleep between refreshes (System OFF / DCDC)
- [ ] Partial display refresh for faster stock-count updates
- [ ] Provisioning mode (BLE scan for Grocy server, write product config)
- [ ] Support additional Grocy objects (locations, chores)
- [ ] Port to nRF5340 (dual-core) for concurrent BLE + display

---

## License

MIT — see [LICENSE](LICENSE).
