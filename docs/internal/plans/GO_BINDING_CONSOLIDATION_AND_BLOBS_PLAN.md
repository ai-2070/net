# Go binding — fold the reference package into `go/`, finish Dataforts blobs

## Status

Planned, 2026-10-03. Targets the release after 0.39. Branch `LZL0/go-blobs`.
No slice has landed.

**Amended 2026-10-03 after a source-grounded review at `d1b6f29`.** The
review accepted the direction (one Go module at `go/`, the Rust FFI crates
stay put, one `libnet`, port rather than copy) and held implementation on
nine findings, R1–R9. All nine were re-checked against source before this
amendment and confirmed. Each change below is marked *Amended (Rn)* next to
the text it corrects. The summary is in
[Review amendments](#review-amendments-2026-10-03) at the end.
Implementation waits on a re-read of this version.

## The gap

The repository ships Go in two places:

- **`go/`**: the real binding. It has its own `go.mod`
  (`github.com/ai-2070/net/go`), CI runs `go test ./...` against it, and it
  links the single `libnet` cdylib built from `bindings/go/net-ffi`.
- **`net/crates/net/bindings/go/`**: the eight Rust FFI crates (`net-ffi` plus
  the seven rlib shims it aggregates) **and** a Go package, `bindings/go/net/`
  (21 files, about 16.5k lines). That package has no `go.mod`, and no CI job
  builds or tests it. Its README calls it a reference implementation for
  people to copy. Several `go/` files were ported from it piecemeal
  (`go/meshos.go:15` "Port of the reference impl at…", and the same note in
  `go/meshdb.go:12` and `go/deck.go:13`).

So surface that exists only in the reference package isn't shipped, and
because nothing compiles that package, nothing catches it drifting from the
C ABI.

### How this was checked

- Every exported `func` / `type` name in `bindings/go/net/*.go` was grepped
  for as a definition in `go/*.go`.
- Every `C.net_*` symbol the reference files call was checked against the
  `extern "C" fn` definitions in `src/ffi/*.rs` and
  `bindings/go/*-ffi/src/lib.rs`.
- ~~`go/net.h` was diffed against `include/net.go.h`; they're identical.~~
  *Amended (R2):* that check compared only the set of declared
  `net_*(` names, and the two files' text differs (mostly comments).
  It also didn't ask whether the G2 symbols are declared at all. They aren't;
  see G2.
- The blob surface of `go/blob.go` was diffed against the Node binding
  (`bindings/node/src/blob.rs`) and the Python binding
  (`bindings/python/src/blob.rs`).
- `grep -ln Blob go/*_test.go` returns nothing.

### G1: `go/blob.go` has no tests (defect)

`go/blob.go` (557 lines) binds `MeshBlobAdapter` (`NewMeshBlobAdapter`,
`Store`, `Publish`, `Fetch`, `Exists`, `PrometheusText`, the five overflow
methods), `BlobRefHash`, and `MeshNode.ServeBlobTransfer` / `FetchBlob`. No Go
test exercises any of it. The C side is covered by Rust tests, but the cgo
glue (buffer ownership through `net_blob_free_buffer` /
`net_transport_free_buffer`, the overflow JSON round-trip, `Close` racing an
in-flight call) is not.

### G2: functions the C ABI exports that `go/` doesn't call (gap)

The C entry points exist and are listed in
`bindings/go/net-ffi/exports.baseline`. The reference package already wraps
most of them.

*Amended (R2):* the original text said these were also declared in
`go/net.h`. That was false. A per-symbol grep of `go/net.h` and its one
local include, `go/net_cortex.h`, finds none of the directory-transfer,
registry, replication, greedy/gravity or token-wait functions. Of the
canonical headers, only `include/net_transport.h` declares the four
directory-transfer functions, and it isn't in the Go include chain. The
registry, replication, greedy and token-wait functions are declared in **no**
header under `include/`. The reference package got around this with local
`extern` declarations in its cgo preambles. The placement-filter functions
are the exception: `go/net.h` declares them. So every G2 slice starts with
header work, not just Go wrappers (see "Header closure" under the design).

| Area | C symbols | Reference wrapper |
|---|---|---|
| Directory transfer | `net_store_dir`, `net_fetch_dir`, `net_dir_manifest_read`, `net_fetch_blob_discovered` | `transport.go`: `StoreDir`, `FetchDir`, `DirManifestRead`, `FetchBlobDiscovered`, `DirStats`, `TransferError` |
| Blob adapter registry | `net_blob_register_fs_adapter`, `net_blob_register_callback_adapter`, `net_blob_unregister_adapter`, `net_blob_adapter_registered`, `net_blob_publish`, `net_blob_resolve` | none |
| RedEX replication | `net_redex_enable_replication`, `net_redex_disable_replication`, `net_redex_replication_runtime_count`, `net_redex_replication_prometheus_text` | `redex.go` |
| Greedy Dataforts / data gravity | `net_redex_{enable,disable}_greedy_dataforts`, `net_redex_greedy_cached_channel_count`, `net_redex_greedy_prometheus_text`, `net_redex_{enable,disable}_gravity_for_greedy` | `redex.go`: `DataGravityConfig` and friends |
| CortEX read-your-writes | `net_tasks_wait_for_token`, `net_memories_wait_for_token` | `tasks.go` / `memories.go`: `WaitForToken`, `PollForToken`, `WaitForTokenContext` |
| Placement filters | `net_compute_{register,unregister,has}_placement_filter`, `net_compute_set_placement_filter_dispatcher` (`compute-ffi/src/lib.rs:3194–3423`) | `placement.go` |

`go/cortex.go` defines `Redex` with only `Free` and `OpenFile`; the replication
and greedy methods above are absent.

*Amended (R5):* the shipped Go `RedexFileConfig` (`go/cortex.go:142–150`) also
has no `Replication` field, although the C JSON parser accepts one
(`src/ffi/cortex.rs:995–1038`). `enable_replication` only installs an empty
router. A per-channel runtime starts when a file is opened with
`RedexFileConfig.replication` set (`src/adapter/net/redex/manager.rs:298–323`).
Without the field, a Go caller can't replicate a channel at all.

*Amended (R6):* Go CRUD returns `(uint64 seq, error)`
(`go/cortex.go:487–524`), and the adapter doesn't keep the `originHash` it was
opened with (`OpenTasks(redex, originHash, persistent)`, `:426`). The
reference package's CRUD returned `WriteToken{OriginHash, Seq}` instead, so
copying it would be a source break.

### G3: blob features the C ABI doesn't reach (gap)

Node and Python both expose these on `MeshBlobAdapter` and `BlobRef`. There is
no `extern "C"` function for any of them, so Go can't reach them yet.

| Feature | Core | Node / Python |
|---|---|---|
| Range fetch | `MeshBlobAdapter` via `BlobAdapter::fetch_range` | `fetchRange` / `fetch_range` |
| Tree store with encoding (Replicated / Reed-Solomon k,m) | `store_stream_tree` (`dataforts/blob/mesh.rs:1322`) | `storeStreamTreeFromBytes(data, encoding?)` |
| Repair | `repair_blob` (`mesh.rs:2856`) → `RepairReport` | `repairBlob` / `repair_blob` |
| Tree-node cache | `with_tree_node_cache`, `tree_node_cache_stats` | `treeNodeCacheBytes` option, `treeNodeCacheStats` |
| `BlobRef` tree accessors | `BlobRef` | `isTree`, `isChunked`, `treeRootHash`, `treeDepth`, `version`, `uri`, `size` |
| Feature tags | `DATAFORTS_BLOB_{TREE,CDC,ERASURE,BANDWIDTH_CLASS}_SUPPORTED` | exported constants |

Go handles a `BlobRef` only as opaque encoded bytes, so it can't tell a tree
ref from a flat one.

**Not a gap, recorded so nobody builds it:** Node exports `ChunkingStrategy`
and `BandwidthClass` as value types, but **no Node method takes either one**.
`store_stream_tree_from_bytes` hard-codes `ChunkingStrategy::default()`
(`bindings/node/src/blob.rs:1408`). Python is the same. Go matches that
posture: the feature-tag constants, and no chunking or bandwidth parameters
until a binding has a method that uses them.

*Amended (R9):* the four `DATAFORTS_BLOB_*_SUPPORTED` constants are
capability-advertisement **strings** (`"dataforts:blob-tree-supported"`,
`blob/blob_tree.rs:65`, and the sibling cdc/erasure/bandwidth modules), not
bits. They describe what a *peer* advertises, not what the local build
supports. Go exports them as string constants with the same values, for
parity. S6 no longer proposes a mask function.

*Amended (R3):* `Fetch` on a tree ref is deliberately unsupported
(`blob/mesh.rs:3624–3634`: "use fetch_range for Tree blobs"). Go can store a
tree only after S6, and can then read it back only through `FetchRange`.

### G4: the rest of the reference package

Reference symbols with no `go/` definition, outside blobs:
`resilience.go` (`CircuitBreaker`, `CallWithRetry`, `CallWithHedge`, pure Go),
`capability_schema.go` (`AxisSchema`, `SchemaError`, …), and parts of
`capability.go` (`DiffCapabilities`, `CapabilitySetDiff`, clause tracing),
`deck.go` (`BlastRadius`, `AvoidScope`, `Audit`, …) and `meshdb.go` (the
`Decoded*` aggregate types, builder helpers). Each must be classified as
**port** (the C ABI supports it and another binding ships it) or **drop**
(superseded, or never had a C entry point) before the package is deleted.

## The design

**Move the Go package, leave the Rust crates.** The eight `*-ffi` crates stay
under `net/crates/net/bindings/go/`. They are workspace members
(`Cargo.toml:14–23`), and `ci.yml` names their paths in about 15 places
(clippy and test matrices, the `libnet` export check at `ci.yml:4777`).
Moving them would churn CI for no gain. "Bindings" there means the Rust side
of the C ABI; the Go side lives in `go/`.

**Port, don't copy.** Nothing has compiled the reference files, so each one
is rewritten against the current `go/` conventions instead of copied in:
the `ErrBlob`-style sentinel errors, `runtime.SetFinalizer` plus an explicit
`Close`, the RW-lock handle guard that `go/cortex.go` uses so `Close` can't
free a handle another goroutine is inside, and `go/net.h` as the only header.
Each ported file gets a test file. That is what makes this more than a move.

**New C ABI for G3 follows the existing blob FFI shape.** Each function takes
the opaque `net_mesh_blob_adapter_t*`, returns `c_int` from the `NET_ERR_*`
space, and hands out buffers freed by `net_blob_free_buffer`. Structured
results (`RepairReport`, tree-cache stats, `BlobRef` fields) cross as JSON
strings freed by `net_free_string`, the same choice
`net_mesh_blob_adapter_overflow_config` already made, so the ABI has no new
C structs to version. Each function goes in `src/ffi/blob.rs` with a
`blob_stubs.rs` twin returning `NET_ERR_FEATURE_NOT_BUILT`.

*Amended (R9):* a list of names and allocators leaves the observable contract
open. S6 starts by committing a normative table to this plan, covering every
new function, before any code:

| Contract item | Rule |
|---|---|
| Widths | Lengths and offsets are `size_t` / `uint64_t`. Go never narrows a length to `C.int`; it checks against `math.MaxInt` before `C.GoBytes`. |
| Ranges | `[start, end)` half-open, `uint64_t`. `start > end` or `end > size` → the existing invalid-argument code. `start == end` → empty buffer, success. |
| Null / empty pointers | Null handle or null out-pointer → `NetError::NullPointer`. A `(NULL, 0)` input pair is allowed where the core accepts empty input; `(NULL, n>0)` → null-pointer error. |
| Failure outputs | Every out-pointer is set to `NULL` / `0` **before** the first early return. Existing blob functions are not uniform here, so this is stated for the new ones and tested, not assumed. |
| Encoding | `encoding_kind`: `0` = Replicated, `1` = Reed-Solomon. Any other value → invalid argument. For RS, `k ≥ 1`, `m ≥ 1`, `k + m ≤ 255`, computed in a widened integer. `k = m = 0` means use the core defaults (`DEFAULT_RS_K` / `DEFAULT_RS_M`). |
| JSON DTOs | Field names are the core struct's snake_case names; counters are JSON integers (`u64`). Go decodes them into `uint64` fields **without** `omitempty`. |
| Optional state | Tree-cache stats when no cache is installed → success with JSON `null`, which Go maps to `(nil, nil)`. Not an error. |
| `describe` | Variant-specific fields (`tree_root_hash`, `tree_depth`) are **absent** for a small ref, never zero placeholders. Go uses pointer fields for them. |
| Errors in Go | New codes map into the existing `ErrBlob` tree with `errors.Is` sentinels. Feature-off (`NET_ERR_FEATURE_NOT_BUILT`) gets its own sentinel. |
| Handles | Functions borrow the adapter handle under the same guard as the existing blob functions, so `Close` waits for in-flight calls. Every body runs inside the existing `adapter_guard` panic containment. |

**Header closure.** *Amended (R2).* Every slice that calls a C function
declares it in the canonical header and its Go mirror in the **same** slice,
not in S6. For each function, the slice records which canonical header owns
it (`include/net_transport.h` for directory transfer, `include/net.go.h` or a
new section of it for the rest), how it reaches cgo (`go/net.h` either
mirrors it or includes the canonical header), its error constants, and its
feature-off behavior. No local `extern` declarations in Go preambles.
Existing exported signatures don't change. Evidence: `go vet` plus a link
against the single built `libnet`, and an extension to
`go/abi_stability_*_test.go` that pins each new declaration's signature.
The export baseline changes **only** in S6, and only by adding the new
symbols. It is never regenerated wholesale to hide a missing export.

**Error mapping.** *Amended (R8).* `go/blob.go:432–479` already exports
`ErrTransfer` and its sentinels (`ErrTransferNotFound`,
`ErrTransferHashMismatch`, …) with a code mapper. Ports extend that mapper
rather than replace it. The `NET_ERR_DIR_*` codes (`src/ffi/transport.rs:82–114`)
and feature-off get new sentinels under `ErrTransfer`. The reference
`TransferError` type isn't ported, so every existing `errors.Is` check keeps
working.

**SDK-first rule.** AGENTS.md requires new user-facing mesh capability to land
in `net-mesh-sdk` first. G3 adds no capability: every feature already exists
in core and ships in Node and Python, and `sdk/src/dataforts.rs` already
covers it for Rust. The Go binding sits on the C ABI over core, as Node and
Python sit on core. The rule doesn't apply. The PR should say so.

**Alternatives rejected:**

- *Give `bindings/go/net/` its own `go.mod` and test it in CI.* That leaves two
  Go packages covering the same surface, which is the drift problem this plan
  exists to end.
- *Delete `bindings/go/net/` now and port later from git history.* It is the
  only record of how several surfaces were meant to look in Go (dir transfer,
  greedy and gravity config). Port first, delete last (S8).
- *Pass `BlobRef` as a C struct.* It versions badly, and the encoded-bytes
  form plus a JSON accessor is what the existing functions already use.

### Decision: callback blob adapters (S5)

`net_blob_register_callback_adapter` lets Go implement a `BlobAdapter`.
Rust calls back into Go on Rust-owned threads, the same shape as the MCP
consent callbacks, where the `mcp-sdk` branch review found use-after-free
and cgo thread races.

**Recommendation:** ship the filesystem adapter and `Publish` / `Resolve` in S5,
and put the callback adapter in its own slice (S5b) behind a `runtime/cgo.Handle`
registry, with a `-race` test that unregisters mid-call. If S5b slips, S5 still
stands on its own.

*Amended (R1): a handle table is not an ownership contract.* Unregister
removes only the registry's `Arc` (`blob/registry.rs:74–83`).
`net_blob_resolve` already holds a clone (`src/ffi/blob.rs:316–352`), and the
adapter clones its `Arc<OpaqueCtx>` into `spawn_blocking`, where it stays in
use through `fetch` and the later `free_buffer` callback (`:664–756`). The
vtable has no context destructor (`:509–522`), and `OpaqueCtx` gives no drop
notification (`:551–567`). So deleting the `cgo.Handle` when unregister
returns can race a later Rust→Go lookup, and never deleting it leaks.
Revised S5b design:

- **Additive owned-context registration.** A new
  `net_blob_register_callback_adapter_owned(id, vtable, ctx, release_fn)`
  leaves the existing function and the `NetBlobAdapterVtable` layout
  untouched. `release_fn(ctx)` runs exactly once, from the `Drop` of the
  **shared context** that the blocking work retains, not the outer adapter.
  A cancelled future can drop the adapter while `spawn_blocking` is still
  running.
- **Ownership on failure.** If registration fails (duplicate id, null
  pointer, feature off), ownership stays with the caller, `release_fn` is not
  called, and Go deletes the handle. On success, Rust owns the context until
  `release_fn` runs, and only then does Go call `Handle.Delete`.
- **Buffers.** Callback-produced buffers are allocated and freed by matching
  C helpers in the Go module's cgo preamble (`C.malloc` / `C.free`), never by
  the Go GC.
- **Panic containment.** `recover` wraps the entire exported trampoline,
  including the handle lookup and type assertion, not just the user method.
- **Witnesses.** Deterministic, not `-race` alone. A test-helper barrier
  holds a Rust worker (a) before the handle lookup and (b) before
  `free_buffer`. The test unregisters while it is held, releases it, and
  asserts that the callback completes against the original adapter and that
  `release_fn` fires exactly once, after the release. It also covers: a
  failed registration leaves the handle owned by Go; a cancelled resolve; and
  a same-id re-registration while an old call is held, where each call reaches
  its own adapter. `-race` runs are supporting evidence only.

S5b staying deferrable doesn't make S7's placement port safe. That has its
own generation problem (below).

### Decision: placement-filter lifetime (S7)

*Added (R1).* The reference wrapper maps a string id to a Go predicate
(`bindings/go/net/placement.go:135–148`) and deletes the map entry as soon as
Rust unregister returns (`:290–329`). A scheduler-held `CgoPlacementFilter`
holds only the id (`compute-ffi/src/lib.rs:3209–3216`) and dispatches it
later (`:3265–3280`). So an old filter can veto after removal, or call a
**new** predicate registered under the same id. The registration mutex
doesn't cover scheduler-held `Arc` clones.

**Recommendation:** bind each native bridge to a per-registration token
(an id plus a generation, or a `cgo.Handle`) through an additive
registration entry point with an owned-context release, the same pattern as
S5b. The original predicate stays alive until the bridge's last owner
releases it. Panic containment wraps the whole trampoline. If that is
too much for this track, S7 records placement as **deferred**, with this
reason in the ledger, and S8 preserves the reference API design in that row
before deleting the file. Either way, the placement port doesn't land
without the witness: old filter acquired → unregister → same-id
replacement registered → the old invocation still runs its original
predicate.

## The slices

Each slice leaves `go test ./...` green (cgo enabled, `libnet` from
`cargo build --release -p net-ffi --features net-ffi/test-helpers`).

### S1: tests for the existing `go/blob.go`

`go/blob_test.go`, on a `Redex` from `NewRedex` with a temp `persistentDir`:
- `Store` then `Fetch` round-trips, and `Exists` flips from false to true.
- `Publish(uri, data)` returns a ref that `BlobRefHash` decodes to the
  BLAKE3 of the data.
- With `Persistent: true`, a ref fetches after closing and reopening the
  `Redex` on the same directory.
- `OverflowConfig` set → get round-trips every field; an out-of-range ratio
  returns an `ErrBlob`-wrapped error.
- `PrometheusText` contains the `dataforts_blob_` prefix (the same witness as
  `tests/net_blob_cli.rs:302`).
- `Close` racing 32 `Fetch` goroutines under `-race` doesn't crash, and every
  call after `Close` returns an error.
- Two-node `ServeBlobTransfer` + `FetchBlob` on `meshHandshakePair`
  (`go/mesh_test.go:38`).

**Proves it:** the file runs green under `go test -race -run Blob`. Mutation
check: swapping `Store`'s ref and data arguments, or dropping the
`Persistent` flag from the options JSON, must turn a test red.

*Amended (R7, R4).* Corrections to the list above. S1 is tests only, so it
pins **current** behavior and adds no new policy:
- **Overflow validation.** The parser assigns any finite ratio as given
  (`src/ffi/blob.rs:952–986`), and the core setter doesn't range-check it, so
  "an out-of-range ratio returns an error" isn't true today. The test pins
  that a finite out-of-range ratio is **accepted**, and covers refusal with
  inputs that really are refused: an unknown `scope` string and malformed
  JSON. Ratio validation, if wanted, is a separate behavior change with its
  own range and compatibility note. It isn't in this plan.
- **Round-trip.** `OverflowConfig`'s numeric fields are `omitempty`, so an
  explicit zero goes over the wire as "absent" and comes back as the native
  default. The round-trip test uses non-zero values and pins the zero →
  default behavior separately.
- **Exists.** `Publish` stores the object, so "false then true" uses a known
  ref against an independent, empty adapter for the `false` half.
- **Persistence.** Close the **adapter** and then the `Redex` before
  reopening; the adapter holds its own `Arc<Redex>`.
- **Mutation target.** Persistence is a separate C `int` argument
  (`net_mesh_blob_adapter_new`, `src/ffi/blob.rs:1008–1065`), not a JSON
  field. The mutation drops that argument.
- **Race evidence.** CI's Go runner (`.github/scripts/go-test-with-native-stacks.sh`,
  `ci.yml:4815,4870`) doesn't pass `-race`, so the normal Go job is not race
  evidence. This slice adds an explicit `-race` step that runs the blob tests
  (and, from S5b on, the callback tests) through the same native-stack
  runner, so hangs still report native frames.

