# BLE Mesh Plan for etag (Firmware + Raspberry Pi Bridge)

## Goals

- Support a deployment of 30+ eTag nodes with improved range and reliability.
- Use Bluetooth Mesh for node-to-node propagation (including optional relay/repeater nodes).
- Keep one central Raspberry Pi as the Grocy integration bridge and operational control point.
- Preserve low-power operation on battery-powered shelf tags.

## Constraints and assumptions

- Hardware: nRF52840-based tags.
- Firmware stack: Embassy-based app with existing display/buttons logic.
- Bridge host: Linux/Raspberry Pi, always-on, with network access to Grocy.
- Existing direct GATT behavior should continue to work during migration.

## Target architecture

### Firmware roles

1. **Tag Node (battery-powered shelf tag)**
   - BLE Mesh node using Low Power Node (LPN) mode.
   - Publishes stock-change events and battery status to mesh model addresses.
   - Subscribes to product metadata/configuration updates from bridge or admin tools.
   - Maintains local state cache (product name, Grocycode, stock count) for display continuity.

2. **Relay/Repeater Node (mains-powered, optional)**
   - Same base firmware profile, but configured as non-LPN relay/friend-capable nodes.
   - Forwards mesh traffic to extend coverage and improve delivery.
   - No Grocy sync logic; routing-only responsibility.

3. **Provisioning mode**
   - Unprovisioned beacon mode for onboarding.
   - Secure provisioning and app-key binding for required models.
   - Role selection (Tag vs Relay) at provisioning time through config messages.

### Bridge server roles (Raspberry Pi)

1. **Mesh Gateway + Provisioner**
   - Initializes/manages the mesh network (network key/app keys).
   - Provisions new tags/repeaters and assigns unicast/group addresses.
   - Applies model subscriptions/publications according to deployment policy.

2. **State Aggregator**
   - Consumes mesh publications from all nodes.
   - Maintains authoritative per-node/device state in the bridge registry.
   - Performs deduplication/idempotency using message metadata and monotonic counters.

3. **Grocy Sync Orchestrator**
   - Converts mesh stock deltas to Grocy API updates.
   - Pushes downstream changes from Grocy/admin UI back into mesh config/state messages.
   - Handles retry and backoff while preserving eventual consistency.

4. **Operations Plane**
   - Exposes health telemetry (last seen, battery, relay path quality, message loss indicators).
   - Supports reconfiguration: TTL, publish periods, relay policy, friendship settings.
   - Produces audit logs for provisioning and stock transactions.

## Message/model plan

Use vendor-specific mesh models first (fast path), with optional migration to standard models later.

1. **Inventory State Model (vendor model)**
   - Fields: node_id, grocycode/product_id, stock_count, battery_pct, seq/version.
   - Operations: `SetStockDelta`, `SetStockAbsolute`, `InventoryStatus`.

2. **Provisioning/Config Model (vendor model)**
   - Fields: product_name, grocycode, role, publish interval, power profile.
   - Operations: `SetConfig`, `GetConfig`, `ConfigStatus`.

3. **Health/Diagnostics Model**
   - Low battery, node fault, stale cache, friend loss, relay congestion hints.

## Delivery and consistency strategy

- Use acknowledged updates for critical writes (provisioning/config), unacknowledged for periodic telemetry.
- Include per-node monotonic revision/sequence for idempotent bridge processing.
- Bridge stores last applied revision to prevent duplicate Grocy writes.
- For offline tags, queue authoritative updates in bridge and deliver when node reconnects to mesh friend/relay path.

## Security plan

- Use BLE Mesh security primitives: NetKey/AppKey separation and key refresh workflow.
- Separate app keys by functional domain (inventory vs provisioning) where possible.
- Restrict provisioning to trusted bridge identity and operator-approved flow.
- Rotate keys on compromise/lifecycle events and maintain bridge-side key version tracking.

## Power and performance plan

- Tag nodes: LPN + Friend support target; tune poll timeout against latency requirements.
- Relay nodes: mains-powered profile with relay enabled and higher receive duty cycle.
- Set bounded publish frequency for battery and heartbeat traffic.
- Measure message latency and delivery ratio at 30+ nodes with realistic placement before go-live.

## Migration plan (phased)

### Phase 0 — Foundation

- Add mesh-compatible abstraction layer in firmware and bridge (transport-agnostic inventory events).
- Keep existing GATT bridge path as fallback.

### Phase 1 — Provisioning + basic inventory over mesh

- Implement provisioning flow from Raspberry Pi.
- Implement vendor Inventory State model for stock/battery uplink.
- Bridge consumes mesh events and syncs Grocy.

### Phase 2 — Downlink config + repeaters

- Add bridge-to-tag config pushes (product name, grocycode, stock corrections).
- Enable dedicated relay/repeater profile and deployment tooling.

### Phase 3 — Reliability hardening

- Dedup/replay protection verification at scale.
- Failure recovery tests (power loss, bridge restart, relay loss).
- Key refresh and reprovisioning runbooks.

### Phase 4 — Production cutover

- Disable default direct GATT path for production tags (retain temporary debug switch).
- Finalize observability dashboards and on-call alerts.

## Firmware implementation work breakdown

1. Add mesh transport module boundaries (events, command handlers, persistence hooks).
2. Implement role-aware runtime config (Tag LPN vs Relay/Friend profile).
3. Implement vendor models and payload codecs for inventory/config/health.
4. Add persistent storage for provisioning data, keys, and last config revision.
5. Integrate button-driven stock updates with mesh publish path.
6. Add offline-safe local queue/counter handling for eventual bridge consistency.
7. Add power-profile tuning and long-run battery benchmarks.

## Bridge implementation work breakdown

1. Introduce mesh gateway service alongside existing BLE scanner module.
2. Add provisioning manager (node onboarding, address allocation, app key bind).
3. Build mesh message ingest pipeline with dedup and ordering guards.
4. Adapt `DeviceRegistry` to hold mesh identity (unicast/group, role, revisions).
5. Extend Grocy sync layer for idempotent delta/absolute updates from mesh.
6. Add command channel for downlink config and remote stock correction.
7. Add observability endpoints/logging for mesh topology and node health.

## Testing and validation plan

- Unit tests for model codecs, revision handling, dedup/idempotency.
- Integration tests for bridge ingest and Grocy sync under duplicate/out-of-order events.
- Hardware-in-the-loop testbed: 30+ nodes with mixed Tag/Relay profiles.
- Soak tests for at least 24h to validate bridge stability and battery assumptions.
- Failure drills: bridge restart, relay outage, key rotation, reprovisioning.

## Rollout and operations checklist

- Document install/provisioning SOP for field deployment.
- Maintain inventory map: node UUID ↔ product ↔ location ↔ mesh address.
- Define SLOs: event-to-Grocy latency, successful sync ratio, stale node threshold.
- Add rollback strategy: temporary GATT fallback for critical incidents.
