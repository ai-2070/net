# SDK Aggregator + Subnet-Scaling Surface Plan

Bring the subnet-scaling primitives (`Visibility`, `SubnetGateway`, channel-config registry) and the aggregator-lifecycle primitives (`AggregatorDaemon`, `AggregatorRegistry`, `LifecycleGroup`, `HealthMonitor`, `aggregator.registry` RPC) into `net-sdk` and through to the Node, Python, Go, and C bindings.

Mirrors `SDK_GROUPS_SURFACE_PLAN.md`'s shape. The substrate landed across two branches (`subnet-scaling`, `aggregator-lifecycle`) — most types already exist; the SDK surfaces are mostly absent (see "Current state" below).

## Goals

- Three operator-facing client surfaces in every language: `RegistryClient` (list/spawn/unregister live aggregator groups via `aggregator.registry` RPC), `FoldQueryClient` (cross-subnet detail-on-demand via `fold.query`), and `Visibility`-aware channel-config setters.
- One daemon-author surface (Rust + selected bridges): construct `AggregatorConfig`, register groups via `AggregatorRegistry`, attach `HealthMonitor` — for hosts that embed the substrate rather than running the `net-aggregator-daemon` binary.
- Parity across Node (NAPI), Python (PyO3), Go (CGO). Same wire types, same error kinds, same factory-callback infrastructure where applicable — reuse the trampolines `SDK_GROUPS_SURFACE_PLAN` already built.
- Typed `RegistryClientError` / `FoldQueryClientError` on every language, with kind discrimination for `unknown-template` / `duplicate-group-name` / `spawn-not-supported` / `transport` / `codec` — mirrors the `GroupError` pattern.

## Non-goals

- **Re-implementing the daemon's TOML config in every language.** The `net-aggregator-daemon` binary is the supported deployment unit; SDKs operate against running daemons via RPC, not by re-parsing TOML. Languages that need to *host* aggregators in-process (e.g. embedded Rust use) get the daemon-author surface; the others (Node/Python/Go) get *client-only* surfaces.
- **Changing substrate semantics.** Wire shapes are locked. SDK code is wrap-and-forward.
- **Spawn/Scale wire-shape changes.** The `Spawn { template_name, group_name, replica_count }` shape is what daemons accept; SDKs marshal exactly that. Future ops (e.g. dedicated Scale) land in the substrate first.
- **Exposing the `Summarizer` trait across bindings.** Custom summarizers are Rust-only; bindings get the two built-ins (capability, reservation) via the existing aggregator config + the daemon's template registry.
- **Public release wheels.** Same caveat as the groups plan — adding to the published feature set is a separate decision.

---

## Current state

| Surface                                  | Rust SDK    | Node/TS | Python | Go    | C ABI |
|------------------------------------------|-------------|---------|--------|-------|-------|
| `SubnetId` / `SubnetPolicy` / `SubnetRule` | ✓        | ✓ (POJO)| ✓ (dict)| ✓    | ✗     |
| `SubnetGateway`                          | ✓ (re-export)| ✗      | ✗      | ✗     | ✗     |
| `Visibility` (channel)                   | ✓ (re-export)| ✗      | ✗      | ✗     | ✗     |
| `AggregatorSnapshot` (read-only)         | ✓           | ✓       | ✓      | ✗     | ✗     |
| `AggregatorRegistrySnapshot` (read-only) | ✓           | ✗       | ✗      | ✗     | ✗     |
| `AggregatorConfig`                       | ✗           | ✗       | ✗      | ✗     | ✗     |
| `AggregatorDaemon`                       | ✗           | ✗       | ✗      | ✗     | ✗     |
| `AggregatorRegistry`                     | ✗           | ✗       | ✗      | ✗     | ✗     |
| `LifecycleDaemon` / `LifecycleGroup`     | ✗           | ✗       | ✗      | ✗     | ✗     |
| `HealthMonitor`                          | ✗           | ✗       | ✗      | ✗     | ✗     |
| `FoldQueryClient`                        | ✗           | ✗       | ✗      | ✗     | ✗     |
| `RegistryClient`                         | ✗           | ✗       | ✗      | ✗     | ✗     |
| Wire types (`RegistryRequest` etc.)      | ✗ (re-export only) | ✗ | ✗      | ✗     | ✗     |

Slice 8 (`net-aggregator-daemon` binary) closes the "how do I run this?" question for an operator with a TOML file in hand. This plan closes "how do I drive it from my own process?" for SDK consumers.

