# Code review — Transport SDK (`transport-sdk` vs `master`)

Date: 2026-05-31
Branch: `transport-sdk`
Scope: 9 commits, +3,134 lines. Five-tier SDK surface (Rust → C FFI →
Python/pyo3 → TypeScript/napi → Go/cgo) exposing the substrate's
blob-transfer + directory-transfer primitives, plus cross-language
golden vectors (`transfer_vectors.json`) and an error-code drift test
(`tests/transport_error_codes.rs`).

Related docs: `docs/plans/TRANSPORT_SDK_PLAN.md`,
`docs/plans/TRANSPORT_SDK_STATUS.md`.

## Summary

Strong, disciplined work that matches existing SDK conventions. The FFI
handle-lifecycle reuse (`mesh_node_arc` / the new `blob_adapter_arc`,
both cloned under the handle guard so a concurrent `_free` can't
deallocate mid-call) is sound, panics are caught at every `extern "C"`
boundary, NULL / oversize-length args are rejected before any deref, and
the buffer alloc/free layouts are symmetric. **No memory-safety or logic
bugs were found in the implemented Rust / FFI code.** The findings below
are about cross-tier consistency and one linker gap.

Assumptions verified during review:

- **Golden vectors are correct.** Hand-checked the postcard encodings
  (enum variant tag + varint / `[u8; 32]`) against the fixture; all 9
  cases match.
- **`net_dir_manifest_read` decode path is consistent.** Substrate
  `store_dir` postcard-encodes `DirManifest` and stores it as a blob
  (`dir.rs:278`); `fetch_dir` decodes the same way (`dir.rs:355`); the
  FFI fetches the manifest blob and runs the same
  `postcard::from_bytes::<DirManifest>` → `serde_json`. Consistent.
- **Feature gating is sound.** Both Python and Node define
  `dataforts = ["...", "net", ...]`, so `#[cfg(feature="dataforts")] mod
  transport` (which imports `mesh_bindings::NetMesh`, gated on `net`)
  always compiles. The async `(&self, blob_ref: &BlobRef)` signature
  already exists in `blob.rs`, so the napi transport code follows a
  compiling precedent.

## Findings

### 1. Medium — Go reference binding can break thin-feature libnet links (no transport stubs)

`bindings/go/net/transport.go` (commit `b0ccd322f`) declares
`net_serve_blob_transfer` / `net_fetch_blob` / `net_store_dir` / etc. as
unconditional `extern`s. Those symbols only exist when libnet is built
with the full `net,dataforts,netdb,redex-disk` quad. The feature-off
stubs that would let a thin build resolve them were **deferred**
(`TRANSPORT_SDK_STATUS.md` lines 78-80 say they "Land with T-F" — but
T-F is exactly what landed here, without them).

Existing `blob.go` is safe because `ffi::blob_stubs` provides
`NET_ERR_FEATURE_NOT_BUILT` stubs for its symbols; `blob_stubs.rs` has
**no** transport equivalents (confirmed). Net effect: a Go consumer
compiling the `net` package against a libnet built without the quad now
gets unresolved-symbol link errors for the whole package, where before
adding `transport.go` it linked.

Impact is latent — only non-default builds of a reference binding not
yet in CI — but the stub work and the binding that needs it have drifted
out of sync.

**Action:** either add the transport feature-off stubs now (mirroring
`ffi::blob_stubs`), or document in `transport.go` + the status doc that
the binding requires the quad until the stubs land.

### 2. Low — Cross-tier inconsistency: `NotFound` vs `AllPeersFailed`

The plan, the Rust SDK (`TransferError::AllPeersFailed`), and the C / Go
layers (`NET_ERR_TRANSFER_ALL_PEERS_FAILED` / `"all-peers-failed"`)
deliberately distinguish "this holder lacks it" from "no connected peer
served it." But **Python** (`map_blob_err` → flat `TransferError`
string) and **Node** (`transfer_blob_err` → reason string) collapse
`fetch_blob_discovered` failures into a single opaque message. The
distinction the plan calls out as intentional isn't programmatically
reachable in two of the five tiers.