### S2: directory transfer (G2)

Port `transport.go` into `go/blob.go` (or `go/transfer.go`): `StoreDir`,
`FetchDir` (returns `DirStats{Files, Bytes}`), `DirManifestRead` (decodes the
JSON), `FetchBlobDiscovered`, and `TransferError` mapping the
`NET_ERR_TRANSFER_*` codes.

**Proves it:** a two-node test stores a 3-file tree with a nested
subdirectory, fetches it into a temp dir, and byte-compares every file.
`DirStats` matches the file count and byte total. `FetchBlobDiscovered` finds
a blob without a holder id. A missing manifest returns `TransferError`, not a
panic.

*Amended (R2, R8).*
- **Header.** Declare the four functions for cgo from
  `include/net_transport.h`, either by mirroring it as `go/net_transport.h`
  and including it from `go/net.h`, or by moving the declarations. Whichever
  is chosen, the ABI-stability test pins the signatures.
- **No `TransferError` type.** Extend the existing `ErrTransfer` mapper with
  `NET_ERR_DIR_*` and feature-off sentinels (see "Error mapping").
  "A missing manifest returns `TransferError`" becomes
  `errors.Is(err, ErrDirInvalidManifest)` (name final in the slice) **and**
  `errors.Is(err, ErrTransfer)`.
