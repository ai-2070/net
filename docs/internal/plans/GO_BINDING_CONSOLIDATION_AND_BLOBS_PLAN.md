# Go binding — fold the reference package into `go/`, finish Dataforts blobs

## Status

Planned, 2026-10-03. Targets the release after 0.39. Branch `LZL0/go-blobs`.
No slice has landed.

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
- `go/net.h` was diffed against `include/net.go.h`; they're identical.
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

The C entry points exist, are in `go/net.h`, and are listed in
`bindings/go/net-ffi/exports.baseline`. The reference package already wraps
most of them.

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
consent callbacks, where the earlier `mcp-sdk` review found use-after-free
and cgo thread races (memory: mcp-sdk Go binding review).

**Recommendation:** ship the filesystem adapter and `Publish` / `Resolve` in S5,
and put the callback adapter in its own slice (S5b) behind a `runtime/cgo.Handle`
registry, with a `-race` test that unregisters mid-call. If S5b slips, S5 still
stands on its own.

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

### S4: CortEX read-your-writes (G2)

`TasksAdapter.WaitForToken` / `MemoriesAdapter.WaitForToken`, plus the
`context.Context` variant from the reference package.

**Proves it:** a write's token, waited on immediately, returns, and the
following `List` includes the write. An already-cancelled context returns
`context.Canceled` without blocking. A token from a different adapter is
rejected.

### S5: blob adapter registry: filesystem + publish/resolve (G2)

`RegisterFilesystemBlobAdapter(id, root)`, `UnregisterBlobAdapter`,
`BlobAdapterRegistered`, `BlobPublish(adapterID, uri, data)`,
`BlobResolve(ref)`.

**Proves it:** publish through a filesystem adapter writes under `root`, and
resolve returns the bytes. After unregister, resolve fails with a typed error.
Registering a duplicate id fails.

### S5b: callback blob adapters

`RegisterBlobAdapter(id, BlobAdapter)`: a Go interface with `Store`, `Fetch`,
`FetchRange`, `Exists`, dispatched through a `cgo.Handle` table.

**Proves it:** a Go map-backed adapter round-trips through `BlobPublish` /
`BlobResolve`. A Go adapter that panics surfaces as an error, not a process
abort (reuse `callback_recover.go`). Unregistering during 16 concurrent
resolves under `-race` is clean.

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

### S7: classify and port the rest of the reference package (G4)

One table in this plan, filled in when the slice lands, with a row per
reference file: port / drop / already ported, and the reason. Ports land with
tests. `resilience.go` is pure Go and needs no FFI. `placement.go` needs the
cgo dispatcher, the same handle care as S5b.

**Proves it:** the table is complete, and the S1–S6 grep (every exported
reference symbol has a `go/` definition or a "drop" row) comes back empty.

### S8: delete `bindings/go/net/`

Remove the directory. Repoint the comments in `go/*.go` that cite it
(`go/deck.go:13`, `go/meshdb.go:12`, `go/meshos.go:15`,
`go/meshos_test.go:128,144`) and the stale paths in
[`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) with an amendment note,
not a rewrite. Update `go/README.md` to list blobs and directory transfer.

**Proves it:** `grep -rn "bindings/go/net/" --include=*.go --include=*.md`
returns only historical plan text. `go test ./...` and the CI Go job are
green.

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
- **The disk fills mid-build.** S6 rebuilds `net-ffi` with its full feature
  set. *Fallback:* one target directory, check free space before the gates.
- **The RS repair test is slow or flaky** on a loopback two-node mesh.
  *Fallback:* run repair on a single node (delete a local chunk file), which
  is what the Rust `repair_blob` tests do.

## Not in scope

- Moving or renaming the Rust `*-ffi` crates.
- Chunking-strategy or bandwidth-class parameters on any Go method (see G3).
  Go gets them when a binding first has a method that takes them.
- Streaming blob store/fetch (`io.Reader` / `io.Writer` over the C ABI). This
  plan is byte-slice in, byte-slice out, as the existing functions are.
- Blob GC, pin / unpin, and auth-guarded operations (`pin_authorized`,
  `delete_chunk_authorized`). Node and Python don't expose them either.
- Any change to the Node or Python bindings.