---

## SDK design decisions

### 1. Client surfaces are universal; daemon-author surfaces are Rust-only

`RegistryClient` and `FoldQueryClient` are pure RPC clients — they need only `MeshNode::call` + postcard codec. Every binding already has a `MeshNode` handle. Wiring these in every language is mechanical.

Daemon-author surfaces (`AggregatorDaemon`, `AggregatorRegistry`, `LifecycleGroup`, `HealthMonitor`) are async-trait-heavy and assume a tokio runtime in the host process. Bridging them through NAPI / PyO3 / CGO means re-bridging `LifecycleDaemon` (an async trait) into language-native async — same pain that motivated the `LifecycleDaemon` sibling-vs-`MeshDaemon` decision in the first place.

**Decision:** ship daemon-author types only in the Rust SDK. Other bindings get *client-only* surfaces + the existing `aggregator-daemon` binary as the supported deployment unit. Operators who want a Node / Python / Go *aggregator host* run the binary alongside their app and RPC into it.

### 2. `Visibility` flows through the channel-config setter, not as a free type

`Visibility` only matters when constructing a `ChannelConfig`. Exposing it as a standalone enum across bindings has no value unless the binding can also build a `ChannelConfig`. For Node / Python / Go, surface it via the `register_channel` (or equivalent) call's parameters — operator says `register_channel(name, visibility="parent-visible", …)` — rather than as a separately-imported enum.

C ABI gets a `NET_VISIBILITY_*` enum (`NET_VISIBILITY_GLOBAL`, `NET_VISIBILITY_PARENT_VISIBLE`, `NET_VISIBILITY_SUBNET_LOCAL`) the same way it handles other discriminants.

### 3. RegistryClient builders mirror the Rust API verbatim

Every language gets:

- `RegistryClient::new(mesh)` constructor
- `with_deadline(duration)` builder
- `list(target_node_id) -> Future<RegistryGroupSummary[]>`
- `spawn(target_node_id, template_name, group_name, replica_count) -> Future<RegistryGroupSummary>`
- `unregister(target_node_id, group_name) -> Future<bool>`

The `RegistryGroupSummary` shape is identical across languages: `{ name, group_seed (32 bytes / hex string), replicas: [{generation, healthy, diagnostic, placement_node_id}] }`.

### 4. FoldQueryClient gets the same cache semantics as Rust

The Rust client caches `LatestSummary` results for 5s by default (`DEFAULT_QUERY_CACHE_TTL`). Other languages honor the same default + expose `with_ttl(duration)` / `with_deadline(duration)` builders. `SummarizeNow` is never cached.

### 5. Wire-error discrimination is structured, not stringly-typed

`RegistryClientError` / `FoldQueryClientError` translate to language-native error types with a `kind` discriminator. Mirrors the `GroupError` pattern from `SDK_GROUPS_SURFACE_PLAN`:

- Node/TS: `class RegistryClientError extends Error { kind: 'transport'|'codec'|'unknown-template'|...; serverDetail?: string; }`
- Python: `RegistryClientError(Exception)` with `kind` attribute + subclasses (`UnknownTemplate`, `DuplicateGroupName`, `SpawnNotSupported`) inheriting from it.
- Go: `RegistryClientError struct { Kind, Detail string }` with `Is(target error) bool` matching kinds.
- C: `net_registry_error_kind_t` enum + `net_registry_last_error_detail()` accessor.

---

## Stage 0 — net-sdk scaffolding

Add a `net_sdk::aggregator` module (Rust SDK) that re-exports the substrate types other layers will lean on. No new feature gates — the existing `net` feature is sufficient since all substrate types live under `behavior::aggregator` / `behavior::lifecycle`.

```rust
// sdk/src/aggregator.rs

pub use net::adapter::net::behavior::aggregator::{
    // Client-only surfaces (every binding can re-export these).
    FoldQueryClient, FoldQueryClientError, FoldQueryError, FoldQueryOp,
    RegistryClient, RegistryClientError, RegistryGroupSummary,
    RegistryReplicaSummary, RegistryRequest, RegistryResponse, RegistryRpcError,
    DEFAULT_QUERY_CACHE_TTL, DEFAULT_QUERY_DEADLINE, DEFAULT_REGISTRY_DEADLINE,

    // Daemon-author surfaces (Rust-only re-exports).
    AggregatorConfig, AggregatorDaemon, AggregatorError, AggregatorPublishError,
    AggregatorRegistry, AggregatorRegistryError, AggregatorGroupEntry,
    CapabilityFoldSummarizer, ReservationFoldSummarizer, Summarizer,
    SummaryAnnouncement, SpawnFn, SpawnRequest, snapshot_group,
    REGISTRY_SERVICE, FOLD_QUERY_SERVICE,
};
pub use net::adapter::net::behavior::lifecycle::{
    HealthMonitor, HealthMonitorStats, LifecycleDaemon, LifecycleError,
    LifecycleGroup, LifecycleGroupError, LifecycleHandle, ReplicaContext,
    ReplicaHealth,
};
```