- **The JSON boundary gets its own witness.** The test calls
  `DirManifestRead` directly and asserts the decoded entries: paths, sizes,
  the externally tagged entry kind (file vs. dir), and that each embedded
  encoded ref decodes with `BlobRefHash`. A byte-compare after `FetchDir`
  never touches that JSON.
- **Allocators.** Byte outputs are freed with `net_transport_free_buffer`;
  the manifest JSON is freed with `net_free_string`. A Go test helper counts
  the frees.
- **Fixture.** Both peers install the transfer engine (`ServeBlobTransfer`).
  `FetchBlobDiscovered` keeps the exact 32-byte hash check: 31- and 33-byte
  hashes are refused in Go before the cgo call.

### S3: RedEX replication and greedy Dataforts (G2)

Methods on `go/cortex.go`'s `Redex`: `EnableReplication` /
`DisableReplication` / `ReplicationRuntimeCount` /
`ReplicationPrometheusText`, and `EnableGreedyDataforts` /
`DisableGreedyDataforts` / `GreedyCachedChannelCount` /
`GreedyPrometheusText` / `EnableGravityForGreedy(DataGravityConfig)` /
`DisableGravityForGreedy`. The Node SDK's N8 slice in
[`NODE_SDK_GAPS_PLAN.md`](NODE_SDK_GAPS_PLAN.md) covers the same surface
(less `DisableReplication`, which has a C entry point but no Node method);
match its names and defaults.