**Action:** re-tag discovery `NotFound` in the py / node paths, matching
`src/ffi/transport.rs:295`.

### 3. Low — Inconsistent out-param zeroing in the C FFI

`net_fetch_blob`, `net_fetch_blob_discovered`, and `net_store_dir` zero
their out-params on entry. `net_fetch_dir` and `net_dir_manifest_read`
do **not** — they only write on success. No bug today (the Go wrapper
zero-inits its locals and reads only on `OK`, and the header doesn't
promise the values are untouched on error), but the inconsistency
invites a C caller to read a stale `out_json` after an error.

**Action:** zero the out-params on entry for uniformity.

### 4. Low / informational — `-212` is silently skipped in the error band

Codes run `… NET_ERR_DIR_PATH_INVALID = -211`, then `NET_ERR_DIR_IO =
-213`. The plan reserved `-212` for `SYMLINK_UNSUPPORTED`; since
`DirError` has no symlink variant this is correct, but the gap is
undocumented in both `net_transport.h` and `src/ffi/transport.rs`. The
drift test correctly only checks declared codes, so it's not a test gap.

**Action:** add a one-line "`-212` reserved for future
symlink-unsupported" comment in both files.

### 5. Informational — cross-language guarantee is asserted-by-construction, not yet tested

`transfer_vectors.json` is exercised only by the Rust test; the Python /
TS / Go consumers of it (and the C-example compile, cgo compile, and
`.d.ts` regen) are deferred to per-language CI (T-H). The byte-identical
claim is sound *by construction* — every tier calls the same postcard
codec on the same substrate types — but isn't yet verified end-to-end.
This is a reasonable phased plan and `TRANSPORT_SDK_STATUS.md` is
explicit about it; flagging only so the "verified across tiers" claim is
read accurately until T-H lands.

## Minor observations (no action needed)

- `fetch_blob` for a multi-chunk `Manifest` fetches chunks sequentially
  (`await` per chunk), unlike `fetch_dir`'s bounded concurrency.
  Consistent with the plan's "no concurrency policy at the SDK layer,"
  and the discovered variant's doc-comment already warns about cost.
- The `fetch_blob_stream` `unfold` state machine (surface-error-then-
  terminate) is well-judged, and the inline comment explaining why
  `take_while` / `then` wouldn't work is a nice touch.

## Verdict

Mergeable. Finding #1 is the only one worth resolving or explicitly
tracking before this reaches a thin-build Go consumer; the rest are
polish / follow-up.

## Resolution (2026-05-31)

All findings addressed on `transport-sdk`, one commit each:

1. **Transport feature-off stubs** — `src/ffi/transport_stubs.rs` added
   (mirrors `ffi::blob_stubs`); Go maps `-107` → `"feature-not-built"`.
   Verified thin-config build + stub tests, and full-quad build with
   stubs compiled out (no symbol conflict). Also wrapped the latent
   missing-`unsafe` in the `blob_stubs` test now that the thin config is
   test-compiled. → `fix(ffi): transport feature-off stubs…`
2. **`NotFound` vs `AllPeersFailed` in py/node** — discovery `NotFound`
   re-tagged as "all peers failed" in both bindings; Node unit tests
   cover the re-tag + fall-through. → `fix(bindings): distinguish
   all-peers-failed…`
3. **Out-param zeroing** — `net_fetch_dir` / `net_dir_manifest_read`
   zero on entry; guarantee documented in `net_transport.h`. →
   `fix(ffi): zero out-params on entry…`
4. **`-212` reserved gap** — documented in both `net_transport.h` and
   `src/ffi/transport.rs`; drift test still green. → `docs(ffi): note
   -212 is reserved…`
5. **Cross-language vectors** — Node wire types pinned to the canonical
   `transfer_vectors.json` vectors via `cargo test`-time unit tests.
   Python's pyo3 codec needs a live GIL under `extension-module`, so a
   runnable test there isn't reasonable; that tier stays on
   `py_compile` + the deferred T-H behavioural test. → `test(node): pin
   transport wire types…`
