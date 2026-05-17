# Phase 3 — Rust FFI Soundness Review (`ai2070-net`)

Scope: every file under `net/crates/net/src/ffi/` and the C headers in
`net/crates/net/include/`. Out of scope: language bindings, tests,
examples. The FFI surface uses a per-handle `HandleGuard` quiescing
protocol with an intentional outer-box leak (documented and load-bearing
for soundness) — that design is sound.

## Findings (severity-ordered)

### F-1 — `slice::from_raw_parts` with caller-supplied `len` and no `isize::MAX` guard (multiple sites)

- **File:line:**
  - `net/crates/net/src/ffi/cortex.rs:1171` — `net_redex_file_append`
  - `net/crates/net/src/ffi/cortex.rs:2897` — `net_netdb_open_from_snapshot`
  - `net/crates/net/src/ffi/mesh.rs:1768` — `net_mesh_subscribe_channel_with_token`
  - `net/crates/net/src/ffi/mesh.rs:2243` — `net_identity_sign`
  - `net/crates/net/src/ffi/mesh.rs:2323` — `net_identity_install_token`
  - `net/crates/net/src/ffi/mesh.rs:2425` — `net_parse_token`
  - `net/crates/net/src/ffi/mesh.rs:2452` — `net_verify_token`
  - `net/crates/net/src/ffi/mesh.rs:2476` — `net_token_is_expired`
  - `net/crates/net/src/ffi/mesh.rs:2509` — `net_delegate_token` (`parent_len`)
  - `net/crates/net/src/ffi/blob.rs:239` — `net_blob_publish` (`data_len`)
  - `net/crates/net/src/ffi/blob.rs:307` — `net_blob_resolve` (`payload_len`)
  - `net/crates/net/src/ffi/blob.rs:937, 945, 976, 1016` — `net_mesh_blob_adapter_{store,fetch,exists}`
- **Severity:** high
- **Bug class:** undefined behaviour / panic-across-FFI
- **What:** Every call site listed builds a slice from a C-owned
  pointer + caller-supplied `usize` length via
  `std::slice::from_raw_parts`. That intrinsic's safety contract
  requires `len <= isize::MAX`; violating it is immediate UB
  (debug builds will panic — that panic unwinds across `extern "C"`).
  `mod.rs:737, 787, 873, 1637` and `mesh.rs:1354, 1923` already gate
  this correctly with `if len > isize::MAX as usize`; the README at
  `include/README.md:1024` claims `net_redex_file_append`,
  `net_identity_install_token`, and `net_parse_token` "now reject
  `len > isize::MAX`" — they don't. The check is missing on every
  site enumerated above.
- **Repro / scenario:** A C caller passing a sign-extended `(size_t)-1`
  (e.g. forwarding a Go `int` that holds `-1` into `C.size_t`) hits
  immediate UB; under cgo this manifests as a debug-assert panic and
  cross-FFI unwind on debug builds, or silent OOB reads on release.
- **Fix sketch:** Add `if <len> > isize::MAX as usize { return
  NetError::InvalidJson.into(); }` (or a dedicated typed code) before
  each `from_raw_parts` call. Mirrors the pattern already in `mod.rs`.

### F-2 — `net_blob_publish` / `net_blob_resolve` returned buffer layout vs. `net_blob_free_buffer`

- **File:line:** `net/crates/net/src/ffi/blob.rs:259-263` and
  `:324-328`, freed by `:342-347`
- **Severity:** medium
- **Bug class:** allocator-layout mismatch / undefined behaviour on drop
- **What:** `publish` / `resolve` do
  `Vec<u8>::into_boxed_slice() → Box::into_raw → *mut u8`; the matching
  free reconstructs via
  `Box::from_raw(slice_from_raw_parts_mut(ptr, len))`. This works for
  the common path because `into_boxed_slice` shrinks to fit, but
  `MeshBlobAdapter::fetch` at `blob.rs:985-988` is a different shape:
  it does `bytes.into_boxed_slice()` then `as_mut_ptr()` + `len()` +
  `std::mem::forget(boxed)`. The buffer is then freed by
  `net_blob_free_buffer` which expects the matching layout — that
  works only because `Box<[u8]>` and the `slice_from_raw_parts_mut`-
  reconstructed Box share the same allocator size when `len ==
  capacity`. The contract relies on `into_boxed_slice` doing an exact
  shrink; if a future allocator change (or a manual swap to `Vec`
  raw-parts) breaks the `len == capacity` invariant, dropping the
  reconstructed Box is UB.
- **Repro / scenario:** Not currently reachable, but a one-line
  refactor swapping `bytes.into_boxed_slice()` for
  `Box::from_raw(Vec::leak(bytes).as_mut_ptr())` would silently break
  this. The brittleness is the bug.