**Proves it:** enable → runtime count goes to 1, disable → back to 0, and the
Prometheus text names the channel. Calling enable twice is idempotent or a
typed error, whichever the C side does; the test pins that. Gravity
config round-trips.

*Amended (R5): that witness was invalid.* `enable_replication` installs an
**empty** router. `replication_runtime_count` counts router entries, not an
enabled flag (`redex/manager.rs:541–549`). The count reaches 1 only once a
file is opened with a replication config, and Go can't express that today.
Revised scope and witness:
- **Scope adds** a `Replication *RedexReplicationConfig` field on
  `RedexFileConfig`, forwarded through `OpenFile` to the JSON the C parser
  already accepts (`src/ffi/cortex.rs:995–1038`). The DTO mirrors that
  parser's keys exactly. A nil pointer leaves the key out, which is the
  current behavior.
- **Header.** Declare the replication, greedy and gravity functions in the
  canonical header and `go/net_cortex.h` (none are declared anywhere today).
- **Ownership.** `EnableReplication`, `EnableGreedyDataforts` and
  `EnableGravityForGreedy` take a `net_mesh_arc_clone` box, which C
  **consumes on every return code** (`src/ffi/cortex.rs:459–499`). Go clones
  it immediately before the call and never frees it afterward, including on
  error. A test helper counts live Arcs, and the error-path test asserts no
  double free and no leak.
