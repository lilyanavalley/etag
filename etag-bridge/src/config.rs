//! Runtime configuration for the etag-bridge server.
//!
//! All values are loaded from environment variables so that the service can be
//! configured without recompiling — ideal for a Raspberry Pi `systemd` unit.
//!
//! # Required variables
//!
//! | Variable        | Description                                      |
//! |-----------------|--------------------------------------------------|
//! | `GROCY_URL`     | Base URL of the Grocy instance (no trailing `/`) |
//! | `GROCY_API_KEY` | REST API key — generate in *Settings → API Keys* |
//!
//! # Optional variables (all have defaults)
//!
//! | Variable              | Default | Description                                  |
//! |-----------------------|---------|----------------------------------------------|
//! | `SCAN_DURATION_SECS`  | `30`    | Seconds to run each BLE scan pass            |
//! | `SCAN_INTERVAL_SECS`  | `5`     | Seconds to pause between scan passes         |
//! | `DEVICE_TIMEOUT_SECS` | `300`   | Seconds before a device is considered "away" |
//! | `STATS_INTERVAL_SECS` | `60`    | Seconds between device-stats log summaries   |
//! | `TRANSPORT_MODE`      | `gatt`  | `gatt` or `mesh` runtime mode                |
//! | `MESH_POLL_INTERVAL_SECS` | `5` | Mesh heartbeat interval in mesh mode         |
//! | `MESH_INGEST_BIND_ADDR` | `127.0.0.1:9478` | UDP bind address for mesh ingest |
//!
//! # Example `.env` / systemd `EnvironmentFile`
//!
//! ```ini
//! GROCY_URL=http://grocy.local
//! GROCY_API_KEY=your_api_key_here
//! SCAN_DURATION_SECS=30
//! SCAN_INTERVAL_SECS=5
//! DEVICE_TIMEOUT_SECS=300
//! STATS_INTERVAL_SECS=60
//! ```

use anyhow::{Context, Result};

/// Bridge runtime transport mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Current BLE GATT central behavior.
    Gatt,
    /// Phase-0 mesh foundation runtime.
    Mesh,
}

/// Runtime configuration for the etag-bridge server.
#[derive(Debug, Clone)]
pub struct Config {
    /// Base URL of the Grocy instance (e.g. `http://grocy.local`).
    ///
    /// No trailing slash required; the client appends API paths automatically.
    pub grocy_url: String,

    /// Grocy REST API key — generate one in *Settings → API Keys*.
    pub grocy_api_key: String,

    /// How long (seconds) to run each BLE scan pass before pausing.
    ///
    /// Longer values increase the chance of hearing from infrequently advertising
    /// devices.  Default: **30**.
    pub scan_duration_secs: u64,

    /// Seconds to pause between scan passes (0 = scan continuously).
    ///
    /// A short pause prevents the BLE adapter from overheating on busy
    /// environments.  Default: **5**.
    pub scan_interval_secs: u64,

    /// Seconds without a BLE advertisement before a device is logged as "away".
    ///
    /// Only used by the stats reporter; it does not cause disconnection.
    /// Default: **300** (5 minutes).
    pub device_timeout_secs: u64,

    /// How often (seconds) to emit a device-stats summary to the log.
    ///
    /// Set to a large value (e.g. `86400`) to reduce noise.  Default: **60**.
    pub stats_interval_secs: u64,

    /// Runtime transport mode (`gatt` or `mesh`).
    pub transport_mode: TransportMode,

    /// Heartbeat interval used by mesh runtime mode.
    pub mesh_poll_interval_secs: u64,

    /// UDP socket bind address for mesh ingest packets in mesh mode.
    pub mesh_ingest_bind_addr: String,
}

impl Config {
    /// Load configuration from environment variables.
    ///
    /// Returns an error if a required variable is missing or if any optional
    /// variable cannot be parsed as an unsigned integer.
    pub fn from_env() -> Result<Self> {
        let grocy_url =
            std::env::var("GROCY_URL").context("GROCY_URL environment variable not set")?;

        let grocy_api_key =
            std::env::var("GROCY_API_KEY").context("GROCY_API_KEY environment variable not set")?;

        Ok(Self {
            grocy_url,
            grocy_api_key,
            scan_duration_secs: env_u64("SCAN_DURATION_SECS", 30)?,
            scan_interval_secs: env_u64("SCAN_INTERVAL_SECS", 5)?,
            device_timeout_secs: env_u64("DEVICE_TIMEOUT_SECS", 300)?,
            stats_interval_secs: env_u64("STATS_INTERVAL_SECS", 60)?,
            transport_mode: env_transport_mode("TRANSPORT_MODE", TransportMode::Gatt)?,
            mesh_poll_interval_secs: env_u64("MESH_POLL_INTERVAL_SECS", 5)?,
            mesh_ingest_bind_addr: std::env::var("MESH_INGEST_BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:9478".to_owned()),
        })
    }
}

/// Read an environment variable as `u64`, returning `default` if unset.
fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(s) => s
            .parse::<u64>()
            .with_context(|| format!("{name} must be a non-negative integer")),
        Err(_) => Ok(default),
    }
}

/// Read an environment variable as [`TransportMode`], returning `default` if unset.
fn env_transport_mode(name: &str, default: TransportMode) -> Result<TransportMode> {
    match std::env::var(name) {
        Ok(s) => match s.trim().to_ascii_lowercase().as_str() {
            "gatt" => Ok(TransportMode::Gatt),
            "mesh" => Ok(TransportMode::Mesh),
            _ => anyhow::bail!("{name} must be one of: gatt, mesh (got: {})", s),
        },
        Err(_) => Ok(default),
    }
}