Add a `net_sdk::subnets` re-export for `Visibility` (already present per the inventory, but worth verifying with a test).

### Exit criteria (Stage 0)

- `net_sdk::aggregator::*` resolves without compile errors.
- A doctest demonstrates `RegistryClient::new(mesh).list(target).await` against an in-memory two-node mesh, mirroring the existing `tests/aggregator_registry_rpc.rs` shape.

---

## Stage 1 — Rust SDK surface

The Rust SDK is mostly re-exports (Stage 0). Stage 1 layers ergonomic helpers that mask the few rough edges:

### `RegistryClient::new_for_node(mesh, target_node_id)`

Variant that binds a target node id at construction time so subsequent calls don't repeat it:

```rust
let client = RegistryClient::for_node(mesh.clone(), target_node_id);
let groups = client.list().await?;
let spawned = client.spawn("primary", "newgrp", 3).await?;
```

This is just an `Arc`-clone wrapper over `RegistryClient` + a stored `u64`. Operators talking to multiple registries keep using the base constructor.

### `AggregatorRegistry::install_default_service(mesh)` helper

A daemon-author shortcut that wraps `install_registry_service_with_spawner` against a `NoOpSpawnFn` for read-only daemons:

```rust
let registry = Arc::new(AggregatorRegistry::new());
let _serve = registry.install_default_service(&mesh)?; // no Spawn support
```

`install_default_service` calls `install_registry_service` (already exists). This isn't strictly new — it's a doc alias to make the read-only case discoverable. Decision: skip if the existing name is already discoverable; add only if review shows operators tripping on it.

### Tests (Rust SDK)

- `tests/aggregator_registry_client_round_trip.rs` — two in-process MeshNodes, daemon side installs the registry + a single template, client side lists / spawns / unregisters via `net_sdk::aggregator::RegistryClient`.
- Doctest on `net_sdk::aggregator` module-level docstring that demonstrates the typical operator flow (boot daemon binary → spawn from SDK).

### Exit criteria (Stage 1)

- `net_sdk::aggregator::RegistryClient` and `FoldQueryClient` are reachable from a `use net_sdk::aggregator::*;`.
- One round-trip integration test passes.
- Existing tests (`tests/aggregator_fold_query.rs`, `tests/aggregator_registry_rpc.rs`) move under the SDK or get a peer SDK-side test — they pin the substrate, not the SDK boundary.

---

## Stage 2 — TypeScript (NAPI)

### NAPI crate additions

Add a `aggregator.rs` module under `bindings/node/src/`. Mirrors the `groups.rs` shape from `SDK_GROUPS_SURFACE_PLAN`.

Exports (NAPI):

```rust
#[napi]
pub struct RegistryClient { inner: Arc<net_sdk::aggregator::RegistryClient> }

#[napi]
impl RegistryClient {
    #[napi(factory)]
    pub fn new(mesh: &MeshNode) -> Self { ... }

    #[napi]
    pub fn with_deadline(&self, ms: u32) -> Self { ... }

    #[napi]
    pub async fn list(&self, target_node_id: BigInt) -> napi::Result<Vec<RegistryGroupSummaryJs>> { ... }

    #[napi]
    pub async fn spawn(
        &self,
        target_node_id: BigInt,
        template_name: String,
        group_name: String,
        replica_count: u32,
    ) -> napi::Result<RegistryGroupSummaryJs> { ... }

    #[napi]
    pub async fn unregister(
        &self,
        target_node_id: BigInt,
        group_name: String,
    ) -> napi::Result<bool> { ... }
}

#[napi(object)]
pub struct RegistryGroupSummaryJs {
    pub name: String,
    pub group_seed_hex: String,          // 64-char hex; BigInt is awkward at 32 bytes
    pub replicas: Vec<RegistryReplicaRowJs>,
}

#[napi(object)]
pub struct RegistryReplicaRowJs {
    pub generation: BigInt,              // u64
    pub healthy: bool,
    pub diagnostic: Option<String>,
    pub placement_node_id: Option<BigInt>,
}
```