- **Witness.** Disabled → count 0. Enable → still 0 (empty router). Open a
  file with a replication config → 1. Append on node A, then bounded
  (≤ 10 s) reads on node B return the same events: replication is proven
  by data arriving, not by the metric. Disable → count drains to 0.
  Calling enable twice pins the documented idempotence.
- **Gravity.** There is no C getter, so "config round-trips" was
  unobservable. The witness becomes: invalid configs are refused at the C
  boundary (pinning whichever validation the C side does), a valid config
  takes effect (greedy cached-channel count and Prometheus text change as
  documented), and disable reverts it. If a getter turns out to be needed,
  it's recorded here as a missing accessor for S6, not assumed.

### S4: CortEX read-your-writes (G2)

`TasksAdapter.WaitForToken` / `MemoriesAdapter.WaitForToken`, plus the
`context.Context` variant from the reference package.

**Proves it:** a write's token, waited on immediately, returns, and the
following `List` includes the write. An already-cancelled context returns
`context.Canceled` without blocking. A token from a different adapter is
rejected.

*Amended (R6).* **Decision: additive, no source break (recommended).** The
existing CRUD signatures stay `(uint64, error)`. Adapters keep the
`originHash` they were opened with and gain:
- `OriginHash() uint64`
- `Token(seq uint64) WriteToken`, where `WriteToken{OriginHash, Seq}` is a
  new exported struct
