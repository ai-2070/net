# Phase 3 — Umbrella Findings (2026-05-18 audit)

**Crate:** `ai2070-net v0.18.0`
**Inputs:** four module reviews + Phase 1 automated passes.
- [`PHASE1_REPORT.md`](./PHASE1_REPORT.md) — clippy/check/fmt sweep
- [`PHASE3_FFI_REVIEW.md`](./PHASE3_FFI_REVIEW.md) — `src/ffi/` + `include/`
- [`PHASE3_BUS_SHARD_REVIEW.md`](./PHASE3_BUS_SHARD_REVIEW.md) — `src/bus.rs` + `src/shard/`
- [`PHASE3_CONSUMER_REVIEW.md`](./PHASE3_CONSUMER_REVIEW.md) — `src/consumer/`
- [`PHASE3_REDEX_DISK_4GIB.md`](./PHASE3_REDEX_DISK_4GIB.md) — F2 follow-up

## Headline

The crate is in unusually solid shape: zero default-clippy code warnings, clean format, clean feature matrix, no `mem::transmute` in production, `catch_unwind` discipline at every FFI boundary, handle-quiescing protocol verified sound, ring-buffer SPSC ordering verified correct, no locks held across `.await`, every `tokio::spawn` has its `JoinHandle` captured. The Phase-1-flagged 4-GiB disk corruption hazard (F2) was a false alarm — a `MAX_SEGMENT_BYTES = 3 GiB` cap and an upstream `offset_to_u32` guard make the cast lossless by construction.

Two real bugs of consequence emerged, plus a high-severity gap reaching 14 FFI entry points.

## Critical / high — fix promptly

### C-1 — `ShardMetricsCollector` is silently inert on the production hot path
- **Source:** Bus/shard F-1.
- **File:line:** `src/shard/mod.rs:146-189` (`Shard::try_push_raw` / `Shard::try_push`).
- **What:** `record_push` / `record_buffer_len` are never called from production — only from tests. Downstream consumers of those counters read zero forever:
  - `finalize_draining` (`mapper.rs:1185-1207`) sees `pushes_since_drain_start == 0` always; its "shard actually empty" predicate is a no-op and it finalizes any Draining shard after the 100 ms timer regardless of contents. Only the bus's `remove_shard_internal` stranded-flush prevents event loss today.
  - `evaluate_scaling` reads `fill_ratio == 0`, `event_rate == 0` for every shard, so the "underutilized" autoscale trigger matches every Active shard every tick, masked only by warmup + cooldown.
- **Fix sketch:** wire `record_push` and `record_buffer_len` into `try_push_raw` / `try_push` (one call each); the field is already plumbed. Add a regression test that polls `ShardMetricsCollector` after N pushes and asserts non-zero.

### H-1 — `slice::from_raw_parts` without `isize::MAX` guard at 14 FFI entry points
- **Source:** FFI F-1.
- **File:line:** see FFI review F-1 for the full list (`cortex.rs:1171, 2897`, `mesh.rs:1768, 2243, 2323, 2425, 2452, 2476, 2509`, `blob.rs:239, 307, 937, 945, 976, 1016`).
- **What:** `slice::from_raw_parts` requires `len <= isize::MAX`. The pattern is already correctly applied at `mod.rs:737, 787, 873, 1637` and `mesh.rs:1354, 1923` — the 14 sites above missed it. A C caller forwarding `(size_t)-1` (e.g. a Go `int = -1` sign-extended through cgo) hits immediate UB. Worse: `include/README.md:1024-1027` documents three of these (`net_redex_file_append`, `net_identity_install_token`, `net_parse_token`) as "now reject `len > isize::MAX`" — the doc is wrong.
- **Fix sketch:** add `if len > isize::MAX as usize { return NetError::InvalidJson.into(); }` (or a dedicated typed code) before each `from_raw_parts` site. Then either update or remove the misleading README paragraph. One-line fix per site; ~15 minutes total.

## Medium

### M-1 — Consumer `merge.rs` double-fetches on duplicate `shard_id` in poll request
- **Source:** Consumer F-1.
- **File:line:** `src/consumer/merge.rs:469-513`.
- **What:** `request.shards` is consumed verbatim. `vec![0, 0, 1]` issues two `poll_shard(0, …)` calls and may return duplicate events in the response payload (cursor stays correct, payload does not).
- **Fix sketch:** `shards.sort_unstable(); shards.dedup();` before the empty check.

