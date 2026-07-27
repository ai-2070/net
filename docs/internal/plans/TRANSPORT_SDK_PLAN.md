# Transport SDK plan — fairscheduler transport, blob transfer, directory transfer

## Status

This plan covers the five-language SDK work to expose the new transport primitives through the same patterns the existing SDK uses for other substrate surfaces (`tool`, `stream`, `dataforts`, `mesh`). The five tiers, in dependency order: Rust (native), C (foundation FFI), Python (pyo3), TypeScript (napi-rs), Go (CGO over C).

## What the SDK has to expose

Three primitive surfaces from the substrate, each needing SDK ergonomics in each tier:

**Blob transfer.** `BlobTransferEngine` plus the `TransferControl` / `TransferHeader` wire types. Applications using this directly initiate content-addressed fetches from peers advertising `causal:<hash>`, register pending transfers, handle the stream lifecycle. The SDK wraps this into a `fetch_blob(blob_ref) -> Bytes` shape that hides the engine and stream allocation behind a future or async iterator.

**Directory transfer.** `store_dir(adapter, root)` and `fetch_dir(adapter, root_blob_ref, dest)`. These already exist as substrate-public functions and could be re-exported almost directly, but the SDK surface should also expose `DirManifest`, `DirEntry`, `EntryKind`, and `DirStats` so applications can introspect what they're transferring and observe progress.

**Stream-level access for advanced users.** The `transfer_stream_id` / `is_transfer_stream_id` / `next_transfer_stream_id` helpers plus the stream lifecycle (open, send, close) for cases where applications need finer control than the high-level fetch API provides. This surface exists so applications that compose their own transfer-shaped operations on the fairscheduler transport (writeback, future directory sync, etc.) don't have to reach into substrate internals.

## What the SDK does NOT add

This plan deliberately stays at the boundary the existing SDK has established. The SDK is thin wrapper plus ergonomics, not policy or composition.

- **No directory sync primitives.** Watching local files for changes and syncing them incrementally is a composition layer above the SDK, not part of the SDK itself. If it gets built later, it builds on the SDK surface this plan establishes.
- **No rollback machinery, DesiredState tracking, or workspace coordination.** These are application-layer concerns. Future composition layers may build them on substrate primitives, but the SDK doesn't pre-commit to any specific shape.
- **No write-back convenience APIs.** Bidirectional file movement (file in, modified file back) lives in the agent-to-agent layer when that gets built. The SDK exposes the primitives that the agent-to-agent layer composes; it doesn't bundle write-back as its own concept.
- **No retry, timeout, or progress policy.** The substrate's transfer engine has its own retry/timeout semantics for the wire-level operation. The SDK exposes what the substrate provides; applications wrap further policy if they want.
- **No persistence of transfer state beyond what the substrate provides.** The substrate's adapter already auto-stores fetched chunks. The SDK doesn't add a separate transfer cache or progress checkpoint.

The discipline is the same one applied across the SDK already: expose substrate primitives, document them well, let applications compose. Don't accumulate policy at the SDK layer that belongs in applications.

## Design preserving option value

Three directions the SDK should accommodate without building them now:

**Directory sync with file watching.** If this gets built later as a composition layer, it needs to be able to walk a directory, build a `DirManifest`, compare it against a previous manifest, identify changed entries, and transfer only the changed leaves. The SDK should expose enough of `DirManifest`'s internals (entry iteration, content-hash access per entry) that this composition is natural. The current `DirManifest` struct already provides this; the SDK just needs to re-export it cleanly.

**Write-back as part of agent delegation.** Agent A sends agent B a file as delegation input; B modifies it; B sends the modified version back. The SDK needs to support: blob storage on B's side (use `MeshBlobAdapter::store`), fetch from B back to A via the same transfer mechanism that did the forward direction. The substrate already supports this symmetrically. The SDK exposing both `fetch_blob` and the substrate's `publish_with_blob` (already publicly exposed) is sufficient; no new API surface is needed.