- `WaitForToken(tok, timeout)` and `WaitForTokenContext(ctx, tok)`

A caller writes `seq, _ := t.Create(...)` and then
`t.WaitForToken(t.Token(seq), d)`. Changing the CRUD signatures to return
tokens, as the reference package did, would need explicit authorization as
a breaking change. This plan doesn't take it.

Semantics to pin, each with a test:
- **Zero timeout.** In C, `timeout_ms == 0` means poll once
  (`src/ffi/cortex.rs:1830`), but Go `WaitForSeq(…, 0)` means wait
  indefinitely. `WaitForToken` documents and tests its own zero: zero
  polls, matching C. Waiting without a deadline goes through the context
  variant.
- **Errors.** `NET_ERR_TIMEOUT`, `NET_ERR_WRONG_ORIGIN`, `NET_ERR_QUEUE_FULL`
  and `NET_ERR_FOLD_STOPPED` each map to a distinct `errors.Is` sentinel.
- **Wrong origin.** C checks the **origin**, not adapter identity. The
  negative witness uses a token with a **different origin hash**; a second
  adapter opened with the same origin is expected to succeed, and the test
  pins that too.
- **Cancellation.** `WaitForTokenContext` checks `ctx.Err()` **before** the
  first poll (the reference loop polled first), so a pre-cancelled context
  returns `context.Canceled` even when the token is already satisfied.
  Polling slices are bounded, so cancellation is noticed within one slice
  (≤ 50 ms), and the test asserts that bound.
- **Header.** Declare both wait functions in `go/net_cortex.h` and its
  canonical source.

### S5: blob adapter registry: filesystem + publish/resolve (G2)

`RegisterFilesystemBlobAdapter(id, root)`, `UnregisterBlobAdapter`,
`BlobAdapterRegistered`, `BlobPublish(adapterID, uri, data)`,
`BlobResolve(ref)`.

**Proves it:** publish through a filesystem adapter writes under `root`, and
resolve returns the bytes. After unregister, resolve fails with a typed error.
Registering a duplicate id fails.

*Amended (R8, R2).* `net_blob_resolve` takes an explicit adapter id
(`src/ffi/blob.rs:316–352`). The Go API is `BlobResolve(adapterID, ref)`,
not a global `BlobResolve(ref)`; no discovery mechanism is added. All six
registry functions get declarations in the canonical header and `go/net.h`
in this slice. S5 doesn't depend on S5b.

### S5b: callback blob adapters

`RegisterBlobAdapter(id, BlobAdapter)`: a Go interface with `Store`, `Fetch`,
`FetchRange`, `Exists`, dispatched through a `cgo.Handle` table.

**Proves it:** a Go map-backed adapter round-trips through `BlobPublish` /
`BlobResolve`. A Go adapter that panics surfaces as an error, not a process
abort (reuse `callback_recover.go`). Unregistering during 16 concurrent
resolves under `-race` is clean.

*Amended (R1):* superseded by the owned-context design under "Decision:
callback blob adapters". S5b now adds one additive C entry point
(`net_blob_register_callback_adapter_owned`). It's the only new export
outside S6, and it lands with its own additive export-baseline update. The
deterministic barrier witnesses listed there are the proof; the `-race` run
supports them.

### S6: new C ABI for tree, erasure, range and repair (G3)

Rust (`src/ffi/blob.rs` and stubs), with `include/net.go.h` and `go/net.h`
mirrored in the same commit:

- `net_mesh_blob_adapter_fetch_range(h, ref, ref_len, start, end, out, out_len)`
- `net_mesh_blob_adapter_store_tree(h, data, len, encoding_kind, rs_k, rs_m, out_ref, out_ref_len)`
- `net_mesh_blob_adapter_repair_blob(h, ref, ref_len, out_json)`
- `net_mesh_blob_adapter_tree_node_cache_stats(h, out_json)`
- `net_blob_ref_describe(ref, ref_len, out_json)`: version, uri, hash, size,
  is_tree, is_chunked, tree_root_hash, tree_depth
- `net_blob_feature_tags(out_mask)`: the four `DATAFORTS_BLOB_*_SUPPORTED` bits
- `tree_node_cache_bytes` added to `net_mesh_blob_adapter_new`'s options JSON
  (an additive key, so no signature change)

Go: `FetchRange`, `StoreTree(data, Encoding)`, `RepairBlob → RepairReport`,
`TreeNodeCacheStats`, `DescribeBlobRef → BlobRefInfo`, feature-tag consts,
`MeshBlobAdapterOpts.TreeNodeCacheBytes`.

**Proves it:**
- A Rust unit test per C function in `src/ffi/blob.rs`.
- `bindings/go/net-ffi/exports.baseline` regenerated, with the intent stated
  in the commit message (`.github/scripts/check-ffi-exports.py`).
