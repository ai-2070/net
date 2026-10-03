# Go binding — fold the reference package into `go/`, finish Dataforts blobs

## Status

In progress, 2026-10-03. Targets the release after 0.39. Branch
`LZL0/go-blobs`. Plan accepted for implementation at `e33ede5`. **S1–S5
done 2026-10-03** (S1 `aaa797c`, S2 `bb775f6`, S3 `3773b36`, S4 `ab12cff`;
evidence under each slice). CI: S1's head ran green on every Go job, including the new
`-race` step and its roster. Its one failure was the Firefox browser
witness `stage5_a_refused_connect_closes_rtc_and_hands_back_its_attempt`
(wasm leaf), which no Go change reaches and which also fails
intermittently on `master` (run 36970267483). S5b deferred (see S5b); S6–S8 not started.

Reviewed twice before implementation (`d1b6f29`, then `317168c`). The first
review accepted the direction and held on nine findings (R1–R9). The second
accepted those corrections and held on four contract and witness issues
(Q1–Q4) plus consistency cleanup. Both are applied. Following that review's
request, the slice lists below show **only the adopted versions**. What each
review changed, and why, is kept in [Review history](#review-history) at
the end, so the reasoning isn't lost.

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
  for as a definition in `go/*.go`. (Name-only. S7's ledger redoes this per
  receiver type.)
- Every `C.net_*` symbol the reference files call was checked against the
  `extern "C" fn` definitions in `src/ffi/*.rs` and
  `bindings/go/*-ffi/src/lib.rs`.
- Each G2 symbol was grepped for **as a declaration** in `go/net.h`, its one
  local include `go/net_cortex.h`, and every header in `include/`.
  (`go/net.h` and `include/net.go.h` declare the same function names, but
  their text differs, mostly in comments.)
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

Its three buffer copies pass `C.int(outLen)` to `C.GoBytes`
(`go/blob.go:265,291,556`). `C.GoBytes` takes a `C.int`, so a `size_t` length
over 2³¹−1 wraps negative on amd64. The same pattern appears at 21 more
`C.GoBytes` sites across `go/` (cortex, mesh_rpc, tool, …). This plan fixes
the blob sites and records the rest as a defect (see "Defects found on the
way").

### G2: C ABI functions that `go/` neither declares nor calls (gap)

The C entry points exist and are listed in
`bindings/go/net-ffi/exports.baseline`. **None of them is declared in the
headers the Go module compiles against.** `include/net_transport.h` declares
the four directory-transfer functions, but it isn't in the Go include chain.
The registry, replication, greedy/gravity and token-wait functions are
declared in no header under `include/` at all. The reference package got
around this with local `extern` declarations in its cgo preambles. The one
exception is the placement-filter functions, which `go/net.h` does declare.

| Area | C symbols | Reference wrapper |
|---|---|---|
| Directory transfer | `net_store_dir`, `net_fetch_dir`, `net_dir_manifest_read`, `net_fetch_blob_discovered` | `transport.go` |
| Blob adapter registry | `net_blob_register_fs_adapter`, `net_blob_register_callback_adapter`, `net_blob_unregister_adapter`, `net_blob_adapter_registered`, `net_blob_publish`, `net_blob_resolve` | none |
| RedEX replication | `net_redex_enable_replication`, `net_redex_disable_replication`, `net_redex_replication_runtime_count`, `net_redex_replication_prometheus_text` | `redex.go` |
| Greedy Dataforts / data gravity | `net_redex_{enable,disable}_greedy_dataforts`, `net_redex_greedy_cached_channel_count`, `net_redex_greedy_prometheus_text`, `net_redex_{enable,disable}_gravity_for_greedy` | `redex.go` |
| CortEX read-your-writes | `net_tasks_wait_for_token`, `net_memories_wait_for_token` | `tasks.go` / `memories.go` |
| Placement filters | `net_compute_{register,unregister,has}_placement_filter`, `net_compute_set_placement_filter_dispatcher` (`compute-ffi/src/lib.rs:3194–3423`) | `placement.go` |

Related gaps in the shipped Go types:

- `go/cortex.go`'s `Redex` has only `Free` and `OpenFile`.
- `RedexFileConfig` (`go/cortex.go:142–150`) has no `Replication` field,
  although the C JSON parser accepts one (`src/ffi/cortex.rs:995–1038`).
  `enable_replication` only installs an empty router; a per-channel runtime
  starts when a file is opened with a replication config
  (`src/adapter/net/redex/manager.rs:298–323`). Without the field, a Go
  caller can't replicate a channel at all.
- Go CRUD returns `(uint64 seq, error)` (`go/cortex.go:487–524`), and the
  adapter doesn't keep the `originHash` it was opened with
  (`OpenTasks`, `:426`). The reference CRUD returned
  `WriteToken{OriginHash, Seq}`, so copying it would be a source break.

### G3: blob features the C ABI doesn't reach (gap)

Node and Python both expose these on `MeshBlobAdapter` and `BlobRef`. There is
no `extern "C"` function for any of them.

| Feature | Core | Node / Python |
|---|---|---|
| Range fetch | `fetch_range` (`dataforts/blob/mesh.rs:3663–3708`) | `fetchRange` / `fetch_range` |
| Tree store with encoding | `store_stream_tree` (`mesh.rs:1322`) | `storeStreamTreeFromBytes(data, encoding?)` |
| Repair | `repair_blob` (`mesh.rs:2856`) → `RepairReport` | `repairBlob` / `repair_blob` |
| Tree-node cache | `with_tree_node_cache`, `tree_node_cache_stats` | `treeNodeCacheBytes` option, `treeNodeCacheStats` |
| `BlobRef` accessors | `BlobRef` | `isTree`, `isChunked`, `treeRootHash`, `treeDepth`, `version`, `uri`, `size` |
| Feature tags | `DATAFORTS_BLOB_{TREE,CDC,ERASURE,BANDWIDTH_CLASS}_SUPPORTED` | exported constants |

Go handles a `BlobRef` only as opaque encoded bytes. `Fetch` on a tree ref is
deliberately unsupported in core (`mesh.rs:3624–3634`, "use fetch_range for
Tree blobs"), so once Go can store a tree, `FetchRange` is the only way to
read it back.

The feature tags are capability-advertisement **strings**
(`"dataforts:blob-tree-supported"`, `blob/blob_tree.rs:65`, and the sibling
cdc/erasure/bandwidth modules). They describe what a peer advertises, not
what the local build supports.

**Not a gap, recorded so nobody builds it:** Node exports `ChunkingStrategy`
and `BandwidthClass` as value types, but **no Node method takes either one**.
`store_stream_tree_from_bytes` hard-codes `ChunkingStrategy::default()`
(`bindings/node/src/blob.rs:1408`). Python is the same. Go matches that: the
tag strings, and no chunking or bandwidth parameters.

### G4: the rest of the reference package

Reference symbols with no `go/` definition, outside blobs:
`resilience.go` (`CircuitBreaker`, `CallWithRetry`, `CallWithHedge`, pure Go),
`capability_schema.go` (`AxisSchema`, `SchemaError`, …), and parts of
`capability.go` (`DiffCapabilities`, `CapabilitySetDiff`, clause tracing),
`deck.go` (`BlastRadius`, `AvoidScope`, `Audit`, …) and `meshdb.go` (the
`Decoded*` aggregate types, builder helpers). Each is classified as **port**,
**drop** or **deferred** before the package is deleted.

## The design

**Move the Go package, leave the Rust crates.** The eight `*-ffi` crates stay
under `net/crates/net/bindings/go/`. They are workspace members
(`Cargo.toml:14–23`), and `ci.yml` names their paths in about 15 places
(clippy and test matrices, the `libnet` export check at `ci.yml:4777`).
"Bindings" there means the Rust side of the C ABI; the Go side lives in `go/`.

**Port, don't copy.** Nothing has compiled the reference files, so each one
is rewritten against current `go/` conventions instead of copied in:
`errors.Is` sentinel trees, `runtime.SetFinalizer` plus an explicit `Close`,
the RW-lock handle guard `go/cortex.go` uses so `Close` can't free a handle
another goroutine is inside, and no local `extern` declarations. Each ported
file gets a test file.

**SDK-first rule.** AGENTS.md requires new user-facing mesh capability to land
in `net-mesh-sdk` first. This plan adds no capability: every feature already
exists in core and ships in Node and Python, and `sdk/src/dataforts.rs`
already covers it for Rust. The rule doesn't apply. The PR says so.

### Header closure and the export baseline

Every slice that calls a C function declares it in the canonical header and
in the Go include chain **in that slice**. For each function, the slice
records which canonical header owns it (`include/net_transport.h` for
directory transfer, `include/net.go.h` for the rest), how it reaches cgo
(`go/net.h` either mirrors it or includes the canonical header), its error
constants, and its feature-off behavior. Existing exported signatures don't
change. Evidence: a link against the single built `libnet`, plus an
extension to `go/abi_stability_*_test.go` that pins each new declaration's
signature.

**Every slice that adds an exported symbol adds exactly that symbol to
`bindings/go/net-ffi/exports.baseline` in the same commit**, with the reason
in the commit message. That covers S5b's owned registration, S6's blob
functions, and S7's placement registration if it isn't deferred. The baseline
is never regenerated wholesale. The checker
(`.github/scripts/check-ffi-exports.py`) compares one platform-neutral name
set, so an unexpected Linux mismatch is investigated as a missing or extra
export, never answered by regenerating.

### Buffers and lengths

`C.GoBytes(unsafe.Pointer, C.int)` truncates any length over 2³¹−1, and
checking against `math.MaxInt` doesn't prevent that, because Go `int` is
wider than `C.int` on 64-bit targets. **Decision: an int-sized checked copy,
never `C.GoBytes`, in new and ported code.**

```go
// checkedLen converts a C length to a Go int, or fails. Pure: testable at
// the boundary without allocating.
func checkedLen(n C.size_t) (int, error)
// copyCBuf copies n bytes out of a C buffer. (nil, 0) → empty slice;
// (nil, n>0) → error. Does not free; the caller frees with the matching
// allocator.
func copyCBuf(p unsafe.Pointer, n C.size_t) ([]byte, error)
```

`copyCBuf` uses `unsafe.Slice((*byte)(p), n)` plus `copy` into a Go slice, so
the length never passes through `C.int`. The supported size is whatever
`checkedLen` accepts (`n ≤ math.MaxInt`), and core's own per-call limits stay
in force. For example, `MAX_FETCH_RANGE_BYTES` is 1 GiB (`mesh.rs:113`).
Pointer/length consistency and ownership (which allocator frees the buffer)
are checked separately from the length. Boundary tests call `checkedLen`
directly with 2³¹−1, 2³¹, 2³²+1 and `math.MaxInt`+1 (where representable), so
none of them allocates. S1 adds the helpers and moves the three blob copy
sites onto them.

### Error mapping

`go/blob.go:432–479` already exports `ErrTransfer` and its sentinels, with a
code mapper covering −200…−209. Ports **extend** that mapper rather than
replace it, so every existing `errors.Is` check keeps working. The reference
`TransferError` type isn't ported. New sentinels (exact names fixed in the
slice):

- `NET_ERR_DIR_INVALID_MANIFEST` (−210), `NET_ERR_DIR_PATH_INVALID` (−211)
  and `NET_ERR_DIR_IO` (−213): new sentinels under `ErrTransfer`.
- `NET_ERR_FEATURE_NOT_BUILT`: one sentinel shared by every feature-off stub.
- The blob-registry codes (`NET_ERR_BLOB_*`, −110…−120 in `src/ffi/blob.rs`):
  sentinels under `ErrBlob`.

The native mapping is never changed to make a witness pass. In particular,
`net_dir_manifest_read` returns the **fetch** error (`blob_err_code`, so a
missing blob is `NET_ERR_TRANSFER_NOT_FOUND`) and returns
`NET_ERR_DIR_INVALID_MANIFEST` only for bytes that were fetched but don't
decode (`src/ffi/transport.rs:429–440`). Both classifications are preserved
and tested separately (S2).

### New C ABI contract (S6)

Each new function takes the opaque `net_mesh_blob_adapter_t*` (or encoded ref
bytes), returns `c_int`, and has a `blob_stubs.rs` twin returning
`NET_ERR_FEATURE_NOT_BUILT`. Structured results cross as JSON strings freed
with `net_free_string`, the choice `net_mesh_blob_adapter_overflow_config`
already made, so there are no new C structs to version. Byte results are
freed with `net_blob_free_buffer`.

**Normative rules for every S6 function:**

| Item | Rule |
|---|---|
| Widths | Lengths: `size_t`. Offsets and ranges: `uint64_t`. `encoding_kind`: `uint8_t`. `rs_k`, `rs_m`: `uint8_t`. Cache capacity: `uint64_t`. |
| Output storage | Required out-pointers are null-checked **before** any dereference. Null → `NetError::NullPointer` (−1), and nothing is written through any pointer. After that check, every out slot is initialized (`NULL` / `0`) before any later failure can return. If one pointer of a pair (`out_ptr`, `out_len`) is null, that's a null-pointer error and **neither** slot is written. Tested per function, with each pointer nulled in turn. |
| Input pairs | `(NULL, 0)` is an empty input where core accepts empty input. `(NULL, n>0)` → null-pointer error. |
| Range order | **Preserves core's order** (`mesh.rs:3663–3708`), evaluated in this sequence: (1) `start > end` → invalid argument. (2) `start == end` → success with an empty buffer, **even if `start` is beyond the blob's size**, as core does. (3) Non-empty: `end - start > MAX_FETCH_RANGE_BYTES` → invalid argument. (4) Non-empty: `end > size` → invalid argument. The accept/reject decisions are identical to core. The only difference is **classification**: core reports (1), (3) and (4) as `BlobError::Backend`, and the new C function pre-checks them in the same order and returns a dedicated invalid-argument code instead. That is a deliberate tightening of the error code, named here, not pure forwarding. |
| Invalid-argument code | A new `NET_ERR_BLOB_INVALID_ARGUMENT`, numbered from a fresh inventory of the `src/ffi` code space. `−120` is **not** free (see "Defects found on the way"). |
| Encoding | `encoding_kind`: `0` = Replicated, `1` = Reed-Solomon, any other value → invalid argument. For Replicated, `rs_k` and `rs_m` **must both be 0**; anything else is invalid argument (no silent ignore). For Reed-Solomon, `rs_k == 0 && rs_m == 0` together means use the core defaults (`DEFAULT_RS_K` / `DEFAULT_RS_M`). That is the only default, and exactly one of them being 0 is invalid argument. Otherwise `k ≥ 1`, `m ≥ 1`, and `k + m ≤ 255`, computed in `u16`. |
| JSON DTOs | Field names are the core struct's snake_case names, and counters are JSON integers (`u64`). Go decodes them into `uint64` fields **without** `omitempty`. |
| Optional state | Tree-cache stats with no cache installed → success with JSON `null`, which Go maps to `(nil, nil)`. Not an error. |
| `describe` | Variant-specific fields (`tree_root_hash`, `tree_depth`) are **absent** for a non-tree ref, never zero placeholders. Go uses pointer fields for them. |
| Go errors | New codes join the `ErrBlob` tree. Feature-off uses the shared sentinel. |
| Handles and panics | Borrow the adapter under the same guard as the existing blob functions, so `Close` waits for in-flight calls. Every body runs inside the existing `adapter_guard` panic containment. |

**Alternatives rejected:**

- *Give `bindings/go/net/` its own `go.mod` and test it in CI.* That leaves two
  Go packages covering the same surface, which is the drift this plan ends.
- *Delete `bindings/go/net/` now and port later from git history.* It is the
  only record of how several surfaces were meant to look in Go. Port first,
  delete last (S8).
- *Pass `BlobRef` as a C struct.* It versions badly. Encoded bytes plus a
  JSON accessor is what the existing functions already use.
- *Keep `C.GoBytes` behind a `C.int` maximum check.* Workable, but it caps
  every buffer at 2 GiB for no reason core imposes. The checked copy costs
  nothing more.

### Decision: callback blob adapters (S5b)

`net_blob_register_callback_adapter` lets Go implement a `BlobAdapter`, with
Rust calling back into Go on Rust-owned threads. That's the same shape as the
MCP consent callbacks, where the `mcp-sdk` branch review found
use-after-free and cgo thread races.

A `cgo.Handle` table alone is not an ownership contract. Unregister removes
only the registry's `Arc` (`blob/registry.rs:74–83`). `net_blob_resolve`
already holds a clone (`src/ffi/blob.rs:316–352`), and the adapter clones its
`Arc<OpaqueCtx>` into `spawn_blocking`, where it stays in use through `fetch`
and the later `free_buffer` callback (`:664–756`). The vtable has no context
destructor (`:509–522`), and `OpaqueCtx` gives no drop notification
(`:551–567`).

**Decision:**

- **Additive owned-context registration.** A new
  `net_blob_register_callback_adapter_owned(id, vtable, ctx, release_fn)`
  leaves the existing function and the `NetBlobAdapterVtable` layout
  untouched. `release_fn(ctx)` runs exactly once, from the `Drop` of the
  **shared context** that blocking work retains, not the outer adapter,
  because a cancelled future can drop the adapter while `spawn_blocking` is
  still running.
- **Ownership on failure.** If registration fails (duplicate id, null
  pointer, feature off), ownership stays with the caller, `release_fn` is not
  called, and Go deletes the handle. On success, Rust owns the context until
  `release_fn` runs, and only then does Go call `Handle.Delete`.
- **Buffers.** Callback-produced buffers are allocated and freed by matching
  C helpers in the Go module's cgo preamble (`C.malloc` / `C.free`), never
  by the Go GC.
- **Panic containment.** `recover` wraps the entire exported trampoline,
  including the handle lookup and type assertion, not just the user method.

S5b can be deferred without affecting S5. Deferring it doesn't make the S7
placement port safe.

### Decision: placement-filter lifetime (S7)

The reference wrapper maps a string id to a Go predicate
(`bindings/go/net/placement.go:135–148`) and deletes the entry as soon as Rust
unregister returns (`:290–329`). A scheduler-held `CgoPlacementFilter` holds
only the id (`compute-ffi/src/lib.rs:3209–3216`) and dispatches it later
(`:3265–3280`). So an old filter can veto after removal, or call a **new**
predicate registered under the same id. The registration mutex doesn't cover
scheduler-held `Arc` clones.

**Decision:** bind each native bridge to a per-registration token through an
additive registration entry point, with an owned-context release (the S5b
pattern). The original predicate stays alive until the bridge's last owner
releases it, and whole-trampoline panic containment applies. If that is too
much for this track, S7 records placement as **deferred**, with this reason
and the reference API design preserved in its ledger row before S8 deletes
the file.

## The slices

Each slice leaves `go test ./...` green (cgo enabled, `libnet` from
`cargo build --release -p net-ffi --features net-ffi/test-helpers`), and
does its own header closure and export-baseline additions.

### S1: tests for the existing `go/blob.go`, plus checked buffer copies

The one non-test change: add `checkedLen` / `copyCBuf` and move
`go/blob.go`'s three `C.GoBytes` sites onto them. Everything else pins
**current** behavior and adds no new policy.

`go/blob_test.go`, on a `Redex` from `NewRedex` with a temp `persistentDir`:

- **Round-trip.** `Store` then `Fetch` returns the same bytes.
- **Exists.** A known ref is `false` on a fresh, empty adapter. After `Store`
  on that adapter, it's `true`. (`Publish` also stores, so it can't supply
  the `false` half.)
- **Publish.** `Publish(uri, data)` returns a ref that `BlobRefHash` decodes to
  the BLAKE3 of the data.
- **Persistence.** With `Persistent: true`: store, `Close` the **adapter**,
  free the `Redex`, reopen both on the same directory, and fetch. The adapter
  holds its own `Arc<Redex>`, so closing only the `Redex` isn't a restart.
- **Overflow config, current behavior.** Set → get round-trips with
  **non-zero** values. Explicit zeros are pinned separately: `omitempty` sends
  them as absent, and they come back as native defaults. A finite
  out-of-range ratio is pinned as **accepted** (the parser assigns finite
  ratios as given, `src/ffi/blob.rs:952–986`). Refusal is covered with inputs
  that really are refused: an unknown `scope` and malformed JSON.
- **Metrics.** `PrometheusText` contains the `dataforts_blob_` prefix (the
  witness at `tests/net_blob_cli.rs:302`).
- **Close race.** `Close` racing 32 `Fetch` goroutines doesn't crash, and every
  call after `Close` returns an error.
- **Two nodes.** `ServeBlobTransfer` on both peers of `meshHandshakePair`
  (`go/mesh_test.go:38`), then `FetchBlob` returns the bytes.
- **Length boundaries.** `checkedLen` at 2³¹−1, 2³¹ and 2³²+1, and `copyCBuf`
  with `(nil, 0)` and `(nil, 1)`. No large allocation.

**Proves it:** green under `-race`. Mutations that must turn a test red:
swapping `Store`'s ref and data arguments; passing `0` instead of the
`persistent` **C int argument** to `net_mesh_blob_adapter_new`
(`src/ffi/blob.rs:1008–1065`); and reverting one copy site to
`C.GoBytes(…, C.int(n))` (a `checkedLen` unit test catches the helper, and a
grep test fails if `C.GoBytes` reappears in `go/blob.go`).

**CI:** CI's Go runner (`.github/scripts/go-test-with-native-stacks.sh`,
`ci.yml:4815,4870`) doesn't pass `-race`, so the normal Go job isn't race
evidence. S1 adds an explicit `-race` step through the same native-stack
runner, so hangs still report native frames. It runs the blob tests, and
from S5b on the callback tests too.

