//! etag-gui — Tauri application library.
//!
//! This crate wires a Tauri window to the etag-bridge gRPC server.
//! Each Tauri command creates a short-lived gRPC channel to the bridge,
//! performs the requested operation, and returns a JSON-serialisable DTO to
//! the frontend.
//!
//! # Commands exposed to the frontend
//!
//! | Command            | gRPC call          | Description                         |
//! |--------------------|--------------------|-------------------------------------|
//! | `list_nodes`       | `ListNodes`        | Snapshot of all known nodes         |
//! | `get_node`         | `GetNode`          | Single node by BLE address          |
//! | `provision_node`   | `ProvisionNode`    | BLE Mesh provisioning request (WIP) |
//! | `link_node_to_grocy` | `LinkNodeToGrocy`| Assign a Grocy item to a node       |
//! | `set_bridge_addr`  | —                  | Update the bridge gRPC address      |
//! | `get_bridge_addr`  | —                  | Read the current bridge gRPC address|

use std::sync::Mutex;

use tonic::transport::Channel;

// ── Generated gRPC client types ───────────────────────────────────────────────

mod proto {
    tonic::include_proto!("etag_bridge");
}

use proto::{
    etag_bridge_client::EtagBridgeClient, GetNodeRequest, LinkNodeRequest, ListNodesRequest,
    NodeStatus, ProvisionNodeRequest, WatchNodesRequest,
};

// ── Shared application state ──────────────────────────────────────────────────

/// Holds the mutable gRPC address so the Settings page can update it at runtime.
pub struct BridgeState {
    /// Base URI of the etag-bridge gRPC server (e.g. `http://127.0.0.1:50051`).
    pub addr: Mutex<String>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            addr: Mutex::new("http://127.0.0.1:50051".to_owned()),
        }
    }
}

// ── Data Transfer Objects (frontend-visible) ──────────────────────────────────

/// JSON-serialisable node summary returned to the frontend.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct NodeInfoDto {
    pub address:        String,
    pub name:           Option<String>,
    pub grocycode:      Option<String>,
    pub product_id:     Option<u32>,
    pub product_name:   Option<String>,
    pub stock_count:    Option<i32>,
    pub battery_pct:    Option<u32>,
    pub rssi:           Option<i32>,
    pub first_seen_unix: i64,
    pub last_seen_unix:  i64,
    pub connected:      bool,
    /// One of: `"active"`, `"away"`, `"provisioning"`, `"unknown"`.
    pub status:         String,
}

impl From<proto::NodeInfo> for NodeInfoDto {
    fn from(n: proto::NodeInfo) -> Self {
        let status = match n.status {
            s if s == NodeStatus::NodeStatusActive as i32 => "active",
            s if s == NodeStatus::NodeStatusAway as i32 => "away",
            s if s == NodeStatus::NodeStatusProvisioning as i32 => "provisioning",
            _ => "unknown",
        }
        .to_owned();

        Self {
            address:         n.address,
            name:            n.name,
            grocycode:       n.grocycode,
            product_id:      n.product_id,
            product_name:    n.product_name,
            stock_count:     n.stock_count,
            battery_pct:     n.battery_pct,
            rssi:            n.rssi,
            first_seen_unix: n.first_seen_unix,
            last_seen_unix:  n.last_seen_unix,
            connected:       n.connected,
            status,
        }
    }
}

/// Result of a provision or link operation.
#[derive(Debug, serde::Serialize)]
pub struct OperationResult {
    pub success: bool,
    pub message: String,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn connect(state: &tauri::State<'_, BridgeState>) -> Result<EtagBridgeClient<Channel>, String> {
    let addr = state.addr.lock().unwrap().clone();
    EtagBridgeClient::connect(addr)
        .await
        .map_err(|e| format!("Cannot connect to bridge: {e}"))
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Return a snapshot of all nodes currently known to the bridge.
#[tauri::command]
pub async fn list_nodes(
    bridge: tauri::State<'_, BridgeState>,
) -> Result<Vec<NodeInfoDto>, String> {
    let mut client = connect(&bridge).await?;
    let resp = client
        .list_nodes(ListNodesRequest {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(resp.into_inner().nodes.into_iter().map(NodeInfoDto::from).collect())
}

/// Return a single node by BLE address.
#[tauri::command]
pub async fn get_node(
    address: String,
    bridge:  tauri::State<'_, BridgeState>,
) -> Result<NodeInfoDto, String> {
    let mut client = connect(&bridge).await?;
    let resp = client
        .get_node(GetNodeRequest { address })
        .await
        .map_err(|e| e.to_string())?;
    Ok(NodeInfoDto::from(resp.into_inner()))
}

/// Send a BLE Mesh provisioning request for the given node address.
///
/// Returns immediately with `success: false` and a WIP message until the
/// mesh branch is merged.
#[tauri::command]
pub async fn provision_node(
    address:          String,
    mesh_network_key: Option<String>,
    bridge:           tauri::State<'_, BridgeState>,
) -> Result<OperationResult, String> {
    let mut client = connect(&bridge).await?;
    let resp = client
        .provision_node(ProvisionNodeRequest { address, mesh_network_key })
        .await
        .map_err(|e| e.to_string())?;
    let r = resp.into_inner();
    Ok(OperationResult { success: r.success, message: r.message })
}

/// Link a node to a Grocy item by writing its grocycode into the bridge registry.
///
/// `grocycode` must match the `grcy-p-<id>` format.
#[tauri::command]
pub async fn link_node_to_grocy(
    address:   String,
    grocycode: String,
    bridge:    tauri::State<'_, BridgeState>,
) -> Result<OperationResult, String> {
    let mut client = connect(&bridge).await?;
    let resp = client
        .link_node_to_grocy(LinkNodeRequest { address, grocycode })
        .await
        .map_err(|e| e.to_string())?;
    let r = resp.into_inner();
    Ok(OperationResult { success: r.success, message: r.message })
}

/// Update the gRPC address used to reach the bridge (persists for this session).
#[tauri::command]
pub fn set_bridge_addr(
    addr:   String,
    bridge: tauri::State<'_, BridgeState>,
) -> Result<(), String> {
    *bridge.addr.lock().unwrap() = addr;
    Ok(())
}

/// Return the currently configured bridge gRPC address.
#[tauri::command]
pub fn get_bridge_addr(bridge: tauri::State<'_, BridgeState>) -> String {
    bridge.addr.lock().unwrap().clone()
}

// ── Tauri application entry point ─────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(BridgeState::default())
        .invoke_handler(tauri::generate_handler![
            list_nodes,
            get_node,
            provision_node,
            link_node_to_grocy,
            set_bridge_addr,
            get_bridge_addr,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