- `go/abi_stability_*_test.go` extended to the new symbols.
- Go tests: a 3 MiB blob stored with `ReedSolomon{4,2}` describes as a tree,
  `FetchRange(1<<20, 1<<20+4096)` equals the source slice, `RepairBlob`
  after deleting one chunk reports one repaired chunk and the blob then
  fetches, and a `start > end` range is a typed error.
- A cross-language check: a `BlobRef` encoded by Go decodes in the Python
  test suite with identical `describe` fields (extend `tests/cross_lang_*`
  if a blob fixture exists, otherwise add one).

*Amended (R3, R4, R9).* Corrections to the S6 list above:
- **No `net_blob_feature_tags(out_mask)`.** The tags are strings (see G3), and
  Go exports them as string constants. No mask is defined. A local
  runtime-capability mask, if ever needed, gets its own names, bit
  assignments and width, and must not suggest anything about remote-peer
  support.
- **Tree-node cache option.** `net_mesh_blob_adapter_new` takes `persistent`
  as a C `int` plus an **overflow-only** JSON (`src/ffi/blob.rs:1008–1065`),
  and Go marshals only `opts.Overflow` (`go/blob.go:148–165`). There is no
  general options JSON to add a key to. **Decision (recommended): an
  additive constructor**,
  `net_mesh_blob_adapter_new_v2(redex, id, persistent, options_json)`, whose
  JSON has `overflow` and `tree_node_cache_bytes` as top-level keys.
  The legacy constructor stays as it is. An absent key means no cache. An
  explicit `0` installs a zero-capacity cache; the core treats these as
  distinct states, and Go keeps them distinct with
  `TreeNodeCacheBytes *uint64`. The rejected alternative was to overload
  the overflow JSON with a constructor-specific key: it's compatible, but it
  turns an overflow config parser into a general options parser.
  Witnesses: cache enabled with nil overflow; cache omitted (stats → `nil`);
  explicit zero (stats present, capacity 0); and legacy overflow-only
  construction unchanged.
- **Repair witness, corrected.** RS closes a stripe only at `k` full chunks,
  and an incomplete trailing stripe is stored Replicated with no parity
  (`blob/erasure.rs:493–543`). Default chunks are 4 MiB
  (`blob/blob_tree.rs:155–160`, `blob/blob_ref.rs:131`), so 3 MiB under
  `ReedSolomon{4,2}` has **no** parity to repair. The fixture is at least
  16 MiB (16,777,216 bytes, four full chunks), each chunk with distinct
  content so content addressing can't collapse them. The test identifies a
  real **data** shard and makes it unavailable through a narrowly gated
  test seam (a `test-helpers`-only C function, or a
  shutdown → delete → reopen fixture that also clears RedEX/cache state).
  Deleting a file while live state can still serve the shard proves nothing.
  It then proves the shard is unavailable **before** repair. No production
  deletion authority is added to make the test convenient. Assertions:
  `chunks_restored == 1` and `stripes_repaired == 1`. A second `RepairBlob`
  restores nothing. `FetchRange(ref, 0, size)` equals the source; plain
  `Fetch` is unsupported for tree refs. Because an unrecoverable stripe is
  reported in the counters rather than always as an error
  (`blob/mesh.rs:3197–3229`), Go documents that a nil error doesn't mean a
  complete repair, and a test with more than `m` shards lost pins that.
- **Cross-language fixture.** Core's describe fields are optional per
  variant, and Python's getters fall back to zero values, so the two aren't
  automatically identical JSON. The fixture freezes a **normalized** form
  (absent fields omitted, hashes as lowercase hex). Python changes are
  **test-only**: fixture validation in the Python suite, with no change to
  the Python binding's production API, which stays in "Not in scope".
- **Export baseline.** Only the new symbols are added, with the reason in the
  commit message. The baseline isn't regenerated wholesale.

### S7: classify and port the rest of the reference package (G4)

One table in this plan, filled in when the slice lands, with a row per
reference file: port / drop / already ported, and the reason. Ports land with
tests. `resilience.go` is pure Go and needs no FFI. `placement.go` needs the
cgo dispatcher, the same handle care as S5b.

**Proves it:** the table is complete, and the S1–S6 grep (every exported
reference symbol has a `go/` definition or a "drop" row) comes back empty.

*Amended (R1, review notes).*
- **Ledger granularity.** The ledger accounts for methods under their
  receiver types and public names (`(*CircuitBreaker).Call`, not `Call`).
  The original name-only grep missed same-named methods on different
  receivers.
- **Preserve before deleting.** Every "drop" or "deferred" row keeps the
  relevant API design (signatures and the doc-comment intent) in the ledger
  itself, because S8 deletes the only copy.
- **Placement is gated** on the lifetime design under "Decision:
  placement-filter lifetime", or on a recorded, justified deferral. S5b's
  status doesn't decide it either way.

### S8: delete `bindings/go/net/`