**Done locally, 2026-10-03.** Exact-head CI is still owed: the new `-race`
step and roster have run only as their local equivalents. On Windows
(go1.26.2, WinLibs gcc, `net.dll` from
`cargo build --release -p net-ffi --features net-ffi/test-helpers`):

- `go test -count=1 -run '^TestBlob' -v .`: 14/14 pass (`go/blob_test.go`).
- `go test -race -count=3 -run '^TestBlob' .`: ok, no races.
- `go test -count=1 .` (the whole package, without
  `RUN_INTEGRATION_TESTS`): ok, 188 s.
- Mutations, each applied to `go/blob.go`, confirmed to compile with
  `go vet`, and confirmed killed by a named test:
  - `persistent` → `persistent*0`: `TestBlobPersistenceAcrossReopen/persistent`
  - Store's ref and data arguments swapped:
    `TestBlobExistsAndStoreOnIndependentAdapter`,
    `TestBlobStoreRefusesMismatchedBytes`
  - `Fetch` reverted to `C.GoBytes(…, C.int(n))`: `TestBlobSourceHasNoGoBytes`
  - the `InvalidJson` → `ErrBlobInvalidConfig` mapping removed:
    `TestBlobOverflowConfigRefusals`

  A first pass reported two false results. The persistence mutant didn't
  compile (it left `persistent` unused), and the swap mutant never applied (a
  whitespace mismatch). Both were rerun with an apply check and a compile
  gate, and are killed as listed above.
