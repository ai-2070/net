# RedEX + replication — Go FFI design note

Companion to [`REDEX_DISTRIBUTED_PLAN.md`](REDEX_DISTRIBUTED_PLAN.md)
Phase I and [`CONFIG_REPLICATION.md`](../CONFIG_REPLICATION.md). This
doc scopes the Go FFI surface for Redex + replication so a future
session can land it as focused mechanical work rather than a
greenfield design exercise.

## Status

**Not implemented.** The Node + Python bindings ship full replication
surfaces (config + metrics + status snapshot). The Go binding's
existing FFI surfaces (`compute-ffi`, `rpc-ffi`) target compute
daemons + RPC and do NOT expose Redex/NetDB. Adding replication
to Go requires building a new Redex FFI surface end-to-end.

## Scope

In scope for a v1 Go Redex FFI:

- `Redex` lifecycle (`new`, `new_with_persistent_dir`, free).
- `Redex::enable_replication(mesh)` taking an existing `MeshNodeHandle`
  from the `bindings/go/net` FFI.
- `Redex::open_file(name, cfg)` with the full config surface
  (persistent, fsync policy, retention caps, replication config).
- `RedexFile` lifecycle (handle returned by `open_file`, freed by
  `_free`).
- `RedexFile::append(bytes) -> seq`.
- `RedexFile::next_seq()`.
- `RedexFile::read_range(start, end)` returning a borrowed byte
  slice + length per event.
- `Redex::close_file(name)` + auto-close on `Redex` drop.
- `Redex::replication_prometheus_text() -> *const c_char`.
- `Redex::replication_runtime_count() -> u32`.

Out of scope for v1:

- `RedexFile::tail(start_seq)` async stream — needs a goroutine
  callback bridge. Land in v2 if a consumer needs the streaming
  shape.
- `Redex::replication_status_snapshot()` structured form — operators
  consume the Prometheus text path; structured access for non-
  Prometheus pipelines is v2.
- `Redex::replication_coordinator_for(name)` per-channel handle —
  operators force-transition channels via the metrics + manual
  intervention; the per-channel coordinator handle adds FFI
  complexity without a clear v1 use case.

## Handle model

Reuse the established pattern from `compute-ffi` and `rpc-ffi`:

- Every Rust object crosses the FFI boundary as `*mut T` (a
  heap-allocated `Box<Arc<T>>`).
- Go owns the pointer with a runtime-finalizer; MUST call the
  matching `_free` exactly once.
- `c_int` return values: `0` success, `< 0` error.
- Error detail rides an out-param `*mut *mut c_char` freed via
  `net_redex_free_cstring`.

```c
// New error codes (additions to NET_COMPUTE_* range or a new
// NET_REDEX_* range — likely separate to keep error tables
// disjoint per cdylib).
const int NET_REDEX_OK = 0;
const int NET_REDEX_ERR_NULL = -1;
const int NET_REDEX_ERR_CALL_FAILED = -2;
const int NET_REDEX_ERR_REPLICATION_REQUIRES_ENABLE = -3;
const int NET_REDEX_ERR_INVALID_REPLICATION_CONFIG = -4;
const int NET_REDEX_ERR_PERSISTENT_DIR_MISSING = -5;
const int NET_REDEX_ERR_CHANNEL_NAME_INVALID = -6;
```

## ReplicationConfig wire shape

Cross-FFI struct (no string parsing on the Go side). All numeric
fields stay numeric; the placement-strategy and under-capacity
policy enums get integer encodings so Go doesn't have to round-
trip through strings.

```rust
#[repr(C)]
pub struct CReplicationConfig {
    pub factor: u8,                     // 0 = use default (3)
    pub heartbeat_ms: u64,              // 0 = use default (500)
    pub placement: u8,                  // 0=Standard 1=Pinned 2=ColocationStrict
    pub pinned_nodes_ptr: *const u64,   // null = unused; else valid for pinned_nodes_len
    pub pinned_nodes_len: usize,
    pub leader_pinned: u64,             // 0 = unused (NodeId 0 is reserved)
    pub leader_pinned_present: bool,    // explicit "set" flag
    pub on_under_capacity: u8,          // 0=Withdraw 1=EvictOldest
    pub replication_budget_fraction: f32, // 0.0 = use default (0.5)
}
```

