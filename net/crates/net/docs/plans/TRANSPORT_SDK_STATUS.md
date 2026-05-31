# Transport SDK — implementation status & handoff

Companion to `TRANSPORT_SDK_PLAN.md`. Records what landed, the decisions
that deviate from the plan (and why), and the work deferred to a
follow-up PR that runs in the per-language CI environments. Prepared
during the initial implementation pass (Rust + wire format + C ABI +
Python + TypeScript + Go reference binding).

## Status by task

| Task | Status | Verified by |
|------|--------|-------------|
| **T-A** Rust SDK `net_sdk::transport` | ✅ done | `cargo test -p net-mesh-sdk` (unit + doctest), clippy |
| **T-B** Cross-language golden vectors | ✅ done (fixture + Rust) | `cargo test --test transfer_golden_vectors` |
| **T-C** C FFI (`net_transport.h`) | ✅ core done; async trio + stubs deferred | full-feature build, `ffi::transport` unit tests, drift test |
| **T-D** Python (pyo3) | ✅ done (full surface) | `cargo check -p net-python`, `py_compile` |
| **T-E** TypeScript (napi) | ✅ done (wire types + helpers + node-driven `NetMesh` methods) | `cargo check -p net-node` |
| **T-F** Go (cgo) | ✅ reference binding written | `gofmt` (no cgo toolchain here to compile) |
| **T-G** Docs across tiers | ◑ inline module docs in every tier + this overview | n/a |
| **T-H** Tests (cross-language behavioural) | ◑ Rust done; per-language deferred to CI | — |

## What landed, by tier

- **Rust** — `sdk/src/transport.rs`: re-exports (engine, `TransferControl`/
  `TransferHeader`, stream-id helpers, dir types, `store_dir`), `TransferError`
  (+`From<BlobError>`/`From<DirError>`), and `fetch_blob` / `fetch_blob_stream` /
  `fetch_blob_discovered` / `fetch_dir` / `serve_blob_transfer` over `Mesh`.
  Wired in `lib.rs`.
- **Wire format** — `tests/cross_lang_transfer_formats/transfer_vectors.json`
  (canonical postcard vectors) + `sdk/tests/transfer_golden_vectors.rs`
  (assert + `#[ignore]` regenerator).
- **C ABI** — `include/net_transport.h`, `src/ffi/transport.rs`
  (`net_serve_blob_transfer` / `net_fetch_blob` / `_discovered` /
  `net_store_dir` / `net_fetch_dir` / `net_dir_manifest_read` /
  `net_transport_free_buffer`), `tests/transport_error_codes.rs` (drift),
  `examples/transport.c`. Two substrate accessors added: widened
  `ffi::mesh::mesh_node_arc` to `dataforts`, added `ffi::blob::blob_adapter_arc`.
- **Python** — `bindings/python/src/transport.rs` (pyo3 wire types +
  helpers + node-driven functions), registered in `lib.rs`; facade
  `sdk-py/src/net_sdk/transport.py`; `net/__init__.py` exports.
  `PyMeshBlobAdapter::inner_arc` accessor added.
- **TypeScript** — `bindings/node/src/transport.rs` (napi wire types +
  helpers + node-driven `NetMesh` methods: `serveBlobTransfer` /
  `fetchBlob` / `fetchBlobDiscovered` / `storeDir` / `fetchDir`),
  `sdk-ts/src/transport.ts` facade + `index.ts` exports.
  `MeshBlobAdapter::inner_arc` accessor added.
- **Go** — `bindings/go/net/transport.go` (cgo reference binding,
  idiomatic Go API + `TransferError` code mapping).

## Decisions that deviate from the plan (and why)

1. **Handles are node + adapter, not "adapter".** The plan's
   `fetch_blob(adapter, …)` shorthand doesn't match the substrate: transfer
   is driven by `MeshNode::transfer_fetch_chunk(holder, hash)` /
   `_discovered`, and the adapter's `fetch_chunk` is local-only. So every
   tier's fetch/dir ops take the **mesh node** handle; the store/serve ops
   take the **blob adapter** handle. Fetching also requires the engine
   installed (`serve_blob_transfer`), so that's exposed too.
2. **Directory manifest ref = encoded `BlobRef` bytes, not a 32-byte hash.**
   A `store_dir` result can be a multi-chunk `Manifest`, which a bare hash
   can't represent. `net_store_dir` / `StoreDir` / `store_dir` return the
   encoded `BlobRef`; the matching fetch/read take the same bytes.
3. **`net_dir_manifest_read` returns JSON**, not an opaque
   `net_dir_manifest_t` + iterator. Simpler and equally introspectable;
   the opaque-handle form can follow if a binding needs lazy walking.
4. **C surface gated on `net+dataforts+netdb+redex-disk`** — the existing
   `MeshBlobAdapterHandle` FFI needs the triple; the transport surface
   reuses it, so it rides the same quad (the natural full build).
5. **Wire types use the postcard form** (`TransferControl`/`TransferHeader`)
   across all tiers — that's the actual substrate wire codec, locked by the
   T-B golden vectors.

## Deferred to the follow-up PR (needs per-language CI)

- **C async trio** — `net_fetch_blob_async` / `net_transfer_await` /
  `net_transfer_cancel` (sound spawn-on-runtime + handle lifecycle). The
  synchronous surface covers the blocking call shape bindings need first.
- **C feature-off stubs** — `transport_stubs`-style symbols for builds
  without the quad, so an unconditional cgo linker resolves them. Lands
  with **T-F** (the first such linker).
- **SDK `MeshNode` (mesh.ts) delegation** — the node-driven ops now exist
  as methods on the napi `NetMesh` class, but the SDK-side `MeshNode`
  wrapper in `sdk-ts/src/mesh.ts` should add thin delegating methods (and
  a `napi build` must regenerate `@net-mesh/core`'s `.d.ts` before tsc
  sees them). Pure-TS follow-up.
- **T-H cross-language behavioural tests** — the Python / TypeScript / Go
  consumers of `transfer_vectors.json` (mirroring the existing
  `tool_event_golden_vectors` tests in each tier), plus a two-node
  round-trip per binding. All need a built artifact (wheel / native module
  / cgo lib), which the per-language CI provides.
- **`examples/transport.c` compile check** — `gcc -fsyntax-only` (no C
  toolchain on the authoring box).

## Verification performed in this pass

- `cargo build -p net-mesh --features net,dataforts,netdb,redex-disk` — clean.
- `cargo test -p net-mesh … --lib reliability` (33), `ffi::transport` (3),
  `--test transport_error_codes` (drift), `--test transfer_golden_vectors` (2),
  `-p net-mesh-sdk --lib transport` (3) + doctest — all pass.
- `cargo check -p net-python` / `-p net-node` — clean.
- `py_compile` (Python facade + `__init__`), `gofmt` (Go) — clean.
- Headers / C example / Go binding are convention- and drift-test-validated;
  not compiler-checked here (no C/cgo toolchain).
