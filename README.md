# etag — ePaper Inventory Tag + Grocy Bridge

A **Cargo Workspace** containing three packages:

| Package | Description |
|---------|-------------|
| [`firmware/`](firmware/) | nRF52840 embedded firmware — ePaper tag with BLE GATT service |
| [`etag-bridge/`](etag-bridge/) | Raspberry Pi server — BLE central that bridges etag ↔ Grocy ERP |
| [`etag-gui/`](etag-gui/) | Graphical interface to `etag-bridge`, that runs anywhere |

---

## etag firmware — ePaper Inventory Tag for Grocy on nRF52840

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
  mesh.rs      — BLE Mesh foundation message types and payload codecs (Phase-0)
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

### Build (firmware)

Firmware must be built from the `firmware/` subdirectory so that the
ARM-specific Cargo config (`firmware/.cargo/config.toml`) is applied automatically:

```bash
cd firmware && cargo build --release
```

The binary lands at `firmware/target/thumbv7em-none-eabihf/release/etag`.

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
# Run from the firmware/ subdirectory (no hardware needed).
cd firmware && cargo test --target x86_64-unknown-linux-gnu --lib
```

---

## Configuration (firmware)

Edit [`firmware/src/config.rs`](firmware/src/config.rs) to change:

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

## BLE Mesh planning

For a deployment plan covering 30+ eTag nodes, optional repeaters, and a central
Raspberry Pi bridge to Grocy, see:

- [`docs/ble-mesh-plan.md`](docs/ble-mesh-plan.md)

---

## etag-bridge — Raspberry Pi BLE ↔ Grocy Bridge

The `etag-bridge` package is a Tokio-based server that:

1. **Scans** for etag devices advertising the custom BLE GATT service.
2. **Connects** via GATT and reads `grocycode`, `product_name`, `stock_count`, and `battery_pct`.
3. **Subscribes** to `stock_count` and `battery_pct` notifications.
4. **Forwards** stock changes to the Grocy REST API (`/api/stock/products/{id}/add` or `/consume`).
5. **Tracks** per-device stats — RSSI signal quality, battery level, and last-seen time — and logs a summary at a configurable interval.

### Requirements

- Raspberry Pi (or any always-on Linux machine) with Bluetooth LE
- BlueZ (`bluez` package) — the Linux BLE stack
- `libdbus-1-dev` and `pkg-config` (required to compile btleplug)

```bash
sudo apt install bluetooth bluez libdbus-1-dev pkg-config
```

### Build

```bash
cargo build -p etag-bridge --release
```

The binary lands at `target/release/etag-bridge`.

### Configuration (environment variables)

| Variable              | Required | Default | Description                                     |
|-----------------------|----------|---------|-------------------------------------------------|
| `GROCY_URL`           | ✅        | —       | Base URL of the Grocy instance                  |
| `GROCY_API_KEY`       | ✅        | —       | API key from *Settings → API Keys*              |
| `SCAN_DURATION_SECS`  | ❌        | `30`    | Seconds per BLE scan pass                       |
| `SCAN_INTERVAL_SECS`  | ❌        | `5`     | Pause between scan passes                       |
| `DEVICE_TIMEOUT_SECS` | ❌        | `300`   | Seconds before a device is considered "away"    |
| `STATS_INTERVAL_SECS` | ❌        | `60`    | Seconds between device-stats log summaries      |
| `TRANSPORT_MODE`      | ❌        | `gatt`  | Bridge transport mode: `gatt` or `mesh`         |
| `MESH_POLL_INTERVAL_SECS` | ❌    | `5`     | Heartbeat interval when `TRANSPORT_MODE=mesh`   |
| `MESH_INGEST_BIND_ADDR` | ❌      | `127.0.0.1:9478` | UDP bind address for mesh ingest packets |

### Quick start

```bash
export GROCY_URL=http://grocy.local
export GROCY_API_KEY=your_api_key_here
RUST_LOG=etag_bridge=debug ./target/release/etag-bridge
```

### Mesh mode (Phase-0 foundation)

```bash
export GROCY_URL=http://grocy.local
export GROCY_API_KEY=your_api_key_here
export TRANSPORT_MODE=mesh
RUST_LOG=etag_bridge=info ./target/release/etag-bridge
```

`TRANSPORT_MODE=mesh` currently enables the mesh foundation runtime (idempotent
mesh event processing with UDP ingest plus runtime heartbeat, while preserving
the existing `gatt` mode as default production behavior.

Mesh ingest packet wire format (little-endian, 16 bytes):

- `node_unicast: u16`
- `revision: u32`
- `product_id: u32`
- `op: u8` (`0=delta`, `1=absolute`)
- `stock_value: i32`
- `battery_pct: u8` (`0..=100`)

### Running as a systemd service

Create `/etc/systemd/system/etag-bridge.service`:

```ini
[Unit]
Description=etag BLE ↔ Grocy bridge
After=network.target bluetooth.target