**Composition with the eventual durable operations layer.** When durable operations gets built (parked plan in `DURABLE_OPERATIONS_PLAN.md`), it will use blob transfer for large operation payloads. The SDK's transfer surface should be callable from inside that future composition layer without modification. The plan's API shape (returns a future, exposes progress as needed, doesn't require global state) supports this naturally.

These directions are noted, not built. The SDK API shape this plan defines doesn't foreclose any of them and doesn't pre-commit to any of them.

## Language tier model

The substrate maintains five language tiers with a deliberate dependency structure:

**Rust** is the native tier — the substrate itself, the SDK crate (`net_sdk`), all internal types accessible without translation overhead.

**C** is the foundation FFI tier. The `net` crate is built as `cdylib` + `staticlib` (per current `Cargo.toml`). C headers in `net/crates/net/include/` (`net.h`, `net_cortex.h`, `net_deck.h`, `net_meshdb.h`, `net_meshos.h`, `net_rpc.h`) plus `net.go.h` for CGO consumers define the canonical C ABI. Error codes in `net_error_t` are explicitly synchronized across headers with a regression test that detects drift. The C tier exists because it's the universal binding target — languages without first-class Net bindings (Java/Kotlin via JNI, Ruby via FFI, Lua, Zig, Crystal, Swift, anything else that can call C) use the C headers directly.

**Python** uses pyo3 directly, not CGO. The `net.tool` and `net.mesh_rpc` modules are built via maturin and re-exported through `net_sdk.tool` etc. for the canonical SDK import path.

**TypeScript** uses napi-rs directly, not CGO. The `_internal.ts` module is produced by the napi-rs build and re-exported through the per-concept SDK modules (`stream.ts`, `tool.ts`, etc.).

**Go** uses CGO through the C tier. The `bindings/go/net/` directory wraps the C ABI in idiomatic Go. This is why C is a prerequisite for Go, not parallel to it.

Adding transport SDK support means delivering all five tiers. C is not a tier that can be skipped — it's the foundation that Go (and any future C-ABI-consuming language binding) rides on.

## API shape per language tier

### Rust SDK (`net_sdk::transport` — new module)

Add a new `transport.rs` module to the SDK alongside `dataforts.rs`. The split: `dataforts` continues to expose the read-side and operator-side surface (metrics, inventory, the adapter constructor); `transport` exposes the new transfer primitives.

The module re-exports `BlobTransferEngine`, `TransferControl`, `TransferHeader`, the stream-id helpers, plus the directory types (`DirError`, `DirEntry`, `DirManifest`, `DirStats`, `EntryKind`) and `store_dir` / `fetch_dir`. It adds two high-level convenience wrappers: `fetch_blob(adapter, blob_ref) -> Bytes` and `fetch_blob_stream(adapter, blob_ref) -> impl Stream<Item = Bytes>`, plus a `TransferError` enum that translates substrate-level errors into a stable SDK-facing shape (`NotFound`, `AllPeersFailed`, `HashMismatch`, `Substrate(...)`).

Add `pub mod transport;` to `lib.rs`. Update the doc comment in `dataforts.rs` to note that the transfer surface lives in `transport` (since `dataforts` is no longer the only place blob-related types live).

### C FFI tier (`net/crates/net/include/net_transport.h` — new header)

New C header alongside the existing six. Following the conventions established by `net.h` and `net_meshdb.h`: opaque handles, error code returns, output parameters for results, caller-owned strings/bytes with explicit free functions.

The error code namespace extends `net_error_t` with transport-specific codes below the existing CortEX/RedEX range (`NET_ERR_TRANSFER_NOT_FOUND = -200`, `NET_ERR_TRANSFER_HASH_MISMATCH = -201`, `NET_ERR_TRANSFER_ALL_PEERS_FAILED = -202`, `NET_ERR_TRANSFER_CANCELLED = -203`, `NET_ERR_DIR_INVALID_MANIFEST = -210`, `NET_ERR_DIR_PATH_INVALID = -211`, `NET_ERR_DIR_SYMLINK_UNSUPPORTED = -212`). The exact code numbers are subject to refinement during implementation to fit the existing scheme; what matters is the namespace below the existing CortEX range and above the catchall `NET_ERR_UNKNOWN`.

Opaque handles: `net_blob_adapter_t` (already exists in the broader substrate FFI; transport uses it), `net_transfer_handle_t` (in-flight transfer, consumed by await or cancel), `net_dir_manifest_t` (read-only manifest result, freed by dedicated free function).

C functions to provide:
- `net_fetch_blob(adapter, hash, out_bytes, out_len) -> int` — synchronous fetch.
- `net_fetch_blob_async(adapter, hash, out_handle) -> int` + `net_transfer_await(handle, out_bytes, out_len) -> int` + `net_transfer_cancel(handle) -> int` — async fetch with handle-based lifecycle.
- `net_store_dir(adapter, root_path, out_hash) -> int` — store a local directory and return its manifest root hash.
- `net_fetch_dir(adapter, root_hash, dest_path, out_files, out_bytes) -> int` — reconstruct a directory from a manifest.
- `net_dir_manifest_read(adapter, root_hash, out_manifest) -> int` + `net_dir_manifest_free(manifest)` — manifest introspection for applications that want to walk before reconstructing.

Plus the corresponding `net_free_bytes` (or equivalent existing free function) for byte buffers returned by the FFI.

Rust-side FFI implementation: `#[no_mangle] extern "C"` functions in the substrate crate (probably a new `dataforts/blob/transfer_ffi.rs` and `dataforts/dir_ffi.rs`, or folded into the existing modules). Translation between Rust error types and `net_error_t` codes. Opaque handle lifecycle (heap allocation, free functions, NULL safety, idempotent free).

Update the error-code regression test that scans `net.h` and `net.go.h` for drift; extend it to cover `net_transport.h`.

A C example in `examples/transport.c` demonstrating blob fetch and directory transfer, matching the style of `examples/meshdb.c` and `examples/meshos.c`.

The C work is real engineering — opaque handle lifecycle, error code translation, ensuring the FFI surface is sound under panic and across thread boundaries, NULL safety, idempotent free semantics. Probably 400-500 LoC of Rust FFI code plus the C header plus the example plus the regression test extension. Two to three days of focused work.

### Python SDK (`net_sdk.transport` — new module)

A new `net_sdk/transport.py` that re-exports from `net.transport` (the pyo3-built module). Module-level docstring explaining the surface, what architectural property it provides (per-operation primitives, fairscheduler multiplexing, content-addressed dedup), and a reference to substrate docs.

The re-exports mirror the Rust SDK surface: `fetch_blob`, `fetch_blob_stream`, `store_dir`, `fetch_dir`, `DirManifest`, `DirEntry`, `DirStats`, `EntryKind`, `BlobTransferEngine`, `TransferControl`, `TransferHeader`, the stream-id helpers, and `TransferError`.

Requires adding a `net.transport` module to the pyo3 bindings (in `net/crates/net/bindings/python/python/net/`) exposing the same types. Following the pattern already established by `net.tool` and `net.mesh_rpc`.

Note: Python bindings use pyo3 directly, not CGO. The pyo3 binding is independent of the C tier — pyo3 wraps Rust types into Python classes natively without going through the C ABI.

The pyo3 binding work: probably 200-300 LoC of Rust wrapping the substrate types in pyo3 classes, plus an equivalent Python facade in `net.transport`.

### TypeScript SDK (`net_sdk/transport.ts` — new module)

Re-exports from `_internal.ts` (the napi-rs facade), following the pattern of `stream.ts`, `tool.ts`, etc. Same set of types and functions as the other tiers, with the napi-rs binding code producing the native module.

Note: TypeScript bindings use napi-rs directly, not CGO. Independent of the C tier for the same reason as Python.

### Go SDK (`net/crates/net/bindings/go/net/transport.go` — new file)

Go binding wraps the C ABI through CGO. Depends on the C tier being shipped.

The CGO include block references both `net.h` (for shared types like the blob adapter handle) and the new `net_transport.h`. The Go-facing API is idiomatic Go: `FetchBlob(adapter, blobRef) ([]byte, error)`, `FetchBlobStream(adapter, blobRef) (BlobStream, error)`, `StoreDir(adapter, root) (BlobRef, error)`, `FetchDir(adapter, rootRef, dest) (*DirStats, error)`, plus the manifest introspection wrappers.

Error translation: a `TransferError` struct that maps the C error codes to a Go-side error type, following the pattern the existing Go bindings use.

Handle lifecycle: the CGO wrappers handle allocation and deallocation, with `runtime.SetFinalizer` as a safety net for cases where callers forget explicit cleanup (matching existing Go binding conventions).

The Go binding work: one to two days once T-C (C FFI) is shipped, because most of the engineering decisions (handle model, error semantics, lifecycle rules) are settled by the C layer.

## Cross-language byte-compatibility

Same testing pattern as the existing tool calling layer: golden vectors verify that `TransferControl` and `TransferHeader` wire encoding is byte-identical across Rust, C (which Go consumes), Python, and TypeScript. The substrate's existing cross-language test infrastructure handles this; the new transfer types need their own golden vectors added.

Probably 50 LoC of vector definitions in the test suite, exercising the encode and decode paths in each language against the same expected bytes.

## Documentation

Each language's SDK needs documentation for the new module. Following the existing convention:

- One-paragraph module-level doc explaining what the surface is for and what architectural property it provides (per-operation substrate primitives, fairscheduler multiplexing, content-addressed dedup).
- Example for the common case: fetch a blob, transfer a directory.
- Note about when to reach for `BlobTransferEngine` directly versus using the high-level wrappers.
- Cross-reference to the directory transfer demo (when that exists) and to the related substrate docs.

The C header gets the most extensive documentation since it's the contract that downstream language bindings (Go now, others potentially later) rely on. The header file itself includes the handle model, error model, build instructions, and link instructions — following the established convention in `net.h` and `net_meshdb.h`.

The doc work matches the substantive doc work the engineering deserves: brief, technical, oriented to engineers reading the code who want to know what's there and how to use it.

## Implementation order

T-A: Rust SDK `transport.rs` module with re-exports and the two high-level fetch wrappers. Builds on what's already there in the substrate. Smallest piece; do first. Half a day.

T-B: Cross-language golden vectors for `TransferControl` and `TransferHeader`. Locks the wire format before the bindings ship. Half a day.

T-C: C FFI layer. New `net_transport.h` header, Rust-side `extern "C"` wrappers with handle lifecycle and error-code translation, regression test extension for error code drift, C example in `examples/transport.c`. Foundation for Go binding. Two to three days.

T-D: Python `net.transport` pyo3 bindings + `net_sdk.transport` facade. Independent of T-C; can be parallelized. Two to three days.

T-E: TypeScript `transport.ts` + napi-rs binding code. Independent of T-C; can be parallelized with T-D. Two to three days.

T-F: Go `transport.go` CGO binding over the C ABI from T-C. Depends on T-C completion. One to two days once T-C is shipped.

T-G: Documentation across all five tiers. One day spread across the SDKs, including the C header documentation (which is the most substantive).

T-H: Test coverage — unit tests in each binding verifying the wrappers work, cross-language behavioral tests for the golden vectors, error code regression test updates. Two to three days.

Total: roughly 10-14 days of focused work, depending on how much polish each binding gets. The C work is the new addition relative to the previous version of this plan; it's also the prerequisite for Go, which is why ordering matters.

Parallelization opportunities: T-D (Python) and T-E (TypeScript) can run alongside T-C (C). T-F (Go) waits on T-C. If two engineers are on this, one can do T-A + T-B + T-C + T-F (the C-and-Go branch) and the other can do T-D + T-E (the Python-and-TS branch), with T-G and T-H shared across.

Serialized by a single engineer: about two weeks of focused work. With parallelization: about one week.

## What this enables, what it doesn't

**Enabled by this SDK work:**
- Hermes integration calling blob transfer for cross-machine file movement during agent delegation.
- The directory transfer demo we discussed (`node_modules` between paired machines) can be implemented through the SDK rather than through substrate-direct calls.
- Future composition layers (durable operations, agent-to-agent delegation, eventual directory sync if built) compose on a documented SDK surface rather than reaching into substrate internals.
- Application developers writing on Net can move bulk data between paired machines using a clean API in the language they're using (Rust, C, Python, TypeScript, or Go).
- Future C-ABI-consuming language bindings (Java/Kotlin, Ruby, Lua, Zig, Swift, etc.) can use the transport surface through the C headers without waiting for first-class bindings.

**Not enabled by this SDK work (deliberately, as separate workstreams):**
- Anything resembling the workspace coordination patterns explored in the Kyra transcript exploration. Those are composition layers above this SDK, not part of it.
- Write-back convenience APIs as their own concept. The substrate primitives this SDK exposes support write-back patterns when applications compose them; the agent-to-agent layer will compose these specifically when it gets built.
- Directory sync, file watching, or rollback semantics. These are noted as possible future composition layers; the SDK shape this plan defines doesn't foreclose them or build them.

## Origin and relationship to other plans

This plan was prepared as a follow-up to the fairscheduler transport substrate work that merged on May 31, 2026 (PR #265). It builds on the substrate primitives that PR shipped and extends them to the SDK boundary so application code can use them through the canonical import paths in all five language tiers.

Related parked plans:
- `DURABLE_OPERATIONS_PLAN.md` — composition layer that will use blob transfer for large operation payloads.
- `STREAM_RETRANSMIT_PLAN.md` — substrate-side hardening already in flight.
- Future agent-to-agent delegation plan — will compose blob transfer for file-shaped delegation inputs and outputs.

Not directly related but worth noting:
- The directory transfer demo at `node_modules` scale that the broader strategic conversation has identified as architecturally important. This SDK work is what makes that demo accessible from application code; the demo itself is application code that uses the SDK.

## Revision history

- v1 (initial): four-language plan (Rust, Python, TypeScript, Go) with Go treated as parallel to Python and TypeScript.
- v2 (this version): corrected to five-language plan recognizing C as the foundation FFI tier. The `net` crate ships `cdylib` + `staticlib`; six C headers already exist; Go rides on the C ABI via CGO rather than having its own native binding. C support is not optional — it's the universal binding target that Go uses directly and that future C-ABI-consuming languages can use without waiting for dedicated bindings.
