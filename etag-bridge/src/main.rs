//! # etag-bridge
//!
//! A Tokio-based server that bridges **etag BLE inventory tags** to the
//! **[Grocy](https://grocy.info/) ERP** REST API.
//!
//! Designed to run as a `systemd` service on a Raspberry Pi (or any
//! always-on Linux machine with Bluetooth).
//!
//! ## Architecture
//!
//! ```text
//!  ┌────────────────────────────────────────────────────────────────────┐
//!  │                         etag-bridge                                │
//!  │                                                                    │
//!  │   ┌──────────────┐   BLE events   ┌───────────────────────────┐  │
//!  │   │  BLE scanner │ ─────────────► │  DeviceRegistry           │  │
//!  │   │  (btleplug)  │                │  (addr → TagInfo)         │  │
//!  │   └──────┬───────┘                │  • grocycode / product_id │  │
//!  │          │ GATT notifications     │  • stock count            │  │
//!  │          ▼                        │  • battery %              │  │
//!  │   ┌─────────────┐                 │  • RSSI / last seen       │  │
//!  │   │ GrocyClient │ ─── HTTP ──►    └───────────────────────────┘  │
//!  │   │  (reqwest)  │    Grocy ERP                                    │
//!  │   └─────────────┘                                                 │
//!  │          │                                                         │
//!  │   ┌──────┴─────────┐                                              │
//!  │   │ stats_reporter │  (periodic log summary of all devices)       │
//!  │   └────────────────┘                                              │
//!  └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Quick start
//!
//! ```bash
//! export GROCY_URL=http://grocy.local
//! export GROCY_API_KEY=your_api_key_here
//! cargo run --release -p etag-bridge
//! ```
//!
//! Set `RUST_LOG=etag_bridge=debug` for verbose output.
//!
//! ## Configuration
//!
//! See [`config::Config`] for all available environment variables.

use std::sync::Arc;

use anyhow::Result;
use tokio::signal;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

mod ble;
mod config;
mod device;
mod grocy;
mod mesh;

pub use config::{Config, TransportMode};
pub use device::DeviceRegistry;
pub use grocy::GrocyClient;

// ─────────────────────────────────────────────────────────────────────────────
// AppState — shared across all async tasks
// ─────────────────────────────────────────────────────────────────────────────

/// Shared application state.
///
/// Wrapped in an [`Arc`] so it can be cloned cheaply across Tokio tasks.
pub struct AppState {
    /// Runtime configuration (loaded from environment variables at startup).
    pub config: Config,
    /// Thread-safe registry of all discovered etag devices and their live stats.
    pub registry: DeviceRegistry,
    /// HTTP client for the Grocy REST API.
    pub grocy: GrocyClient,
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // ── Structured logging / tracing ──────────────────────────────────────────
    //
    // Default filter: etag_bridge=debug, everything else=info.
    // Override at runtime with: RUST_LOG=etag_bridge=trace,btleplug=debug
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("etag_bridge=debug,btleplug=info,reqwest=warn")),
        )
        .with_target(true)
        .with_line_number(false)
        .compact()
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "etag-bridge starting");

    // ── Configuration ─────────────────────────────────────────────────────────
    let config = Config::from_env()?;
    info!(
        grocy_url          = %config.grocy_url,
        scan_duration_secs = config.scan_duration_secs,
        scan_interval_secs = config.scan_interval_secs,
        stats_interval_secs = config.stats_interval_secs,
        transport_mode = ?config.transport_mode,
        "Configuration loaded"
    );

    // ── Shared application state ──────────────────────────────────────────────
    let state = Arc::new(AppState {
        grocy: GrocyClient::new(&config.grocy_url, &config.grocy_api_key),
        registry: DeviceRegistry::new(),
        config,
    });

    // ── BLE task ──────────────────────────────────────────────────────────────
    let transport_state = Arc::clone(&state);
    let transport_mode = state.config.transport_mode;
    let transport_handle = tokio::spawn(async move {
        let result = match transport_mode {
            TransportMode::Gatt => ble::run(transport_state).await,
            TransportMode::Mesh => mesh::run(transport_state).await,
        };

        if let Err(e) = result {
            error!(?e, ?transport_mode, "Transport task exited with error");
        }
    });

    // ── Graceful shutdown ─────────────────────────────────────────────────────
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received Ctrl-C — shutting down");
        }
        _ = transport_handle => {
            info!(?transport_mode, "Transport task completed");
        }
    }

    Ok(())
}