### M-2 — `CompositeCursor::update_from_events` advertised but production `poll()` bypasses it
- **Source:** Consumer F-2.
- **File:line:** `merge.rs:223-272` (def) vs `:558-560, :746-760` (production cursor advance).
- **What:** `update_from_events` is the documented cursor-advance primitive — it routes through `compare_stream_ids` and refuses to advance across a backend format change. Production `poll()` writes the cursor with unconditional `nc.set(...)`, skipping the guard. A JetStream→Redis mid-stream migration overwrites the cursor with the new-format id and the documented protection never fires.
- **Fix sketch:** route the `nc.set` / Step-2 override through `update_from_events` (or a `pub(crate)` equivalent), or downgrade the public visibility + docstring on `update_from_events`.

### M-3 — Wall-clock `Instant` deadlines inside tokio-virtualized sleep loops
- **Source:** Bus/shard F-2 / F-3.
- **File:line:** `src/bus.rs:1680, 1684` (`manual_scale_down`), `src/bus.rs:2270, 2272` (drain worker `finalize_deadline`).
- **What:** Same anti-pattern that was fixed at `bus.rs:1388-1392`. Under `tokio::time::pause()` these loops spin until real wall-clock advances.
- **Fix sketch:** swap `std::time::Instant::now()` for `tokio::time::Instant::now()` in both locations.

### M-4 — `EventBus::Drop` calls `parking_lot::Mutex::lock()` on every shard
- **Source:** Bus/shard F-4.
- **File:line:** `src/bus.rs:1793` → `src/shard/mod.rs:696`.
- **What:** `total_pending_in_rings` takes each shard's `parking_lot` mutex. A drop running on a thread that already holds a shard lock — single-thread runtime + panic during shutdown — deadlocks.
- **Fix sketch:** in `Drop`, either use `try_lock_for(short)` or the lock-free atomic counters in `ShardManager::stats()`.

### M-5 — `net_blob_publish` / `net_blob_resolve` allocator-layout coupling fragile
- **Source:** FFI F-2.
- **File:line:** `src/ffi/blob.rs:259-263, 324-328`, freed at `:342-347`.
- **What:** Allocate with `Vec → into_boxed_slice → Box::into_raw`, deallocate via `Box::from_raw(slice_from_raw_parts_mut(...))`. Sound today because `into_boxed_slice` shrinks-to-fit, but the contract is implicit; a one-line refactor to `Vec::leak` would silently mismatch.
- **Fix sketch:** route returned buffers through the same explicit `std::alloc::Layout` path that `mesh.rs:alloc_bytes` / `net_free_bytes` already use.

### M-6 — `OpaqueCtx` carries C pointers across worker threads with no documented thread-safety contract
- **Source:** FFI F-3.
- **File:line:** `src/ffi/blob.rs:449-450, 473-474`.
- **What:** `unsafe impl Send + Sync for OpaqueCtx` is sound at the type level, but the registration entry point (`net_blob_register_callback_adapter`, `blob.rs:694-735`) takes a raw `*mut c_void` with no API affordance for declaring the caller's thread-safety. A C caller registering a non-thread-safe context (Python `PyObject*` without GIL, Go-routine-local pointer) will see the substrate hand that pointer to a different worker via `spawn_blocking` and race.
- **Fix sketch:** either document the cross-thread requirement on the C signature, or serialize vtable dispatch behind a per-adapter mutex.

## Low

### L-1 — `String::from_utf8_unchecked` on serde-json output
- **Source:** FFI F-4. `src/ffi/predicate.rs:220`. Sound today (serde-json output is valid UTF-8); replace with `from_utf8(...).map_err(...)` for trivial defence-in-depth.

### L-2 — `alloc_bytes` writes via raw pointers without internal null-check
- **Source:** FFI F-5. `src/ffi/mesh.rs:1968-1973`. Mark helper `unsafe fn` or add a defensive null check.

### L-3 — `blob.rs` uses `pub unsafe extern "C" fn`; other modules use `pub extern "C" fn`
- **Source:** FFI F-6. Style drift only — no functional impact. Normalize one way.

### L-4 — `Ordering::None` is non-deterministic across shards but undocumented
- **Source:** Consumer F-3. `merge.rs:605-611, 685-760`. Document in rustdoc or add stable secondary sort.

### L-5 — `failed_shards` recovery has no surfaced backlog hint
- **Source:** Consumer F-4. `merge.rs:567-577, 685-720`. Slow drain on recovery is silent to the caller.

### L-6 — Workspace `[profile]` sections silently ignored in 4 binding crates
- **Source:** Phase 1 F-1. `bindings/{node,python,go/compute-ffi,go/rpc-ffi}/Cargo.toml`. Move profiles to workspace root or delete dead sections.

