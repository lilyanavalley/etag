//! gRPC server for the etag-bridge.
//!
//! Implements the [`EtagBridge`] service (generated from
//! `proto/etag_bridge.proto`) and exposes it as a Tonic server that the GUI
//! client (or any other gRPC consumer) can connect to.
//!
//! # Architecture
//!
//! ```text
//!  etag-gui (or any gRPC client)
//!       │  gRPC / HTTP-2
//!       ▼
//!  EtagBridgeService              ← this module
//!       │  Arc<AppState>
//!       ├── DeviceRegistry.get_all()  → ListNodes / WatchNodes
//!       ├── DeviceRegistry.get()      → GetNode
//!       ├── DeviceRegistry.update_grocycode() → LinkNodeToGrocy
//!       └── (stub)                    → ProvisionNode (BLE Mesh WIP)
//! ```
//!
//! # Starting the server
//!
//! Call [`serve`] from the Tokio main task after the `AppState` is ready:
//!
//! ```rust,ignore
//! grpc::serve(Arc::clone(&state)).await?;
//! ```

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::time;
use tokio_stream::{wrappers::IntervalStream, Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use crate::{device::parse_product_id, AppState};

// ── Generated proto types ─────────────────────────────────────────────────────

pub mod proto {
    tonic::include_proto!("etag_bridge");
}

use proto::{
    etag_bridge_server::{EtagBridge, EtagBridgeServer},
    GetNodeRequest, LinkNodeRequest, LinkNodeResponse, ListNodesRequest, ListNodesResponse,
    NodeInfo, NodeStatus, ProvisionNodeRequest, ProvisionNodeResponse, WatchNodesRequest,
};

// ── TagInfo → NodeInfo conversion ────────────────────────────────────────────

fn tag_to_proto(tag: &crate::device::TagInfo, timeout_secs: i64) -> NodeInfo {
    let now = chrono::Utc::now();
    let secs_since = (now - tag.last_seen).num_seconds();

    let status = if tag.connected {
        NodeStatus::NodeStatusActive as i32
    } else if secs_since > timeout_secs {
        NodeStatus::NodeStatusAway as i32
    } else {
        NodeStatus::NodeStatusActive as i32
    };

    NodeInfo {
        address:        tag.address.clone(),
        name:           tag.name.clone(),
        grocycode:      tag.grocycode.clone(),
        product_id:     tag.product_id,
        product_name:   tag.product_name.clone(),
        stock_count:    tag.stock_count,
        battery_pct:    tag.battery_pct.map(|b| b as u32),
        rssi:           tag.rssi.map(|r| r as i32),
        first_seen_unix: tag.first_seen.timestamp(),
        last_seen_unix:  tag.last_seen.timestamp(),
        connected:      tag.connected,
        status,
    }
}

// ── Service implementation ────────────────────────────────────────────────────

pub struct EtagBridgeService {
    state: Arc<AppState>,
}

impl EtagBridgeService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    fn timeout_secs(&self) -> i64 {
        self.state.config.device_timeout_secs as i64
    }
}

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl EtagBridge for EtagBridgeService {
    // ── ListNodes ─────────────────────────────────────────────────────────────

    async fn list_nodes(
        &self,
        _req: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let timeout = self.timeout_secs();
        let nodes = self
            .state
            .registry
            .get_all()
            .await
            .iter()
            .map(|t| tag_to_proto(t, timeout))
            .collect();

        Ok(Response::new(ListNodesResponse { nodes }))
    }

    // ── GetNode ───────────────────────────────────────────────────────────────

    async fn get_node(
        &self,
        req: Request<GetNodeRequest>,
    ) -> Result<Response<NodeInfo>, Status> {
        let addr = req.into_inner().address;
        let timeout = self.timeout_secs();

        let tag = self
            .state
            .registry
            .get(&addr)
            .await
            .ok_or_else(|| Status::not_found(format!("Node {addr} not found")))?;

        Ok(Response::new(tag_to_proto(&tag, timeout)))
    }

    // ── WatchNodes (server-streaming) ─────────────────────────────────────────

    type WatchNodesStream = BoxStream<NodeInfo>;

    async fn watch_nodes(
        &self,
        req: Request<WatchNodesRequest>,
    ) -> Result<Response<Self::WatchNodesStream>, Status> {
        let interval_ms = {
            let ms = req.into_inner().interval_ms;
            if ms == 0 { 1_000 } else { ms as u64 }
        };
        let timeout = self.timeout_secs();
        let state   = Arc::clone(&self.state);

        let stream = IntervalStream::new(time::interval(Duration::from_millis(interval_ms)))
            .then(move |_| {
                let state = Arc::clone(&state);
                async move { state.registry.get_all().await }
            })
            .flat_map(move |nodes| {
                let infos: Vec<Result<NodeInfo, Status>> = nodes
                    .iter()
                    .map(|t| Ok(tag_to_proto(t, timeout)))
                    .collect();
                tokio_stream::iter(infos)
            });

        Ok(Response::new(Box::pin(stream)))
    }

    // ── ProvisionNode (WIP) ───────────────────────────────────────────────────

    async fn provision_node(
        &self,
        req: Request<ProvisionNodeRequest>,
    ) -> Result<Response<ProvisionNodeResponse>, Status> {
        let addr = req.into_inner().address;
        warn!(address = %addr, "ProvisionNode called — BLE Mesh provisioning is WIP");
        Ok(Response::new(ProvisionNodeResponse {
            success: false,
            message: "BLE Mesh provisioning is not yet implemented (WIP branch)".into(),
        }))
    }

    // ── LinkNodeToGrocy ───────────────────────────────────────────────────────

    async fn link_node_to_grocy(
        &self,
        req: Request<LinkNodeRequest>,
    ) -> Result<Response<LinkNodeResponse>, Status> {
        let inner    = req.into_inner();
        let addr     = &inner.address;
        let grocycode = &inner.grocycode;

        // Basic format validation.
        if parse_product_id(grocycode).is_none() {
            return Ok(Response::new(LinkNodeResponse {
                success: false,
                message: format!(
                    "Invalid grocycode \"{grocycode}\": expected \"grcy-p-<id>\""
                ),
            }));
        }

        // Write the new grocycode into the registry.  The BLE task will carry
        // it to the node's GATT characteristic on the next connection.
        self.state.registry.update_grocycode(addr, grocycode).await;

        info!(address = %addr, grocycode = %grocycode, "Node linked to Grocy item");
        Ok(Response::new(LinkNodeResponse {
            success: true,
            message: format!("Node {addr} linked to {grocycode}"),
        }))
    }
}

// ── Server entry point ────────────────────────────────────────────────────────

/// Start the gRPC server and listen on the configured address.
///
/// This function runs indefinitely (until the process exits or the BLE task
/// shuts down the runtime).  Spawn it with `tokio::spawn`.
pub async fn serve(state: Arc<AppState>) -> anyhow::Result<()> {
    let addr = state
        .config
        .grpc_listen_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid GRPC_LISTEN_ADDR: {e}"))?;

    info!(%addr, "gRPC server listening");

    Server::builder()
        .add_service(EtagBridgeServer::new(EtagBridgeService::new(state)))
        .serve(addr)
        .await
        .map_err(Into::into)
}