Remove the directory. Repoint the comments in `go/*.go` that cite it
(`go/deck.go:13`, `go/meshdb.go:12`, `go/meshos.go:15`,
`go/meshos_test.go:128,144`) and the stale paths in
[`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) with an amendment note,
not a rewrite. Update `go/README.md` to list blobs and directory transfer.

**Proves it:** `grep -rn "bindings/go/net/" --include=*.go --include=*.md`
returns only historical plan text. `go test ./...` and the CI Go job are
green.

*Amended (review notes):* user-facing docs move with the surface, not only
the deleted paths. That covers the Go section of the docs site
(`web/src/content/docs/`), Go snippets in the `net-event-bus` skill's
Dataforts material, any capability or support matrices that list Go blob
support, and the release notes for the shipping version (mirrored with
`npm run sync:releases`).

## Risks

- **The reference code is wrong against today's ABI.** It has never been
  compiled in CI. *Fallback:* port means rewrite against `go/net.h`; the C
  signature wins every disagreement, and S7 records each one.
- **Callback adapters reintroduce the cgo races the MCP binding had.**
  *Fallback:* S5b is separate and optional; S5 doesn't depend on it.
- **Windows dev box vs Linux CI.** cgo runs locally (WinLibs gcc, `net.dll`
  on PATH), but `exports.baseline` is generated from the Windows PE artifact
  and CI checks `libnet.so`. *Fallback:* regenerate on the platform the
  checker reads, and treat a CI export-check failure as a baseline
  regeneration issue first.
  *Amended (review notes): that fallback was wrong.* The checker
  (`.github/scripts/check-ffi-exports.py`) compares one platform-neutral
  name set. A Linux mismatch is investigated as an unexpected missing or
  extra export, and only the intended additive set is updated, with the
  reason stated. Regenerating is never the first response.
- **The disk fills mid-build.** S6 rebuilds `net-ffi` with its full feature
  set. *Fallback:* one target directory, check free space before the gates.
- **The RS repair test is slow or flaky** on a loopback two-node mesh.
  *Fallback:* run repair on a single node (delete a local chunk file), which
  is what the Rust `repair_blob` tests do.
  *Amended (R3):* deleting a local file isn't enough while live RedEX or
  cache state can still serve the shard. The single-node fallback uses the
  gated test seam or the shutdown → delete → reopen fixture from S6, which is
  what the Rust precedent does through the adapter's deletion path
  (`blob/mesh.rs:1269–1291`, `:6590–6628`).
- **Callback lifetime needs new native ownership** (R1). *Fallback:* S5b and
  the placement port are both deferrable with a ledger entry. S1–S5 and S6 don't
  depend on either.

## Not in scope

- Moving or renaming the Rust `*-ffi` crates.
- Chunking-strategy or bandwidth-class parameters on any Go method (see G3).
  Go gets them when a binding first has a method that takes them.
- Streaming blob store/fetch (`io.Reader` / `io.Writer` over the C ABI). This
  plan is byte-slice in, byte-slice out, as the existing functions are.
- Blob GC, pin / unpin, and auth-guarded operations (`pin_authorized`,
  `delete_chunk_authorized`). Node and Python don't expose them either.
- Any change to the Node or Python bindings. *Amended (R9):* test-only
  fixture validation in the Python suite (S6) is allowed; production APIs
  stay unchanged.
- Overflow ratio validation (R7). It would be a behavior change, with its own
  plan entry if wanted.
- Changing Go CRUD signatures to return tokens (R6). That needs explicit
  authorization as a breaking change.
- Making `Fetch` accept tree refs (R3). That's a core behavior change.

## Review amendments, 2026-10-03

Review at `d1b6f29`. Verdict: hold for implementation authorization, with
the consolidation direction accepted. Each finding was re-checked against
source before this amendment.

| # | Severity | Finding | Where it changed the plan |
|---|---|---|---|
| R1 | High | A `cgo.Handle` table can't know when Rust drops its last reference to a callback context; the placement bridge dispatches by a reusable string id | Callback decision rewritten (owned context, `release_fn` once from the shared context's drop, barrier witnesses); new placement-lifetime decision; S5b, S7 |
| R2 | Medium | The G2 functions aren't declared in the shipped Go headers; `go/net.h` ≠ `net.go.h` textually | "How this was checked" corrected; G2; "Header closure" moved into each slice |
| R3 | Medium | 3 MiB under RS{4,2} has no parity; deleting a file doesn't remove a shard; `Fetch` refuses tree refs | G3; S6 repair witness (≥ 16 MiB, gated seam, `FetchRange`, counters); Risks |
| R4 | Medium | The constructor has no general options JSON; persistence is a C `int` | S1 mutation target; S6 additive constructor with absent vs. zero cache |
| R5 | Medium | Enabling replication installs an empty router; Go `RedexFileConfig` has no replication field; the Arc is consumed on every return | G2; S3 scope, ownership and data-arrival witness; gravity witness without a getter |
| R6 | Medium | No way to get a bound token without a source break; C checks origin, not identity; zero timeout means poll | G2; S4 additive token decision and pinned semantics |
| R7 | Medium | Ratio validation doesn't exist; `omitempty` zeros; persistence and `Exists` fixtures | S1 corrections; Not in scope |
| R8 | Medium | Resolve needs an adapter id; `ErrTransfer` already exists; `NET_ERR_DIR_*`; allocators | "Error mapping"; S2; S5 |
| R9 | Medium | The new ABI's observable contract was unspecified; feature tags are strings, not bits | Normative contract table; G3; S6; Not in scope |

Review notes also adopted: an explicit `-race` CI step through the
native-stack runner (S1); export-baseline discipline (design, S6, Risks);
a receiver-qualified S7 ledger that preserves designs before deletion; and
user-facing docs and release notes in S8.