`leader_pinned_present` plus the value disambiguates "explicitly
set to NodeId 0" from "leave at default". NodeId 0 isn't currently
reserved at the substrate level, but the FFI layer treats it as a
sentinel so the wire shape stays stable.

## Crate layout

```
bindings/go/redex-ffi/
├── Cargo.toml          # mirrors compute-ffi shape (cdylib + rlib)
├── src/lib.rs          # FFI surface
└── tests/              # symbol-stability tests (no Rust unit tests
                       # for FFI — Go-side test suite covers behavior)
```

`Cargo.toml` enables the same `net` feature set as compute-ffi
(`"net", "netdb", "redex-disk"`) so cfg-gated field offsets line
up if both cdylibs link into the same Go test binary. See
compute-ffi's Cargo.toml comment on the layout-alignment hazard.

## Go-side surface

```go
package net

// Redex wraps the C handle with a finalizer-managed lifecycle.
type Redex struct {
    handle *C.RedexHandle
}

func NewRedex() *Redex { /* ... */ }
func NewRedexWithPersistentDir(dir string) (*Redex, error) { /* ... */ }
func (r *Redex) EnableReplication(mesh *NetMesh) error { /* ... */ }
func (r *Redex) OpenFile(name string, cfg *RedexFileConfig) (*RedexFile, error) { /* ... */ }
func (r *Redex) ReplicationPrometheusText() string { /* ... */ }
func (r *Redex) ReplicationRuntimeCount() uint32 { /* ... */ }
func (r *Redex) CloseFile(name string) error { /* ... */ }

type RedexFileConfig struct {
    Persistent              bool
    FsyncEveryN             uint64  // 0 = unset
    FsyncIntervalMs         uint64  // 0 = unset; mutually exclusive with FsyncEveryN
    RetentionMaxEvents      uint64  // 0 = unset
    RetentionMaxBytes       uint64  // 0 = unset
    RetentionMaxAgeMs       uint64  // 0 = unset
    Replication             *ReplicationConfig // nil = single-node
}

type ReplicationConfig struct {
    Factor                    uint8   // 0 = default (3)
    HeartbeatMs               uint64  // 0 = default (500)
    Placement                 PlacementStrategy
    PinnedNodes               []uint64
    LeaderPinned              *uint64 // nil = let election decide
    OnUnderCapacity           UnderCapacityPolicy
    ReplicationBudgetFraction float32 // 0.0 = default (0.5)
}

type PlacementStrategy int
const (
    PlacementStandard         PlacementStrategy = 0
    PlacementPinned           PlacementStrategy = 1
    PlacementColocationStrict PlacementStrategy = 2
)

type UnderCapacityPolicy int
const (
    UnderCapacityWithdraw    UnderCapacityPolicy = 0
    UnderCapacityEvictOldest UnderCapacityPolicy = 1
)
```

## Test strategy

- **Rust-side**: a single symbol-stability test (`cargo test`)
  asserts every `pub extern "C"` symbol exists with the
  documented signature. No behavioral tests on the Rust side —
  the FFI is a thin wrapper around the core `Redex` types whose
  behavior is already covered by the 281 Rust unit tests.
- **Go-side**: a `go test` suite mirrors the Node + Python
  binding tests. At minimum, the operator surface trip:
  - Construct `Redex`, assert `ReplicationRuntimeCount() == 0`,
    assert `ReplicationPrometheusText() == ""`.
  - `EnableReplication(mesh)`, `OpenFile(...)` with
    replication config, assert count = 1 and prometheus text
    contains the channel name.
  - `CloseFile`, assert count = 0.
- **No end-to-end two-node Go test** for v1 — the Node + Python
  bindings already exercise the wire path; the Go FFI test
  covers the binding-side plumbing.

## Estimate

- Rust FFI surface: 800–1200 lines (mirrors compute-ffi's
  density for the operator-facing slice).
- Go wrapper: 400–600 lines.
- Tests: 100–200 lines Rust + 200–400 lines Go.
- Total: 3–5 focused days for a single engineer familiar with
  the existing compute-ffi pattern.

## Follow-ups (post-v1)

- Streaming `tail(start_seq)` via goroutine callback bridge.
- Structured `replication_status_snapshot()`.
- Per-channel coordinator handle.
- C/FFI binding for non-Go consumers — same scope; the cdylib
  shipped here serves both.