- **Fix sketch:** Allocate every returned buffer through the same
  `std::alloc::Layout::array::<u8>(len)` path that `mesh.rs:alloc_bytes`
  (`mesh.rs:1965-2001`) + `net_free_bytes` (`mesh.rs:2018-2034`) use,
  so allocation + dealloc share an explicit layout independent of
  `Vec` / `Box` internals.

### F-3 — `OpaqueCtx` / `CallbackBlobAdapter` `unsafe impl Send + Sync` with no trust boundary on the FFI input

- **File:line:** `net/crates/net/src/ffi/blob.rs:449-450, 473-474`
- **Severity:** medium
- **Bug class:** soundness of `Send` / `Sync` impl
- **What:** `OpaqueCtx(*mut c_void)` carries an arbitrary C pointer
  across threads (tokio's worker pool, via `spawn_blocking`). The
  `unsafe impl Send for OpaqueCtx {}` / `Sync` is justified in the
  doc-comment by "the caller is responsible for thread-safety," but
  the FFI registration entry point (`net_blob_register_callback_adapter`,
  `blob.rs:694-735`) accepts `ctx: *mut c_void` with no API affordance
  for declaring thread-safety. A C caller that registers a non-thread-
  safe context (e.g. a Python `PyObject*` without the GIL, a Go
  `unsafe.Pointer` to a goroutine-local) will see the substrate hand
  that pointer to a different OS thread inside `spawn_blocking` and
  race. The unsafe impl is sound at the type level, but the contract
  is implicit and undocumented at the registration boundary.
- **Repro / scenario:** A binding wrapping a non-`Sync` callback table
  registers it; the next `MeshBlobAdapter::fetch` runs on a tokio
  worker that's not the registration thread; concurrent calls produce
  a data race in the C-side context.
- **Fix sketch:** Either (a) document the cross-thread requirement on
  the FFI signature (rename to `ctx_send_sync` or add a header
  comment demanding `Sync`), or (b) serialize all vtable calls behind
  a per-adapter `Mutex` so the Send/Sync claim doesn't depend on the
  caller's context.

### F-4 — `predicate.rs:220` `String::from_utf8_unchecked` on serde-json output

- **File:line:** `net/crates/net/src/ffi/predicate.rs:220`
- **Severity:** low
- **Bug class:** soundness (defence-in-depth)
- **What:** After encoding `predicate_to_rpc_header`, the value bytes
  are wrapped in `String::from_utf8_unchecked(value_bytes)`. Today
  the bytes are `serde_json::to_vec` output which is guaranteed
  UTF-8, so the call is sound. But the helper
  `predicate_to_rpc_header` lives outside this module — a future
  refactor that adds e.g. a length prefix, a binary postcard envelope,
  or any non-UTF-8 encoding would silently produce an invalid `String`
  and downstream operations (including `CString::new` in
  `write_string_out`) would either UB or hit the
  `Err(NulError)` path with attacker-controlled content.
- **Repro / scenario:** Requires a substrate-side change to the
  header encoder. Not reachable today; the comment notes this is
  "guaranteed valid UTF-8" without enforcement.
- **Fix sketch:** Use `String::from_utf8(value_bytes).map_err(|_|
  NetError::Unknown)?` (or `.expect_err` is wrong — return an error
  code). One additional UTF-8 validation pass on a header value is
  negligible against the surrounding JSON encoding.

### F-5 — `alloc_bytes` helper writes through `out_ptr`/`out_len` without local null check

- **File:line:** `net/crates/net/src/ffi/mesh.rs:1968-1973`
- **Severity:** low
- **Bug class:** robustness / encapsulation
- **What:** `alloc_bytes` writes `*out_ptr = null_mut(); *out_len = 0`
  on the `len == 0` branch with no internal null-check. Every current
  caller (`net_identity_issue_token`, `_lookup_token`,
  `net_delegate_token`) does the null check at the FFI entry, so this
  is safe today. The helper signature accepts raw pointers without
  declaring its precondition; a future caller forgetting the entry
  check will silently UB.
- **Fix sketch:** Either mark the helper `unsafe fn` (forcing each
  caller to acknowledge the contract) or add a defensive
  `if out_ptr.is_null() || out_len.is_null() { return
  NetError::NullPointer.into(); }` at the top.

### F-6 — `net_blob_register_fs_adapter` and `_unregister_adapter` are publicly `unsafe extern "C" fn` but every other entry is `extern "C"`

- **File:line:** `net/crates/net/src/ffi/blob.rs:135, 163, 182, 211, 284, 342, 695, 862, 905, 927, 966, 1007, 1041, ...`
- **Severity:** low
- **Bug class:** API consistency / no functional bug
- **What:** `blob.rs` uses `pub unsafe extern "C" fn` while every
  other module uses bare `pub extern "C" fn` (relying on the
  `#![allow(clippy::not_unsafe_ptr_arg_deref)]` at the parent
  module). The `unsafe` keyword does not change the C ABI — the
  generated symbol is identical — but C compilers ignore the
  qualifier and `cbindgen`/manual-headers won't reflect it. This is
  stylistic drift, not a UB hazard. Worth aligning before a future
  reader assumes `unsafe extern "C" fn` has different invariants
  than `extern "C" fn`.
- **Fix sketch:** Either drop the `unsafe` qualifier from the FFI
  entries in `blob.rs` to match the other modules, or apply it
  uniformly — the latter is the more accurate marker per Rust's
  2024-edition discipline. Pure style; no behavior change.

### Categories explicitly clean

- **Panic-across-FFI from `unwrap` / `expect` / `panic!` in
  production paths:** clean. Every production `unwrap` / `expect`
  found in `Grep` lives under `#[cfg(test)]`. The few non-test
  `unwrap` calls (`cortex.rs:501, 718`, `mesh.rs:836`) are
  `CString::new("").unwrap()` (empty string can never contain
  NUL) or `unwrap_or_else(|_| "{}".to_string())` — both infallible.
  `block_on` paths abort rather than panic on runtime-in-runtime.
  `runtime()` factories abort on builder failure. Every
  `extern "C" fn` that can panic during user-supplied callback
  dispatch (`net_blob_publish`, `_resolve`,
  `net_tasks_wait_for_token`, `net_memories_wait_for_token`) is
  wrapped in `catch_unwind`. `redis_dedup.rs` wraps every entry in
  `ffi_guard`.

- **Double-free of handle-internal allocations:** clean. Every
  `_free` is guarded by `HandleGuard::begin_free`'s
  `compare_exchange(false, true)` single-winner protocol; the outer
  box is intentionally leaked (documented and load-bearing — see
  `handle_guard.rs:30-45`). `ManuallyDrop::take` only runs on the
  winning caller. The `MeshArcOwned` RAII guard (`cortex.rs:381-413`)
  correctly ensures the `*mut Arc<MeshNode>` is dropped once on
  every error path.

- **Mismatched `Box::into_raw` / `Box::from_raw`:** clean. Every
  `Box::into_raw` traced has a matching `Box::from_raw` on either
  the free path or the take-and-drop path. The `Arc::into_raw` /
  `Arc::from_raw` discipline (only used inside `MeshArcOwned`) is
  symmetric.

- **`Send`/`Sync` impls on FFI handle types:** mostly clean. The
  cortex module pins inner-type bounds via the compile-time
  `assert_send_sync` block (`cortex.rs:257-268`). The only
  `unsafe impl` is on `OpaqueCtx` (F-3 above).

- **`extern "C-unwind"` vs `extern "C"`:** intentionally `extern "C"`
  everywhere; every potential panic site is covered by `catch_unwind`
  or `abort()`. No upgrade to `extern "C-unwind"` is required and
  doing so would change the C ABI.

- **`#[no_mangle]` collisions / wrong calling convention:** clean.
  Every `#[unsafe(no_mangle)] pub extern "C" fn` has a feature-gated
  counterpart (real impl vs. `NET_ERR_FEATURE_NOT_BUILT` stub) so
  exactly one definition reaches the cdylib in any build
  configuration. CR-22 test (`mod.rs:2121-2196`) pins header-Rust
  enum parity.

- **C header layout assertions:** clean. `NetReceipt` (16 B / align 8)
  and `NetEvent` (48 B / align 8) carry compile-time
  `assert!(size_of:: ...)` constants (`mod.rs:1527-1592`). The
  C struct shapes in `net.h:79-100` line up with the Rust
  `#[repr(C)]` declarations.

- **Handle quiescing protocol:** clean. `handle_guard.rs` is sound
  by inspection — Dekker-style SeqCst ordering on `(active_ops,
  freeing)` is correct, and the outer-box leak rule eliminates the
  remaining hazard. Tests exhaustively pin every transition
  (`handle_guard.rs:209-391`).

- **NUL-termination / interior NUL handling:** clean. Every
  Rust-owned-string-to-C path routes through `CString::new(...)`
  with the typed `NetError::InteriorNul` (-11) variant for
  interior-NUL inputs (`mod.rs:386, 1789-1791`). Headers carry
  matching `NET_ERR_INTERIOR_NUL`.

- **`misaligned pointer / handle_is_valid`:** clean. `NetHandle`
  derefs are gated by an explicit `is_multiple_of(align_of)` check
  (`mod.rs:346-348`). The cortex / mesh modules don't have an
  alignment gate, but their handles are always produced by
  `Box::into_raw`, which guarantees alignment.

## Summary

Six findings, of which one (F-1) is high-severity and reaches into
nine FFI entry points. The other five are medium / low and represent
brittleness / future-proofing concerns rather than active UB. The
overall shape of the FFI — handle guard quiescing, intentional outer-
box leak, abort-on-runtime-in-runtime, `catch_unwind` at every
panic-reachable boundary, explicit `extern "C"` discipline — is
sound. The remaining gaps are isolated `from_raw_parts` sites where
the existing `isize::MAX` guard pattern was not consistently applied.