`FoldQueryClient` follows the same shape: `new`, `with_deadline`, `with_ttl` (ms), `query_latest(target_node_id, kind) -> SummaryAnnouncementJs[]`, `query_summarize_now(...)`, `invalidate_cache()`, `invalidate_target(target_node_id)`.

Error mapping: convert `RegistryClientError` to napi-native via a JS `RegistryClientError extends Error` constructor that carries `kind: string` (transport / codec / unknown-template / duplicate-group-name / spawn-rejected / spawn-not-supported) + optional `serverDetail`.

### TS SDK additions

```typescript
// sdk-ts/src/aggregator.ts
export class RegistryClient {
  constructor(mesh: MeshNode);
  withDeadline(ms: number): RegistryClient;
  list(targetNodeId: bigint): Promise<RegistryGroupSummary[]>;
  spawn(
    targetNodeId: bigint,
    templateName: string,
    groupName: string,
    replicaCount: number,
  ): Promise<RegistryGroupSummary>;
  unregister(targetNodeId: bigint, groupName: string): Promise<boolean>;
}

export interface RegistryGroupSummary {
  name: string;
  groupSeedHex: string;
  replicas: RegistryReplicaRow[];
}

export interface RegistryReplicaRow {
  generation: bigint;
  healthy: boolean;
  diagnostic?: string;
  placementNodeId?: bigint;
}

export class RegistryClientError extends Error {
  readonly kind:
    | 'transport' | 'codec' | 'unknown-template'
    | 'duplicate-group-name' | 'spawn-rejected' | 'spawn-not-supported';
  readonly serverDetail?: string;
}
```

`FoldQueryClient` analogously, with `queryLatest` / `querySummarizeNow` / `invalidateCache` / `invalidateTarget`.

### Tests (TS)

- `sdk-ts/test/aggregator_registry_client.test.ts` — boots a Node-side `MeshNode`, runs against a separately-spawned `net-aggregator-daemon` (the test fixture launches the binary on an ephemeral port, captures pubkey via stdout).
- `sdk-ts/test/aggregator_registry_error_kinds.test.ts` — pin every error-kind translation (mocked transport returning each `RegistryRpcError` variant).

### Exit criteria (Stage 2)

- `import { RegistryClient } from '@ai-2070/net-sdk'` resolves.
- Round-trip test passes against a daemon subprocess.
- vitest run is green.

---

## Stage 3 — Python (PyO3)

### PyO3 surface

Add `aggregator.rs` under `bindings/python/src/`, parallel to the Node surface.

```rust
#[pyclass(unsendable)]
pub struct RegistryClient { inner: Arc<net_sdk::aggregator::RegistryClient> }

#[pymethods]
impl RegistryClient {
    #[new]
    fn new(mesh: &PyAny) -> PyResult<Self> { ... }

    fn with_deadline(&self, seconds: f64) -> PyResult<Self> { ... }

    fn list<'py>(&self, py: Python<'py>, target_node_id: u64) -> PyResult<&'py PyAny> {
        // pyo3-asyncio future_into_py
    }

    fn spawn<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        template_name: String,
        group_name: String,
        replica_count: u8,
    ) -> PyResult<&'py PyAny> { ... }

    fn unregister<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        group_name: String,
    ) -> PyResult<&'py PyAny> { ... }
}
```

Returned shapes are dicts (matching the existing `SubnetPolicy → dict` convention):

```python
{
    "name": str,
    "group_seed_hex": str,
    "replicas": [
        {"generation": int, "healthy": bool, "diagnostic": str | None, "placement_node_id": int | None},
        ...
    ],
}
```

Errors:

```python
class RegistryClientError(Exception):
    kind: str  # one of the discriminator strings
    server_detail: str | None

class UnknownTemplate(RegistryClientError): pass
class DuplicateGroupName(RegistryClientError): pass
class SpawnRejected(RegistryClientError): pass
class SpawnNotSupported(RegistryClientError): pass
```