[Service]
ExecStart=/usr/local/bin/etag-bridge
EnvironmentFile=/etc/etag-bridge.env
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Create `/etc/etag-bridge.env`:

```ini
GROCY_URL=http://grocy.local
GROCY_API_KEY=your_api_key_here
```

Enable and start:

```bash
sudo systemctl enable --now etag-bridge
sudo journalctl -u etag-bridge -f
```

### Architecture

```
main()
├── init tracing (RUST_LOG-driven, compact format)
├── load Config from environment
├── create shared AppState (Config + DeviceRegistry + GrocyClient)
└── tokio::spawn  ble::run(state)
     ├── init BLE adapter (btleplug / BlueZ)
     ├── spawn stats_reporter task
     └── scan loop
          ├── start_scan (filter: etag service UUID)
          ├── event loop (scan_duration seconds)
          │    DeviceDiscovered / DeviceUpdated
          │      → upsert device in DeviceRegistry (name, RSSI, last_seen)
          │      → if new: spawn manage_device task
          │    DeviceConnected / DeviceDisconnected
          │      → update connected flag
          ├── stop_scan
          └── pause scan_interval seconds, then repeat

manage_device(peripheral)    ← one task per device
├── connect()
├── discover_services()
├── read grocycode, product_name, stock_count, battery_pct
├── subscribe to stock_count + battery_pct notifications
└── notification loop
     stock_count change → GrocyClient::sync_stock()
     battery_pct change → DeviceRegistry::update_battery()
                          low-battery warning if ≤ 10 %
```

### Device stats

The bridge tracks the following information for every etag it has ever seen:

| Stat           | Description                                    |
|----------------|------------------------------------------------|
| `address`      | BLE MAC address                               |
| `name`         | Device name from advertisement                |
| `grocycode`    | Grocycode (e.g. `"grcy-p-42"`)               |
| `product_name` | Human-readable product label                  |
| `stock_count`  | Current on-shelf quantity                     |
| `battery_pct`  | Battery level 0–100 %                        |
| `rssi`         | Signal strength in dBm (less negative = closer) |
| `first_seen`   | UTC timestamp of first advertisement          |
| `last_seen`    | UTC timestamp of most recent BLE event        |
| `connected`    | Whether a GATT connection is currently open   |

A summary is logged every `STATS_INTERVAL_SECS` seconds:

```
INFO etag_bridge::ble: ─── etag device summary (2 device(s)) ───
INFO etag_bridge::ble: ● AA:BB:CC:DD:EE:FF | Oat Milk | stock=5 | bat=87% | rssi=-62 dBm | last seen 3s ago
INFO etag_bridge::ble: ○ 11:22:33:44:55:66 | Coffee Beans | stock=2 | bat=34% | rssi=-78 dBm | last seen 142s ago
```

---

## License

GNU General Public License, version 3 or later — see [LICENSE](LICENSE).