### L-7 — Bus/shard subtleties documented for completeness
- **Source:** Bus/shard F-5..F-8. Publish-then-spawn ordering in `add_shard_internal`; off-by-one in lossy-shutdown reconciliation under a specific interleave; transient `events_dropped` overcount during `DropOldest` retries; cross-field non-atomicity of `collect_and_reset`. None are reachable as production bugs as far as the reviewer could prove, but worth tightening if touched again.

## Null results (explicitly clean)

These categories were searched and found clean — recording them so the next audit doesn't repeat the work.

- **Production float comparisons** (Phase 1 F-4): all 90 `clippy::float_cmp` warnings are in `#[test]` blocks asserting exact rational constants. No production `==` on `f32`/`f64`.
- **`mem::transmute` in production:** zero occurrences.
- **`unsafe impl Send/Sync`:** confined to FFI handle types (3 occurrences); all judged sound by the FFI reviewer except for the documentation gap in M-6.
- **`tokio::spawn` join-handle discipline:** every spawn captured + awaited with bounded timeouts (bus/shard review).
- **Locks held across `.await`:** none found in bus/shard.
- **Panic-across-FFI from `unwrap`/`expect`/`panic!`:** every production `unwrap` traced is either compile-time-safe (`CString::new("")`), infallible-fallback (`unwrap_or_else`), or wrapped in `catch_unwind` / `ffi_guard`.
- **`tokio::select!` cancellation safety:** zero `select!` blocks in `src/consumer/`; concurrency is `join_all`, cancel-safe at the outer boundary.
- **Double-free of handle-internal allocations:** every `_free` guarded by `HandleGuard::begin_free` single-winner `compare_exchange`.
- **`Box::into_raw` / `Box::from_raw` pairing:** every traced site has a matching free.
- **`#[no_mangle]` collisions:** feature-gated counterparts ensure exactly one definition per cdylib.
- **NUL-termination / interior-NUL handling:** every Rust→C string path goes through `CString::new(...)` with typed error.
- **Handle quiescing protocol:** Dekker-style SeqCst on `(active_ops, freeing)` correct by inspection; exhaustively tested.
- **`RingBuffer` SPSC ordering:** verified correct.
- **Shutdown SeqCst handshake** (`try_enter_ingest` / `shutdown_via_ref` / `drain_finalize_ready`): correct.
- **`redex/disk.rs` 4 GiB cast** (Phase 1 F-2): cap-bounded; cast is lossless by construction.
- **Filter recursion depth:** bounded by serde-json default (128); 10 k-depth JSON rejected.
- **Stream-id decade-rollover comparison:** pinned by JetStream + Redis tests.

## Suggested action order

1. **C-1** (wire shard metrics into the hot path) — 30 min + regression test. Single largest correctness gain.
2. **H-1** (14 `from_raw_parts` length guards + README fix) — 15 min.
3. **M-3 / M-4** (Instant-in-tokio-loops, drop-time mutex acquisition) — 1 hr.
4. **M-1 / M-2** (consumer dedup + cursor format-mismatch routing) — 1–2 hr.
5. **M-5 / M-6** (FFI allocator layout, OpaqueCtx contract) — 2–4 hr (design choice in M-6).
6. Lows can be batched into a single cleanup commit.

## Coverage gaps carried forward

These were in the original plan but not exercised in this pass — defer or schedule:

- **Phase 2** (Miri / ASan / TSan / fuzz): user-skipped this round. TSan + libfuzzer are Linux/macOS only and would need WSL or a Linux runner. Existing `fuzz/fuzz_targets/` is wired and ready when needed.
- **Capability / auth surface** (`tests/capability_*`, `channel_auth*`): P1 in the plan; no module review issued this round.
- **Adapter cfg-gated paths** (`src/adapter/` jetstream / redis): only build-checked, not code-reviewed.
- **Dep audit** (`cargo-audit` / `cargo-machete` / `cargo-deny` / `cargo-udeps`): tools not installed; needs user approval before global install.
- **Cross-language conformance (Phase 4)**: property tests across Rust/TS/Py/Go SDK boundaries — not started.

## Verdict

Two bugs worth fixing immediately (C-1, H-1), a handful of medium tightening items, and a small pile of lows. The codebase's overall hygiene is high — the prior bug-audit history visible in `docs/misc/` (BUG_56_57, BUG_104, BUG_130, BUG_148, BUG_153_154) shows the team has been doing this work consistently, which is why default-clippy is clean and the deepest bugs we found are "metric counter never wired" and "isize::MAX guard missed on 14 of 20 sites" rather than systemic issues.