`FoldQueryClient` ships with the same shape — `query_latest`, `query_summarize_now`, `invalidate_cache`, `invalidate_target`. Uses asyncio futures via pyo3-asyncio (already in the binding's dep set per the inventory).

### Python tests

- `bindings/python/python/tests/test_aggregator_registry.py` — pytest-asyncio. Boots the `net-aggregator-daemon` binary via `subprocess.Popen` on an ephemeral port, captures the bound port + pubkey from a `--print-bootstrap` flag the daemon will need (add this; see Stage 6).
- `test_aggregator_error_kinds.py` — each `RegistryRpcError` variant translates to its typed Python exception.

### Exit criteria (Stage 3)

- `from net_mesh.aggregator import RegistryClient` works.
- Round-trip test passes against subprocess daemon.
- pytest run is green.

---

## Stage 4 — Go (CGO)

### C header additions

Add `aggregator-ffi/` as a new cdylib crate under `bindings/go/`. Parallels `compute-ffi` / `meshos-ffi` etc.

Exported C symbols:

```c
// Opaque handle.
typedef struct net_registry_client_handle_t net_registry_client_handle_t;

// Constructor — takes a mesh handle from the existing net-ffi.
net_registry_client_handle_t* net_registry_client_new(net_mesh_handle_t* mesh);
void net_registry_client_free(net_registry_client_handle_t* client);

// Builder.
void net_registry_client_with_deadline(
    net_registry_client_handle_t* client,
    uint64_t millis);

// Operations — return JSON-shaped char* the Go side parses.
// (Same convention as deck-ffi's snapshot serializers.)
char* net_registry_client_list(
    net_registry_client_handle_t* client,
    uint64_t target_node_id,
    int32_t* out_error_kind);

char* net_registry_client_spawn(
    net_registry_client_handle_t* client,
    uint64_t target_node_id,
    const char* template_name,
    const char* group_name,
    uint8_t replica_count,
    int32_t* out_error_kind);

bool net_registry_client_unregister(
    net_registry_client_handle_t* client,
    uint64_t target_node_id,
    const char* group_name,
    int32_t* out_error_kind);

// Error-kind discrimination — values match the Rust enum order.
#define NET_REGISTRY_OK                       0
#define NET_REGISTRY_ERR_TRANSPORT            1
#define NET_REGISTRY_ERR_CODEC                2
#define NET_REGISTRY_ERR_UNKNOWN_TEMPLATE     3
#define NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME 4
#define NET_REGISTRY_ERR_SPAWN_REJECTED       5
#define NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED  6

// Get the operator-facing detail string for the last error.
const char* net_registry_last_error_detail(net_registry_client_handle_t* client);
```

Same shape for `FoldQueryClient` under a `fold-query-ffi/` cdylib (or fold into `aggregator-ffi`).

### Go package additions

```go
// go/aggregator.go
package net_mesh

type RegistryClient struct { ... }

func NewRegistryClient(mesh *MeshNode) *RegistryClient
func (c *RegistryClient) WithDeadline(d time.Duration) *RegistryClient
func (c *RegistryClient) List(ctx context.Context, targetNodeID uint64) ([]RegistryGroupSummary, error)
func (c *RegistryClient) Spawn(ctx context.Context, targetNodeID uint64, templateName, groupName string, replicaCount uint8) (RegistryGroupSummary, error)
func (c *RegistryClient) Unregister(ctx context.Context, targetNodeID uint64, groupName string) (bool, error)

type RegistryGroupSummary struct {
    Name         string
    GroupSeedHex string
    Replicas     []RegistryReplicaRow
}

type RegistryReplicaRow struct {
    Generation      uint64
    Healthy         bool
    Diagnostic      string  // empty when nil on the wire
    PlacementNodeID uint64  // 0 when absent
}

type RegistryClientError struct {
    Kind   string  // "transport" | "codec" | ...
    Detail string
}

func (e *RegistryClientError) Error() string { return ... }
```

`context.Context` becomes the deadline carrier — the CGO call honors `ctx.Deadline()` if set, falls back to the client's configured default otherwise.

### Go tests

- `go/aggregator_test.go` — boots `net-aggregator-daemon` via `exec.Command`, drives the registry client.
- Subprocess fixtures live under `go/testdata/` (config TOML + a bootstrap helper).

### Exit criteria (Stage 4)

- `go test ./...` covers list / spawn / unregister round-trip + each error-kind translation.
- The C header lives under a single canonical path (`bindings/go/aggregator-ffi/include/net_aggregator.h`) that Go can cgo-include.

---

## Stage 5 — C ABI (standalone)

The C ABI lives in `net/crates/net/src/ffi/` (the main crate's `ffi` module). Today it has no aggregator hooks (per the inventory).

Add a new submodule `ffi/aggregator.rs` exposing the same symbols Stage 4's `aggregator-ffi` cdylib does — but compiled into the main `net-mesh` cdylib alongside the existing `net_init` / `net_ingest` / etc. The CGO `aggregator-ffi` crate becomes a thin re-export, so non-Go C consumers get identical symbols from the main cdylib.

### Stage 5a: Visibility / channel-config setters

C consumers also need a way to *configure* channels with the visibility tiers — today they just see the read-only snapshot path. Add:

```c
// Visibility discriminant.
typedef enum {
    NET_VISIBILITY_GLOBAL          = 0,
    NET_VISIBILITY_PARENT_VISIBLE  = 1,
    NET_VISIBILITY_SUBNET_LOCAL    = 2,
} net_visibility_t;

// Register a channel with a visibility tier. Wraps
// `ChannelConfigRegistry::insert`. The mesh must have a
// registry installed (`net_set_channel_config_registry`).
int net_register_channel(
    net_mesh_handle_t* mesh,
    const char* name,
    net_visibility_t visibility,
    /* future: auth-token args */);
```

### Exit criteria (Stage 5)

- `cbindgen` regenerated header lists the new symbols.
- A C smoke test under `tests/c_ffi/` constructs a registry client + calls list against a daemon subprocess.

---

## Stage 6 — Daemon-side `--print-bootstrap` flag

Both Node and Python tests need the daemon's bound `(addr, public_key, node_id)` triple to perform their handshake. Today the daemon logs it via `tracing::info!`; parsing tracing output is brittle.

Add a `--print-bootstrap` flag to `net-aggregator-daemon` that prints a single JSON line to stdout *before* entering the wait loop:

```json
{"node_id":12345,"bound_addr":"127.0.0.1:54321","public_key_hex":"abcd..."}
```

Bindings' subprocess fixtures read stdout, parse the first line, then drive their handshake.

### Exit criteria (Stage 6)

- `net-aggregator-daemon --config foo.toml --print-bootstrap` emits the JSON line exactly once.
- All language tests use the flag.

---

## Cross-language wire contract

These are locked across every binding:

| Concept                          | Wire shape                                                                |
|----------------------------------|---------------------------------------------------------------------------|
| `RegistryGroupSummary.group_seed` | 32 raw bytes (postcard) → 64-char lowercase hex string in language SDKs  |
| `RegistryReplicaRow.generation`  | u64 (postcard) → BigInt (TS), int (Py), uint64 (Go)                       |
| `RegistryReplicaRow.diagnostic`  | `Option<String>` (postcard) → nullable string in every language           |
| Deadline param                   | TS: number (ms), Py: float (seconds), Go: `time.Duration`                 |
| Error kinds (strings)            | `transport`, `codec`, `unknown-template`, `duplicate-group-name`, `spawn-rejected`, `spawn-not-supported` |

Implementations diverging from this table fail their compatibility test.

---

## Phasing

```text
Stage 0 (sdk scaffold) ─┐
                        ├─→ Stage 1 (Rust SDK)
                        │     │
                        │     ├─→ Stage 2 (Node)
                        │     ├─→ Stage 3 (Python)
                        │     └─→ Stage 5 (C ABI)
                        │           │
                        │           └─→ Stage 4 (Go via CGO)
                        │
                        └─→ Stage 6 (daemon --print-bootstrap)
                              (gates Stages 2, 3, 4 tests)
```

Stages 1–5 are independent after Stage 0 lands; can ship in any order. Stage 6 is short (a single CLI flag) but must land before the binding integration tests are runnable.

---

## What this plan does NOT specify

- **CLI remote-attach** for the `net-mesh` CLI (NET_CLI_PLAN.md Phase 5). The SDK has the surfaces; the CLI's `CliContext` MeshNode bootstrap is a separate substrate gap.
- **Dedicated Scale RPC**. Today expressible as Unregister + Spawn-with-new-count. A future substrate op (in-place `LifecycleGroup::add_replica` / `remove_last`) flows through this SDK plan additively — wire types extend, language surfaces gain a `scale()` method.
- **Auth on the registry RPC**. Spawn/Unregister are operationally privileged; gating them behind capability auth is a separate plan (touches `ChannelConfig::require_token` plumbing on the registry-service channel).
- **Aggregator host bindings for Node/Python/Go**. Decision #1 above explicitly defers this to a future plan once the client-only surfaces have soaked.