- The persistence test carries an in-memory control subtest, so it fails if
  persistence stops mattering.
- `check-roster.py --label blob_test --mode decl`: 14 pinned names exist.
- Runner guard: a test binary run with a non-matching `-test.run` exits 0
  and prints no `=== RUN` line (checked locally). That is the condition
  `go-test-with-native-stacks.sh` now fails on when `GO_TEST_RUN` is set.
  The script itself is Linux-only (`sudo sysctl`); it passes `bash -n`, and
  CI runs it for real.

Changes: `go/cbuf.go` (new: `checkedLen`, `copyCBuf`); `go/blob.go` (three
copy sites moved to `copyCBuf`, plus the two fixes below); `go/blob_test.go`
(new); `.github/scripts/go-test-with-native-stacks.sh` (opt-in
`GO_TEST_RACE` / `GO_TEST_RUN`, and the no-match guard); `ci.yml` (the
"Run Go blob tests (-race)" step and the "Witness roster — Go blob binding
(14)" step).

Defects found and fixed in S1:
- `SetOverflowConfig` returned plain `ErrBlob` for the parser's refusal
  (`InvalidJson`, for example an unknown scope), although the
  `ErrBlobInvalidConfig` doc promises that sentinel for exactly this case.
  The error now wraps `ErrBlobInvalidConfig`, which still matches `ErrBlob`,
  so the change is additive.
- The `MeshBlobAdapterOpts.Persistent` doc pointed to a nonexistent
  `NewRedexWithPersistentDir`. It now says `NewRedex(dir)`.

Observed, not changed: at construction an unknown scope comes back only as a
null handle, so `NewMeshBlobAdapter` can report only the generic `ErrBlob`
(pinned in `TestBlobOverflowConfigRefusals`).

### S2: directory transfer

Header closure: the four functions reach cgo from `include/net_transport.h`,
either mirrored as `go/net_transport.h` and included from `go/net.h`, or with
the declarations moved into `net.go.h`. Port `StoreDir`, `FetchDir`
(returns `DirStats{Files, Bytes}`), `DirManifestRead` (decodes the JSON into
typed entries) and `FetchBlobDiscovered`. Errors use the extended
`ErrTransfer` mapper.

Allocators: byte outputs are freed with `net_transport_free_buffer`; the
manifest JSON is freed with `net_free_string`.

**Proves it**, on a two-node fixture with the transfer engine installed on
both peers:

- **Directory round-trip.** Store a 3-file tree with a nested subdirectory,
  `FetchDir` into a temp dir, and byte-compare every file. `DirStats`
  matches the file count and byte total.
- **The manifest JSON itself.** Call `DirManifestRead` directly and assert
  the decoded entries: paths, sizes, the externally tagged entry kind (file
  vs. dir), and that each embedded encoded ref decodes with `BlobRefHash`. A
  byte-compare after `FetchDir` never touches that JSON.
- **Missing content.** `DirManifestRead` with a well-formed ref to content
  the reachable holder doesn't have: `errors.Is(err, ErrTransferNotFound)`
  and `errors.Is(err, ErrTransfer)`.
- **Not a manifest.** `DirManifestRead` with a ref to a blob that exists and
  fetches but holds non-manifest bytes (for example, an ordinary file blob):
  `errors.Is(err, ErrDirInvalidManifest)` and `errors.Is(err, ErrTransfer)`.
  The two classifications come from different native paths
  (`src/ffi/transport.rs:429–440`). Neither is remapped.
- **Discovered fetch.** `FetchBlobDiscovered` finds a blob without a holder
  id. It refuses 31- and 33-byte hashes in Go before the cgo call. A hash
  nobody holds pins its current code (`NET_ERR_TRANSFER_ALL_PEERS_FAILED`,
  `transport.rs:293`) under `ErrTransfer`.
- **Path refusal.** A `FetchDir` destination with no final name component
  (for example, a filesystem root) is refused as `DirError::UnsafePath`
  (`dataforts/dir.rs:737`), which maps to `NET_ERR_DIR_PATH_INVALID`, and
  Go matches that sentinel. Manifest-entry path refusal (`dir.rs:906`) is
  covered by the Rust tests, because a hostile manifest can't be built from
  Go.

**Done locally, 2026-10-03.** Exact-head CI is still owed.

- **Header closure.** The four functions are declared in the transfer
  section of `go/net.h` and `include/net.go.h`, the same place
  `net_fetch_blob` / `net_serve_blob_transfer` already were. No
  `#define NET_*` was added: Go maps the codes numerically, as `blob.go`
  already did, so the ABI-commit rule's constant trigger doesn't apply.
- **Signature pin.** `TestABIStabilityTransportDeclsMatchCanonicalHeader`
  checks that every function in the canonical `include/net_transport.h` is
  declared in `go/net.h` with identical parameters. `header_parity_test.go`
  only keeps `go/net.h` and `net.go.h` in step, and neither of those is the
  transport surface's own header.
- **Code pin.** `TestABIStabilityTransferCodes` maps every `NET_ERR_*` in
  `net_transport.h` through the Go mapper. To make that possible, the mapper
  now takes a plain `int` (`transferErrorFromInt`).
- **API.** `(*MeshBlobAdapter).StoreDir`, `(*MeshNode).FetchDir`,
  `(*MeshNode).DirManifestRead → *DirManifest`, and
  `(*MeshNode).FetchBlobDiscovered`, in `go/transfer.go`. `StoreDir` hangs
  off the adapter, not the node (Node's `storeDir` is on the mesh), because
  the C call takes only the adapter.
- **New sentinels**, all wrapping `ErrTransfer`: `ErrTransferAllPeersFailed`,
  `ErrDirInvalidManifest`, `ErrDirPathInvalid`, `ErrDirIO`; plus
  `ErrFeatureNotBuilt` (−107). Existing sentinels and messages are
  unchanged, apart from −202, which now returns
  `ErrTransferAllPeersFailed` (same message, still an `ErrTransfer`).
- **Correction to the S2 witness text above:** the manifest has **no
  per-entry size** (`DirEntry` is `path` plus an externally tagged `kind`).
  The manifest test instead checks path order, the one-kind-per-entry
  decode, the `Dir` entry for the empty directory, the file mode (0 on
  Windows), and that each embedded ref's hash equals a fresh publish of the
  same bytes. Content addressing makes that the oracle.

Evidence (Windows, same toolchain as S1):

- `go test -count=1 -run '^(TestABIStability|TestTransfer|TestBlob|TestHeaderParity)' .`:
  ok (9 transfer tests in `go/transfer_test.go`).
- `go test -race -count=3 -run '^(TestBlob|TestTransfer)' .`: ok.
- `go test -count=1 -v .`: 366 pass, 13 skip, 0 fail, 7.2 s. (S1's 188 s
  full run was a slow outlier, not a different test set.)
- Mutations, each confirmed to compile and confirmed killed:
  - −200 mapped to `ErrDirInvalidManifest`: `TestABIStabilityTransferCodes`,
    `TestTransferDirManifestMissingContent`
  - −210 unmapped: `TestABIStabilityTransferCodes`,
    `TestTransferDirManifestNotAManifest`
  - `Dir` kind dropped in decode: `TestTransferDirManifestRead`
  - embedded blob bytes corrupted in decode: `TestTransferDirManifestRead`
  - `DirStats` fields swapped: `TestTransferDirRoundTrip`
  - `go/net.h`'s `net_fetch_dir` losing a `const`:
    `TestABIStabilityTransportDeclsMatchCanonicalHeader`,
    `TestHeaderParityWithCrateHeader`

  A `uint64_t → uint32_t` drift doesn't compile (cgo type-checks the call),
  so the compile is the guard for width changes and the tests guard what
  cgo accepts.
- CI: the race step is now "Run Go blob + transfer tests (-race)" with
  `GO_TEST_RUN="^(TestBlob|TestTransfer)"`, and a "Witness roster — Go
  transfer binding (9)" step is added.

### S3: RedEX replication and greedy Dataforts

Header closure: declare the replication, greedy and gravity functions in
`include/net.go.h` and `go/net_cortex.h`.

- **Scope.** Methods on `Redex`: `EnableReplication` /
  `DisableReplication` / `ReplicationRuntimeCount` /
  `ReplicationPrometheusText`, and `EnableGreedyDataforts` /
  `DisableGreedyDataforts` / `GreedyCachedChannelCount` /
  `GreedyPrometheusText` / `EnableGravityForGreedy(DataGravityConfig)` /
  `DisableGravityForGreedy`. A `Replication *RedexReplicationConfig` field on
  `RedexFileConfig`, forwarded through `OpenFile` to the JSON the C parser
  already accepts. The DTO mirrors that parser's keys exactly, and nil leaves
  the key out (today's behavior). Names and defaults match the Node SDK's N8
  slice in [`NODE_SDK_GAPS_PLAN.md`](NODE_SDK_GAPS_PLAN.md), which has the
  same surface minus `DisableReplication`.
- **Ownership.** The enable functions take a `net_mesh_arc_clone` box, which C
  **consumes on every return code** (`src/ffi/cortex.rs:459–499`). Go clones
  it immediately before the call and never frees it afterward, error included.

**Proves it:**

- **Replication.** Disabled: the count is 0. Enable: still 0, because the
  router starts empty and the count counts router entries
  (`redex/manager.rs:541–549`). Open a file with a replication config: 1.
  Append on node A; bounded (≤ 10 s) reads on node B return the same events,
  so replication is proven by the data, not the metric. Disable: the count
  drains to 0. Enable twice pins the documented idempotence.
- **Arc ownership.** A test-helper live-Arc count shows no leak and no double
  free on the success path or the error path (a shutting-down `Redex`).
- **Greedy.** Admission is event-driven. A peer's event on a channel whose
  chain capabilities pass the greedy policy is cached (the
  `admitted_event_caches_and_announces_chain` path,
  `greedy/runtime.rs:1218`). Enable greedy on node B, publish from node A,
  and `GreedyCachedChannelCount` rises within a bounded wait. Disable stops
  new admissions. The fixture sets the scope / intent-match config so
  admission doesn't depend on defaults.
- **Gravity: forwarding, plus the real observable.** Gravity doesn't create
  or delete cache entries. Enabling it installs a heat registry and a tick
  task, and disabling clears the slot while greedy caching keeps running
  (`greedy/runtime.rs:390–407`, `redex/manager.rs:448–492`). What gravity
  does is turn reads into heat announcements: `gravity_tick` builds
  emissions and calls `announce_heat_batch` (`greedy/runtime.rs:628–701`),
  which writes a `heat:<chain-hex>=<rate>` tag into the node's announced
  capability set (`mesh.rs:46360`). The witness:
  1. Enable greedy and gravity, with a short tick and a low emit threshold in
     the config JSON (`emit_threshold_ratio`, plus the tick and decay knobs
     `into_policy_and_tick` reads, `src/ffi/cortex.rs:828`).
  2. Get a cached channel admitted, and drive reads through it with a
     publisher-stamped `origin_hash`. Unstamped reads only bump
     `dataforts_greedy_gravity_heat_unattributed_total`.
  3. Within a bounded wait (a few ticks), observe the `heat:<hex>=…` tag on
     that node's capabilities through a supported surface: the peer's
     capability view, or the local announced set.
  4. Disable gravity, keep reading, wait the same number of ticks, and
     assert **no newer emission** for that chain. Already-announced tags and
     cumulative counters aren't expected to reset.

  Invalid-config refusal is pinned separately (malformed JSON →
  `InvalidJson`; whichever range checks `into_policy_and_tick` applies).
  **If Go can't read a node's announced tags by prefix,** the smallest hook
  is a `test-helpers`-only C accessor for the local announced capability
  tags. It's declared in this slice and added to the baseline only under
  `test-helpers`. Until it exists, the evidence is stated honestly as
  "forwarding plus the native gravity tests", not as a cache-count toggle.

**Done locally, 2026-10-03.** Exact-head CI is still owed.

- **Header closure.** The ten functions are declared in
  `include/net_cortex.h` and `go/net_cortex.h`, beside `net_redex_new`.
  The mesh-Arc parameter is a forward-declared
  `struct net_compute_mesh_arc_s*`: that's the struct `net.go.h`'s
  `net_compute_mesh_arc_t` names, and it needs no typedef or `NET_*` guard
  macro in the self-contained cortex header. (A guard macro would have
  tripped the ABI-commit rule's `#define NET_*` trigger.) No test compared
  the cortex header pair before; `TestABIStabilityCortexDeclsMatchCanonicalHeader`
  now does, function-for-function, and the existing declarations already
  agreed.
- **API.** In `go/redex_dataforts.go`: the ten `Redex` methods take
  `*MeshNode` and clone the Arc themselves, under the mesh's read lock, just
  before the consuming call (`withMeshArc`), and never free it. The
  reference package made the caller pass a raw Arc pointer.
  `RedexFileConfig.Replication *RedexReplicationConfig` is forwarded through
  `OpenFile`. `GreedyConfig` and `DataGravityConfig` mirror the native
  JSON. Every failure is an `ErrRedex`; encode or parse refusals are also
  `ErrInvalidRedexConfig`; a closed Redex or shut-down mesh is also
  `ErrShuttingDown`; feature-off is also `ErrFeatureNotBuilt`.
- **Not ported:** the reference's binding-side `validateReplicationConfig`.
  The native side validates and refuses with `NET_ERR_REDEX`, and a second
  copy of the bounds in Go would drift. This gets an S7 ledger row.
- **Arc ownership evidence.** The native tests pin consumption on error
  paths (`enable_replication_drops_mesh_arc_on_null_redex` and the greedy
  twin in `src/ffi/cortex.rs`). The Go side has no free to get wrong, and no
  Arc strong-count accessor exists, so the plan's live-Arc-count witness is
  **not built**. It would need a new `test-helpers` symbol, and the native
  tests already cover the contract.

Evidence (Windows):

- `go test -count=1 -run '^TestRedex(Replicat|Greedy|Gravity|Dataforts)' -v .`:
  7/7 pass (`go/redex_dataforts_test.go`).
  - Replication: refused without enable; count 0 → 0 after enable → 1 on
    opening a replicated channel → 0 after disable; and **data arrival**:
    pinned placement with A as leader, 8 appends on A, all 8 read back on
    B.
  - Greedy: B subscribes to A's channel, A publishes, and
    `GreedyCachedChannelCount` reaches ≥ 1. After disable, the count is 0,
    the metrics are `""`, and a later publish admits nothing.
- `go test -race -count=2 -run '^(TestBlob|TestTransfer|TestRedex)' .`: ok.
- Mutations (each compiles):
  - `Replication` not forwarded (`json:"-"`): killed by three replication
    tests
  - greedy config dropped: killed by `TestRedexGreedyCachesAPeersChannel`
    (the default intent policy doesn't admit) and
    `TestRedexGreedyConfigRefusals`
  - `DisableGreedyDataforts` a no-op: killed by
    `TestRedexGreedyCachesAPeersChannel`
  - `DisableReplication` a no-op: killed by
    `TestRedexReplicationRuntimeCountFollowsChannels`
  - `go/net_cortex.h` losing a `const`: killed by
    `TestABIStabilityCortexDeclsMatchCanonicalHeader`
  - **gravity config dropped: SURVIVES.** See the gravity note below.
- CI: the race step's filter is now `^(TestBlob|TestTransfer|TestRedex)`,
  which also puts the existing RedEX file tests under `-race`, and a 7-name
  roster is added.

**Gravity, scoped honestly (as Q4 allowed).** Go's gravity evidence is
forwarding and install rules only: refused without greedy; nil, tuned and
`Enabled: false` configs accepted; NaN refused before the C call;
idempotent disable; greedy survives a gravity disable. **Whether a gravity
config reaches the policy is not observable from Go**, and the
dropped-config mutant above survives to show it. The reason: heat comes
from reads served by the greedy cache (`Redex::greedy_cache_for`), and no
binding (Go, Node or Python) exposes that read path. So no binding can
produce heat, and none can observe a config's effect on it. Native evidence:
`tests/dataforts_gravity_e2e.rs`
(`read_hot_chain_emits_heat_tag_into_local_caps`,
`greedy_without_gravity_emits_no_heat_tags`). The `test-helpers` tag
accessor named above wouldn't help without a read driver, so it wasn't
added. Exposing greedy reads is a new capability (SDK-first rule) and is
recorded under "Defects found on the way" rather than built here.

### S4: CortEX read-your-writes

Header closure: declare both wait functions in `go/net_cortex.h` and its
canonical source.

**Additive API, no source break.** The CRUD signatures stay
`(uint64, error)`. Adapters keep the `originHash` they were opened with and
gain `OriginHash() uint64`, `Token(seq uint64) WriteToken` (a new exported
`WriteToken{OriginHash, Seq}`), `WaitForToken(tok, timeout)` and
`WaitForTokenContext(ctx, tok)`. A caller writes `seq, _ := t.Create(...)`,
then `t.WaitForToken(t.Token(seq), d)`.

Semantics:

- **Zero timeout polls.** In C, `timeout_ms == 0` means check once
  (`src/ffi/cortex.rs:1830`), and `WaitForToken` matches that. This is
  documented as a deliberate contrast with `WaitForSeq`, where zero means wait
  indefinitely. An unbounded wait goes through the context variant.
- **Errors.** `NET_ERR_TIMEOUT`, `NET_ERR_WRONG_ORIGIN`, `NET_ERR_QUEUE_FULL`
  and `NET_ERR_FOLD_STOPPED` each map to a distinct sentinel.
- **Origin, not identity.** C checks the token's origin against the
  adapter's, not which object issued it (`src/ffi/cortex.rs:1806–1847`).
- **Cancellation.** `WaitForTokenContext` checks `ctx.Err()` **before** the
  first poll (the reference loop polled first). Poll slices are bounded
  (≤ 50 ms).

**Proves it:**

- **Read-your-writes.** `Create`, then `WaitForToken(t.Token(seq), 1s)`
  returns nil, and an immediate `List` includes the write.
- **Wrong origin.** A token with a **different origin hash** →
  `ErrWrongOrigin`.
- **Same origin, other adapter.** A second adapter opened with the same
  origin on the same `Redex` is given a token whose sequence **that adapter
  has applied** (it waits on its own fold, `WaitForSeq`, first): success. A
  matching origin alone isn't a visibility guarantee, so the witness never
  relies on it. A same-origin token for a sequence the receiving adapter
  hasn't applied, with a zero timeout, returns `ErrTimeout`.
- **Zero timeout.** On an unapplied sequence, returns `ErrTimeout`
  immediately (bounded under 50 ms).
- **Pre-cancelled context.** Returns `context.Canceled` even when the token
  is already satisfied. Cancelling mid-wait returns within one poll slice.

**Done locally, 2026-10-03.**

- **Header closure.** Both wait functions are declared in the two cortex
  headers beside their `wait_for_seq` siblings, pinned by
  `TestABIStabilityCortexDeclsMatchCanonicalHeader`.
- **API** (`go/write_token.go`), as decided: `WriteToken{OriginHash, Seq}`
  and `OriginHash()`, `Token(seq)`, `WaitForToken(tok, timeout)` and
  `WaitForTokenContext(ctx, tok)` on both `TasksAdapter` and
  `MemoriesAdapter`. CRUD signatures are unchanged. Adapters now keep the
  origin they were opened with, and `NetDb` adapters inherit
  `NetDbConfig.OriginHash`.
- **Errors.** `ErrTokenTimeout` (wraps `ErrStreamTimeout`, since the native
  timeout code 1 already mapped there), `ErrWrongOrigin` (−104),
  `ErrWaitQueueFull` (−105), `ErrFoldStopped` (−106).
- **Timeouts.** Zero or negative polls once. A positive timeout under 1 ms
  rounds up to 1 ms, so it still waits instead of quietly becoming a poll.

Evidence (Windows):

- `go test -count=1 -run '^TestWriteToken' -v .`: 7/7 pass
  (`go/write_token_test.go`). Covers read-your-writes on both adapter kinds;
  a different origin refused; a same-origin second adapter accepted only
  after `WaitForSeq` on its own fold, while an unapplied same-origin seq
  times out; zero timeout returns in < 50 ms and a 120 ms timeout really
  waits; pre-cancelled, deadline and mid-wait cancellation (noticed in
  < 200 ms); NetDb origin inheritance; and `ErrShuttingDown` after Close.
- `go test -race -count=2 -run '^(TestBlob|TestTransfer|TestRedex|TestWriteToken)' .`: ok.
- Full package: ok.
- Mutations (each compiles), all killed:
  - `Token` dropping the origin: 5 tests
  - NetDb not passing the origin: `TestWriteTokenNetDbAdaptersInheritOrigin`
  - zero timeout made to wait: `TestWriteTokenZeroTimeoutPolls`
  - −104 mapped as a timeout: two tests
  - the poll slice made 5 s: the mid-wait cancellation subtest
  - `ctx` checked only after polling (the reference order): first killed
    only by the 10-minute binary timeout, because the `deadline` subtest
    spun forever. That subtest now runs the wait in a goroutine with a 2 s
    bound, and the mutant fails three named subtests in seconds.
- CI: the race filter adds `TestWriteToken`, and a 7-name roster is added.

### S5: blob adapter registry: filesystem + publish/resolve

Header closure: declare the six registry functions in `include/net.go.h` and
`go/net.h`. The Go API: `RegisterFilesystemBlobAdapter(id, root)`,
`UnregisterBlobAdapter(id)`, `BlobAdapterRegistered(id)`,
`BlobPublish(adapterID, uri, data)` and `BlobResolve(adapterID, payload)`.
`net_blob_resolve` takes an explicit adapter id (`src/ffi/blob.rs:316–352`);
no discovery mechanism is added. Errors are `ErrBlob` sentinels from the
`NET_ERR_BLOB_*` codes.

**Proves it:** a publish through a filesystem adapter writes under `root`, and
resolve returns the bytes. After unregister, resolve fails with the
adapter-not-registered sentinel. A duplicate id fails with the duplicate-id
sentinel. Resolving through the wrong adapter id fails without touching the
right one. S5 doesn't depend on S5b.

**Done locally, 2026-10-03.**

- **Header closure.** The five non-callback registry functions are declared
  in the blob section of `go/net.h` and `include/net.go.h` (pinned by
  `TestHeaderParityWithCrateHeader`). `net_blob_register_callback_adapter`
  stays undeclared until S5b.
- **API** (`go/blob_registry.go`): `RegisterFilesystemBlobAdapter(id, root)`,
  `UnregisterBlobAdapter(id) (bool, error)`, `BlobAdapterRegistered(id)`,
  `BlobPublish(adapterID, uri, data)`, `BlobResolve(adapterID, ref)`.
- **Errors.** Every `NET_ERR_BLOB_*` code maps to an `ErrBlob` sentinel:
  `ErrBlobDecode`, `ErrBlobDuplicateID`, `ErrBlobNotRegistered` (both −112
  and −119), `ErrBlobNotFound`, `ErrBlobHashMismatch`, `ErrBlobBackend`,
  `ErrBlobUnsupportedScheme`, `ErrBlobUnauthorized`. Those codes exist only
  as Rust `pub const`s, in no header, so `TestABIStabilityBlobRegistryCodes`
  parses `src/ffi/blob.rs` and fails if a code gains no mapping.
- **Binding-side refusal:** an id or URI containing a NUL byte is refused
  with `ErrBlobInvalidArgument`. `C.CString` would otherwise truncate
  `"a b"` to `"a"` and address a different adapter. This follows the
  existing precedent in `go/mcp.go`.

Found while writing the witnesses (behavior pinned, not changed):

- `net_blob_resolve` returns any payload without the `BLOB_REF_MAGIC`
  prefix **unchanged** (the documented "inline payloads round-trip").
  Garbage is therefore not a decode error, and the test pins the
  round-trip.
- A ref with a few trailing bytes cut off still decodes. The URI is the
  trailing field, so the cut ref names a shorter URI with the same hash, and
  content is still verified against the hash. Decode errors start once the
  cut reaches the fixed 40-byte body (the test uses `ref[:7]`).

Evidence (Windows):

- `go test -count=1 -run '^(TestBlobRegistry|TestABIStabilityBlobRegistry)' -v .`:
  6/6 pass (`go/blob_registry_test.go`). Covers: the blob is on disk at
  `<root>/<hash[0:2]>/<hash>` and resolves; duplicate id; unregister, then
  resolve fails `ErrBlobNotRegistered`, and the id can be re-registered;
  resolving through another adapter is `ErrBlobNotFound` and writes nothing
  there; an unknown id is refused; `mesh:` on a filesystem adapter is
  `ErrBlobUnsupportedScheme`; a tampered on-disk blob is
  `ErrBlobHashMismatch`; NUL refusals; and the code pin.
- `-race` (the existing `^TestBlob` filter covers these): ok. Full package:
  ok.
- Mutations, all killed: −112 unmapped (3 tests); NUL check removed;
  unregister's result inverted; −114 mapped as backend (2 tests).
- CI: "Witness roster — Go blob adapter registry (6)".

### S5b: callback blob adapters

**Deferred, 2026-10-03.** The plan allows this: S5 stands without it. The
design under "Decision: callback blob adapters" is unchanged and still
owed: an additive owned-context registration with a release from the
shared context's drop, plus `test-helpers` barriers for the
unregister-while-held witnesses. That is native work (a new export, new
test hooks) of the same size as S6, so it waits until S6's ABI table and
export-baseline discipline are in practice.

`RegisterBlobAdapter(id, BlobAdapter)`: a Go interface with `Store`, `Fetch`,
`FetchRange` and `Exists`, registered through
`net_blob_register_callback_adapter_owned` (the design above). Adds that one
symbol to the export baseline in the same commit.

**Proves it:** deterministic barrier tests. `-race` runs only support them.

- **Round-trip.** A Go map-backed adapter round-trips through `BlobPublish` /
  `BlobResolve`.
- **Unregister while held.** A `test-helpers` barrier holds a Rust worker
  (a) before the handle lookup and (b) before `free_buffer`. The test
  unregisters while it's held, then releases it, and asserts the callback
  completes against the **original** adapter and `release_fn` fires exactly
  once, after the release.
- **Failure ownership.** A failed registration (duplicate id) leaves the
  handle owned by Go and never calls `release_fn`.
- **Cancellation.** A resolve cancelled mid-callback still ends with exactly
  one `release_fn`.
- **Same-id re-registration.** Re-registering the same id while an old call is
  held: each call reaches its own adapter.
- **Panics.** A panicking Go adapter, including a panic injected into the
  trampoline's lookup path, surfaces as an error, not a process abort.

### S6: new C ABI for tree, erasure, range and repair

Rust in `src/ffi/blob.rs` plus stubs, with `include/net.go.h` and `go/net.h`
mirrored and the new symbols added to the baseline, all in one commit.
Every function follows the normative contract table above.

| Function | Notes |
|---|---|
| `net_mesh_blob_adapter_fetch_range(h, ref, ref_len, uint64_t start, uint64_t end, uint8_t** out, size_t* out_len)` | Range order per the table |
| `net_mesh_blob_adapter_store_tree(h, data, size_t len, uint8_t encoding_kind, uint8_t rs_k, uint8_t rs_m, uint8_t** out_ref, size_t* out_ref_len)` | Default chunking, as in Node/Python |
| `net_mesh_blob_adapter_repair_blob(h, ref, ref_len, char** out_json)` | `RepairReport` JSON |
| `net_mesh_blob_adapter_tree_node_cache_stats(h, char** out_json)` | `null` when no cache |
| `net_blob_ref_describe(ref, ref_len, char** out_json)` | version, uri, hash, size, is_tree, is_chunked; tree fields absent for non-tree refs |
| `net_mesh_blob_adapter_new_v2(redex, id, int persistent, const char* options_json)` | Additive constructor; the legacy one is unchanged |

`new_v2`'s JSON has `overflow` (the legacy overflow object, unchanged) and
`tree_node_cache_bytes` as top-level keys. The legacy constructor takes
`persistent` as a C `int` plus an **overflow-only** JSON
(`src/ffi/blob.rs:1008–1065`), so it has no room for a cache option. An
absent `tree_node_cache_bytes` means no cache. An explicit `0` installs a
zero-capacity cache. Core treats these as distinct, and Go keeps them
distinct with `TreeNodeCacheBytes *uint64`.

Go: `FetchRange`, `StoreTree(data, Encoding)`, `RepairBlob → RepairReport`,
`TreeNodeCacheStats`, `DescribeBlobRef → BlobRefInfo`, the four tag strings as
`const`s, and `MeshBlobAdapterOpts.TreeNodeCacheBytes`, which routes
construction through `new_v2` when set.

**Proves it:**

- **Rust unit tests per C function**, including every row of the contract
  table: each out-pointer nulled in turn; mixed-null pairs; `(NULL, n>0)`
  inputs; every encoding edge (unknown kind, Replicated with non-zero k/m,
  exactly one of k/m zero, `k + m = 256`, both zero → defaults); and the range
  sequence (reversed → invalid; `start == end` beyond size → empty success;
  over-cap → invalid; `end > size` → invalid).
- **Constructor.** Cache enabled with nil overflow; cache omitted (stats →
  `nil`); explicit zero (stats present, capacity 0); and legacy overflow-only
  construction unchanged.
- **Repair.**
  - Fixture: at least 16 MiB (16,777,216 bytes), four full 4 MiB chunks
    (`blob/blob_tree.rs:155–160`, `blob/blob_ref.rs:131`) with distinct
    content, stored with Reed-Solomon `k=4`, `m=2`. RS closes a stripe only at
    `k` full chunks, and an incomplete trailing stripe is stored Replicated
    with no parity (`blob/erasure.rs:493–543`), so anything smaller has
    nothing to repair.
  - Remove a real **data** shard through a narrowly gated test seam: a
    `test-helpers`-only C function, or a shutdown → delete → reopen fixture
    that also clears RedEX and cache state. Deleting a file while live state
    can still serve it proves nothing. No production deletion authority is
    added.
  - Prove the shard is unavailable **before** repair.
  - `RepairBlob` reports `chunks_restored == 1` and `stripes_repaired == 1`;
    a second `RepairBlob` restores nothing; `FetchRange(ref, 0, size)` equals
    the source.
  - Losing more than `m` shards of one stripe: the counters report it
    unrecovered, and Go documents that a nil error doesn't mean a complete
    repair (`mesh.rs:3197–3229`).
- **Describe.** A tree ref has the tree fields set; a small ref has them
  absent (Go `nil`).
- **ABI pins.** `go/abi_stability_*_test.go` covers all six signatures.
- **Cross-language fixture.** A `BlobRef` encoded by Go describes identically
  in the Python test suite, using a frozen **normalized** form: absent
  fields omitted, hashes as lowercase hex. Core's fields are optional per
  variant, while Python's getters fall back to zero values, so raw outputs
  aren't comparable. The Python change is **test-only** fixture validation,
  with no change to the Python production API.

### S7: classify and port the rest of the reference package

A ledger in this plan, filled in when the slice lands: one row per public
symbol, **methods qualified by receiver** (`(*CircuitBreaker).Call`, not
`Call`), each marked port / drop / deferred / already-ported with a reason.
Every drop or deferred row keeps the relevant API design (signatures and the
doc-comment intent) in the ledger itself, because S8 deletes the only copy.
Ports land with tests. `resilience.go` is pure Go.

Placement is gated on the lifetime decision above, or on a recorded,
justified deferral. If ported, it adds its registration symbol to the
baseline in the same commit and lands with this witness: old filter
acquired → unregister → same-id replacement registered → the old invocation
still runs its **original** predicate, and the original's release fires
exactly once, after the last scheduler-held clone drops.

**Proves it:** the ledger is complete, and a receiver-qualified symbol diff
(reference vs. `go/`) leaves nothing unaccounted for.

### S8: delete `bindings/go/net/`

Remove the directory. Repoint the comments in `go/*.go` that cite it
(`go/deck.go:13`, `go/meshdb.go:12`, `go/meshos.go:15`,
`go/meshos_test.go:128,144`), and amend the stale paths in
[`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) with a note rather than a
rewrite. User-facing docs move with the surface: `go/README.md`, the Go
section of the docs site (`web/src/content/docs/`), Go snippets in the
`net-event-bus` skill's Dataforts material, any capability or support
matrix that lists Go blob support, and the release notes for the shipping
version (mirrored with `npm run sync:releases`).

**Proves it:** `grep -rn "bindings/go/net/" --include=*.go --include=*.md`
returns only historical plan text. `go test ./...`, the `-race` step and the
CI Go job are green.

## Risks

- **The reference code is wrong against today's ABI.** It has never been
  compiled in CI. *Fallback:* port means rewrite; the C signature wins every
  disagreement, and S7 records each one.
- **Callback lifetime needs new native ownership.** *Fallback:* S5b and the
  placement port are each deferrable with a ledger entry. S1–S5 and S6 don't
  depend on either.
- **Windows dev box vs Linux CI.** cgo runs locally (WinLibs gcc, `net.dll`
  on PATH), and CI checks `libnet.so`. *Fallback:* the export checker
  compares a platform-neutral name set. A Linux mismatch is investigated as
  an unexpected missing or extra export and fixed at the source.
- **The disk fills mid-build.** S6 rebuilds `net-ffi` with its full feature
  set. *Fallback:* one target directory, check free space before the gates.
- **The RS repair fixture is slow** (16 MiB, two-node). *Fallback:* run repair
  on a single node with the gated seam or the shutdown → delete → reopen
  fixture, which is what the Rust precedent does through the adapter's
  deletion path (`blob/mesh.rs:1269–1291`, `:6590–6628`).
- **The gravity witness can't observe heat from Go.** *Fallback:* the
  `test-helpers` accessor named in S3, or evidence scoped to forwarding plus
  native tests, stated as such.

## Not in scope

- Moving or renaming the Rust `*-ffi` crates.
- Chunking-strategy or bandwidth-class parameters on any Go method (see G3).
- Streaming blob store/fetch (`io.Reader` / `io.Writer` over the C ABI).
- Blob GC, pin / unpin, and auth-guarded operations (`pin_authorized`,
  `delete_chunk_authorized`). Node and Python don't expose them either.
- Production API changes to the Node or Python bindings. Test-only fixture
  validation in the Python suite (S6) is allowed.
- Overflow ratio validation. It would be a behavior change, with its own plan
  entry if wanted.
- Changing Go CRUD signatures to return tokens. That needs explicit
  authorization as a breaking change.
- Making `Fetch` accept tree refs. That's a core behavior change.
- Changing any existing native error mapping.
- Moving the 21 non-blob `C.GoBytes(…, C.int(n))` sites onto the checked
  copy (recorded below).

## Defects found on the way

- **`C.GoBytes` length truncation, outside blobs (deferred).** 21 call sites
  across `go/cortex.go`, `go/mesh_rpc.go`, `go/mesh_rpc_typed.go`,
  `go/tool.go` and others pass `C.int(n)` for a `size_t` length. Each is
  reachable only if that path can return more than 2 GiB, which most core
  limits prevent, but the pattern is wrong in general. Fix: move them onto
  `copyCBuf` in a follow-up. The blob sites are fixed in S1.
- **Duplicate error code −120 (deferred, not ours).**
  `NET_ERR_BLOB_UNAUTHORIZED = -120` (`src/ffi/blob.rs:99`) and
  `NET_ERR_IDENTITY = -120` (`src/ffi/mesh.rs:109`). A caller that maps codes
  without knowing which function returned them can't tell them apart. S6
  numbers its new code from a fresh inventory. Renumbering either constant is
  an ABI change and is out of scope here.

- **gofmt drift in eight existing `go/` files (deferred, not ours).**
  `gofmt -l` lists `abi_stability_test.go`, `capabilities.go`,
  `capabilities_test.go`, `groups.go`, `meshdb_test.go`, `migration.go`,
  `org_test.go` and `subnets_test.go` (LF line endings, so this is real
  formatting drift). No CI gate runs gofmt on `go/`. The files touched by
  this plan are gofmt-clean.

- **No binding can read through the greedy cache (cross-binding gap,
  deferred).** `Redex::greedy_cache_for` has no C, napi or pyo3 entry point,
  so gravity heat (which comes from those reads) can't be produced or
  observed from any language binding. Found while building S3's gravity
  witness. Adding it is a new capability, so per the SDK-first rule it
  starts in `net-mesh-sdk`, not in this plan.

## Review history

### Round 1: `d1b6f29`, 2026-10-03

Verdict: hold for implementation authorization; direction accepted. All nine
findings were re-checked against source and confirmed.

| # | Sev. | Finding | Resolution |
|---|---|---|---|
| R1 | High | A `cgo.Handle` table can't know when Rust drops its last reference to a callback context; the placement bridge dispatches by a reusable string id | Owned-context registration with `release_fn` from the shared context's drop; per-registration placement identity or recorded deferral; barrier witnesses |
| R2 | Med | The G2 functions aren't declared in the shipped Go headers. The original plan said they were, and called `go/net.h` and `net.go.h` identical | "How this was checked" corrected; header closure per slice |
| R3 | Med | 3 MiB under RS{4,2} has no parity; deleting a file doesn't remove a shard; `Fetch` refuses tree refs | 16 MiB distinct-chunk fixture, gated seam, absence proven first, `FetchRange`, counter checks |
| R4 | Med | No general constructor options JSON; persistence is a C `int` | `new_v2` with absent vs. zero cache; the S1 mutation targets the C argument |
| R5 | Med | Enabling replication installs an empty router; no Go replication config; the Arc is consumed on every return | Replication field through `OpenFile`; data-arrival witness; Arc ownership |
| R6 | Med | No bound token without a source break; C checks origin, not identity; zero timeout polls | Additive `Token(seq)`; pinned semantics |
| R7 | Med | Ratio validation doesn't exist; `omitempty` zeros; persistence and `Exists` fixtures | S1 pins current behavior |
| R8 | Med | Resolve needs an adapter id; `ErrTransfer` already exists; `NET_ERR_DIR_*`; allocators | Extended mapper; S2/S5 shapes |
| R9 | Med | The new ABI contract was unspecified; feature tags are strings, not bits | Normative table; tag strings; no mask |

Notes also adopted: an explicit `-race` CI step through the native-stack
runner; no wholesale baseline regeneration; a receiver-qualified S7 ledger;
user-facing docs in S8.

### Round 2: `317168c`, 2026-10-03

Verdict: hold for a bounded correction pass. Round-1 corrections accepted:
owned contexts, placement identity, header closure, additive tokens, `new_v2`,
the production-sized RS fixture. All four findings were re-checked against
source and confirmed.

| # | Sev. | Finding | Resolution |
|---|---|---|---|
| Q1 | Med | "Check `math.MaxInt` before `C.GoBytes`" still truncates: `C.GoBytes` takes `C.int` | `checkedLen` / `copyCBuf` design with allocation-free boundary tests; the 21 other sites recorded as a defect |
| Q2 | Med | S2 mapped a missing manifest to `ErrDirInvalidManifest`; natively, missing content is `NET_ERR_TRANSFER_NOT_FOUND`, and only undecodable fetched bytes are `NET_ERR_DIR_INVALID_MANIFEST` | Two fixtures, two classifications, no native remap |
| Q3 | Med | ABI table contradictions: range precedence (empty beyond size), writing through null out-pointers, k/m under Replicated, unassigned widths | Range order preserves core, with the error-classification tightening named; null-checks before deref; Replicated requires k = m = 0; RS default only for both zero; all widths fixed |
| Q4 | Med | The gravity witness watched cache counts, which gravity doesn't change | Heat-tag witness (stamped reads → tick → `heat:` tag; disable → no newer emission), with a named `test-helpers` fallback |

Cleanup also applied: every slice adds its own export-baseline entries (not
just S6); superseded lists replaced by the adopted versions; the same-origin
token witness now uses a sequence the receiving adapter has applied.

While checking Q3's error codes, this pass found the duplicate −120 (see
"Defects found on the way").
