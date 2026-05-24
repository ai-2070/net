# nRPC v3: Bounded-mpsc Observer Dispatch + Unified Streaming Cancellation

Branch: `nrpc-v3-observer-and-cancel` (suggested).
Predecessor: [`NRPC_STREAMING_PARITY_AND_GO_BINDING.md`](./NRPC_STREAMING_PARITY_AND_GO_BINDING.md) — this plan revises that plan's locked decisions #1 and #2 from "documented-sync, ship later" to "ship in v3 across all three bindings."

Scope: close the two real DX gaps the v1 typed-nRPC surface left open before downstream consumers can file production-pinning issues against them. Both are small per-binding extensions ("~30 lines per binding" was the reviewer estimate that prompted this plan), but skipping them in v1 was load-bearing for shipping the larger plan on time. Doing them now — before the next downstream binding cuts — keeps the surface idiomatic on day one.

## Why now

1. **Observer mpsc + drop counter.** The v1 contract documents "callbacks must be cheap; the substrate dispatch thread blocks until the call returns." In a 3-month adoption cycle, that contract gets violated within the first week — a user wires a Prometheus exporter or a disk-flushing log sink into `setObserver`, the substrate dispatch thread pins, mesh-wide RPC latency spikes, and the issue lands as "the observer hook is broken." Retrofitting bounded-mpsc-with-drop-counter into an already-documented sync observer contract means either breaking the contract (callers who depended on synchronous fire ordering get confused) or layering a parallel `setObserverBuffered` path (twice the API surface for one shape). Flipping the default to mpsc-with-drop-counter for v3 keeps the surface single-shape and gives operators a free "observer drops since last snapshot" diagnostic.

2. **Unified streaming cancellation.** v1 shipped three different cancellation stories across the bindings — Node `close()`-only, pyo3 `close()` via the SDK's `close_notify`, Go ctx-honored-for-unary-but-not-for-streaming. The DX gap is real: every Node user discovers within an hour of writing a `callClientStream` loop that their `AbortSignal` is silently ignored. The fix is small — the napi `MeshRpc::call_client_stream` and `::call_duplex` need the same `cancel_token` shim the unary `call` already has (line range flagged at `bindings/node/src/mesh_rpc.rs:1572-1594`). Same for pyo3's `Cancellable` and Go's `ctx.Context`. Doing this now keeps `AbortSignal` and `context.Context` working from day one, which is what JS and Go users expect by reflex.

3. **Cancellation as a substrate primitive, not three parallel binding shims.** Today the napi binding owns its own `cancel_registry: HashMap<u64, AbortHandle>` (`bindings/node/src/mesh_rpc.rs:NEXT_CANCEL_TOKEN` + `lock_cancel_registry`), the pyo3 binding owns its own `Cancellable` pyclass + watcher pattern, and the Go FFI owns its own `cancel_registry` (`bindings/go/rpc-ffi/src/lib.rs:cancel_registry`). Three parallel implementations of the same idea, each with its own race-condition quirks (Q18 in the Go FFI, CR-13 in the napi binding, the close-notify-via-tokio-select! pattern in pyo3). Promoting `cancel_token: Option<u64>` to a field on the SDK's `CallOptions` lets the SDK own the registry once — bindings shed their per-binding cancel state, each becomes a thin pass-through, and any future caller (CLI, deck, custom Rust consumer) gets cancel-token semantics for free. The substrate cost is small: ~80 lines for a `Mesh::reserve_cancel_token` / `Mesh::cancel(token)` pair + a per-Mesh `cancel_registry` keyed by token, mirroring the napi binding's existing pattern but at the right layer. Without this, the v3 cancellation work would replicate the same indirection in three places, which is exactly what made v1 ship three diverged stories in the first place.

## Locked decisions for this plan

1. **mpsc bound = 1024 events per mesh.** Matches the existing `RpcResponseSink`'s pump-side mpsc bound (`mesh_rpc.rs:1326-1334`). Big enough that a momentarily-slow observer doesn't lose events under normal load; small enough that an actually-broken observer surfaces drops within seconds rather than minutes. Single shared queue per mesh (not per-binding-instance) so the drop counter is meaningful at the operator level.
2. **Drop counter is a single u64 on the snapshot, not per-service.** Observer dispatch is per-mesh, not per-service — bucketing the drop counter by service would require a second tier of mpsc queues with no diagnostic benefit. Add as `RpcMetricsSnapshot::observer_dropped_total`.
3. **Cancellation IS a substrate primitive.** Promote `cancel_token: Option<u64>` to a field on `net::adapter::net::mesh_rpc::CallOptions` and add a `Mesh::reserve_cancel_token() -> u64` + `Mesh::cancel(token)` pair that owns a single per-mesh `cancel_registry` keyed by token. Bindings stop owning their own registries; each binding's `AbortSignal` / `Cancellable` / `context.Context` watcher becomes a thin pass-through that mints + cancels the SDK-level token. The SDK's existing `call` / `call_service` / `call_streaming` / `call_client_stream` / `call_duplex` all honor `cancel_token` uniformly — Drop-on-cancel emits CANCEL on the wire via the existing SDK primitives (`UnaryCallGuard::Drop`, `ClientStreamCallRaw::Drop`, `DuplexCallRaw::Drop`).
4. **Migration path: bindings' existing token plumbing becomes pass-through.** The napi `lock_cancel_registry()`, pyo3 `Cancellable`, and rpc-ffi `cancel_registry` stay as user-facing surfaces (don't break the public API on each binding) but their internals delegate to the SDK's registry instead of holding their own state. The race fixes (Q18 / CR-13 / orphan-TTL) live at the SDK once instead of three times.

Tagged `[S | A | B | C | D | T]`:

- **S** — substrate / SDK changes (new `CallOptions::cancel_token` + `Mesh::reserve_cancel_token` / `Mesh::cancel`).
- **A** — observer mpsc + drop-counter (napi binding + pyo3 binding + C ABI / Go FFI).
- **B** — Node TS typed wrapper changes (re-wire `AbortSignal` for streaming; drop `stripSignal` for streaming entries).
- **C** — Python typed wrapper changes (extend `Cancellable` to streaming).
- **D** — Go typed wrapper changes (wire `ctx.Context` to streaming).
- **T** — cross-binding tests (fixture extensions + per-binding cancel + drop-counter).

---

## Status

| ID    | Pri | Area                | Title                                                                                          |
|-------|-----|---------------------|------------------------------------------------------------------------------------------------|
| C-S1  | H   | SDK substrate       | Add `cancel_token: Option<u64>` to `CallOptions` + `Mesh::reserve_cancel_token` / `Mesh::cancel(token)` + per-mesh registry; thread through `call` / `call_service` / `call_streaming` / `call_client_stream` / `call_duplex` |
| C-S2  | H   | SDK substrate       | SDK-level integration tests: per-call-shape cancel-mid-flight emits CANCEL on the wire; cancel-on-zero-token is a no-op; reservation+cancel race is safe |
| O-A1  | H   | napi binding        | Replace sync `NodeRpcObserver` with bounded-mpsc + drop counter; surface `observerDroppedTotal` in `metricsSnapshot` |
| O-A2  | H   | pyo3 binding        | Replace per-event `spawn_blocking` with bounded-mpsc + worker task + drop counter; surface in `metrics_snapshot` |
| O-A3  | H   | C ABI / Go FFI      | Replace direct dispatcher invocation with Rust-side bounded-mpsc + worker; surface drop counter in JSON of `net_rpc_metrics_snapshot` |
| C-A1  | H   | napi binding        | Migrate `lock_cancel_registry` to thin pass-through over `Mesh::cancel`; populate `opts.cancel_token` for streaming entries (delete the local AbortHandle registry) |
| C-A2  | H   | pyo3 binding        | Migrate `Cancellable` watcher to thin pass-through over `Mesh::cancel`; populate `opts.cancel_token` for streaming entries |
| C-A3  | H   | Go FFI              | Migrate rpc-ffi `cancel_registry` to thin pass-through over `Mesh::cancel`; add `net_rpc_call_client_stream_cancellable` + `net_rpc_call_duplex_cancellable` symbols populating `opts.cancel_token` |
| C-B1  | M   | Node TS wrapper     | `TypedMeshRpc.callClientStream` / `callDuplex` wire `AbortSignal` end-to-end (drop `stripSignal` for streaming) |
| C-C1  | M   | Python wrapper      | `TypedMeshRpc.call_client_stream` / `call_duplex` extract `opts['cancel']` and propagate                       |
| C-D1  | M   | Go wrapper          | `TypedCallClientStream` / `TypedCallDuplex` propagate `ctx` through the cancellable FFI variant                |
| O-T1  | M   | fixture + tests     | Update `golden_vectors_streaming.json::observer_invariants.firing_contract` for mpsc shape; add `observerDroppedTotal` to `metrics_snapshot_invariants`; document the SDK-level cancel-token contract in a new `cancellation_invariants` section |
| O-T2  | M   | tests               | Rust-side reference: drop counter increments under sustained load when the observer is intentionally slow |
| C-T1  | M   | tests               | Rust-side reference: cancel mid-stream observed by server as `RpcStatus::Cancelled` for client-stream + duplex |
| C-T2  | L   | per-binding tests   | Stub-level test in each binding: signal-aborted / cancellable-cancelled / ctx-cancelled streaming call propagates to `close()` on the inner call |

ABI version: this plan bumps `NET_RPC_ABI_VERSION` from `0x0003` → `0x0004` because of the new cancellable FFI symbols. Additive; 0x0003 consumers keep working. The new `CallOptions::cancel_token` field is also additive on the SDK side (existing `..Default::default()` callers continue to compile).

---

## Phasing

**Recommended order: substrate-then-bindings.** Observer (O-A*) is independent of the cancellation work and can land in parallel.

1. **Wave 1 — Observer mpsc dispatch (O-A1 / O-A2 / O-A3 in parallel).** Independent files; safe to land same PR cycle. No substrate dependency.
2. **Wave 2 — Substrate cancellation primitive (C-S1 → C-S2 sequentially).** C-S1 lands the `cancel_token` field + `Mesh::cancel` API + per-mesh registry; C-S2 is the SDK-level test suite pinning the contract. **Blocks Wave 3** — the binding migrations all depend on the SDK primitive being in place.
3. **Wave 3 — Binding cancellation migration (C-A1 / C-A2 / C-A3 in parallel, after Wave 2).** Each binding's existing cancel surface delegates to the SDK primitive instead of holding its own state. C-A3 also bumps the ABI version; downstream Go consumers update at the same cut.
4. **Wave 4 — Typed wrappers (C-B1, C-C1, C-D1 in parallel, after Wave 3).** Thin pass-throughs once the raw layers honor cancel.
5. **Wave 5 — Fixture + tests (O-T1, O-T2, C-T1, C-T2).** Pin the new contract; per-binding tests land alongside their wrappers.

Wave 1 and Wave 2 can run in parallel (different code paths, no overlap). Wave 3 strictly follows Wave 2. Wave 4 can run alongside Wave 3 if the wrapper authors are comfortable working against the SDK-side primitive directly (they're stubs for `Mesh::cancel`).

---

## Wave 1 — Observer mpsc dispatch

### O-A1 — napi `NodeRpcObserver` → bounded mpsc + drop counter

**Rationale.** The v1 implementation at `bindings/node/src/mesh_rpc.rs:NodeRpcObserver::on_call` calls the TSFN directly in `NonBlocking` mode from the substrate dispatch thread. Three problems with this for production users:

1. The TSFN's internal queue has napi-rs's default size; on overflow the events drop silently — no observability.
2. The substrate dispatch thread still pays the TSFN-enqueue cost (one Mutex acquire per event in napi-rs's internal implementation).
3. Documenting "callbacks must be cheap" puts the burden on every user; the substrate has no defense against a user who violates the rule.

**Design.**
- Add a `bounded_mpsc::Sender<RpcCallEventJs>` to `NodeRpcObserver`, size 1024. Construct alongside the TSFN.
- Spawn ONE tokio task per observer install that drains the receiver and pumps each event to the TSFN. Task dies when the sender drops (which happens when `set_observer(None)` is called and the observer Arc is released).
- `on_call` does `try_send` on the channel. Full → `OBSERVER_DROP_COUNTER.fetch_add(1, Relaxed)` and return; never blocks. The dispatch thread's per-event cost drops from "TSFN Mutex acquire" to "atomic-counter inc on a single AtomicUsize."
- `OBSERVER_DROP_COUNTER` is a process-global `AtomicU64` (matches the existing `RPC_METRICS` ergonomics). The metrics snapshot reads-and-leaves; an alternative "reads-and-resets" semantics is rejected because the existing snapshot fields don't reset either, and Prometheus exporters prefer monotonic counters.
- Surface in `RpcMetricsSnapshotJs` as a new top-level `observerDroppedTotal: BigInt` field.

**Files touched.**
- `bindings/node/src/mesh_rpc.rs` — extend `NodeRpcObserver`, add the worker task, add the static `OBSERVER_DROP_COUNTER`, add the new field to `RpcMetricsSnapshotJs` + the conversion.
- `bindings/node/test/mesh_rpc.test.ts` — extend stub tests to cover the new field exists on `RpcMetricsSnapshot`.

**Test plan.**
- Stub-level: a stub TSFN whose `call` blocks indefinitely; 2000 `on_call` invocations from the test thread; assert the drop counter increments to ~976 (2000 - 1024) and the substrate dispatch never blocks. Live observer-firing tests land in O-T2.

**Risks.**
- The worker task needs a clean shutdown path. Drop the sender → channel closes → worker exits its drain loop. `set_observer(None)` does `arc_swap.store(None)` which drops the last Arc → drops `NodeRpcObserver` → drops the sender. Confirm with a test that the worker doesn't outlive the observer's lifetime.

### O-A2 — pyo3 `PyRpcObserver` → bounded mpsc + worker task

**Rationale.** Same as O-A1 for the pyo3 binding. The v1 implementation at `bindings/python/src/mesh_rpc.rs:PyRpcObserver::on_call` spawns a fresh blocking-pool task per event via `self.runtime.spawn_blocking(...)`. Under sustained load this drains the tokio blocking pool faster than user callbacks acquire-and-release the GIL. The blocking pool is bounded (tokio's default ~512 workers); past that, `spawn_blocking` queues internally with no observability. Same diagnostic gap.

**Design.**
- Same bounded-mpsc + single-worker pattern as O-A1. The worker task `spawn`s onto the runtime (not blocking-pool); when it drains an event it acquires the GIL once, calls the Python callable, releases. One worker = serialized GIL acquisition, matching Python's natural threading model.
- `OBSERVER_DROP_COUNTER` is a per-binding `AtomicU64`. Surface in `RpcMetricsSnapshot::observer_dropped_total` (pyclass attribute via `get_all`).
- Update the `mesh_rpc.py` typed wrapper's `_raw_metrics_snapshot_to_typed` to populate the new field on the dataclass.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — `PyRpcObserver` extension + drop-counter accessor + `RpcMetricsSnapshot` pyclass field.
- `bindings/python/python/net/mesh_rpc.py` — extend `RpcMetricsSnapshot` dataclass with `observer_dropped_total: int` field and the `_raw_metrics_snapshot_to_typed` mapping.
- `bindings/python/tests/test_mesh_rpc.py` — extend stub tests.

**Test plan.**
- Stub-level test mirroring O-A1's pattern: a Python callable that `time.sleep(10)`s, 2000 raw events fired through the dispatcher, assert drop counter ≈ 976.

### O-A3 — C ABI / Go FFI: bounded mpsc + worker

**Rationale.** Same as O-A1 / O-A2 for the C ABI consumed by the Go binding. v1 at `bindings/go/rpc-ffi/src/lib.rs::GoRpcObserver::on_call` calls the registered dispatcher synchronously from the substrate dispatch thread — same "callbacks must be cheap" footgun, exposed to Go consumers.

**Design.**
- Same bounded-mpsc + worker pattern. The worker invokes the C function pointer registered by `net_rpc_set_observer_dispatcher`.
- Add a new FFI function `net_rpc_observer_dropped_total() -> u64` so Go can read the drop counter without going through the JSON snapshot. The JSON snapshot also surfaces it as a top-level `observer_dropped_total` field.
- ABI version bump: this is the v3 surface; the next plan's substrate-level changes (if any) drive the v4 bump.

**Files touched.**
- `bindings/go/rpc-ffi/src/lib.rs` — `GoRpcObserver` extension; new `net_rpc_observer_dropped_total` symbol; JSON snapshot adds the field. ABI version stays at `0x0003` (additive: new symbol; no signature changes to existing functions).
- `bindings/go/net/mesh_rpc_typed.go` — extend `RpcMetricsSnapshot` Go struct with `ObserverDroppedTotal uint64` field; tag with `json:"observer_dropped_total"`. Optionally add a top-level `ObserverDroppedTotal(rpc *TypedMeshRpc) uint64` helper that calls the new FFI symbol directly without paying the JSON-decode cost.
- `bindings/go/net/mesh_rpc.go` — cgo `extern` decl for the new symbol.

**Test plan.**
- Rust-side: `bindings/go/rpc-ffi/src/lib.rs` mod tests — a synthetic dispatcher that sleeps, 2000 fires, assert drop counter ≈ 976. The Go-side tests live alongside C-D1 (since cgo can't be exercised in this environment's test infrastructure).

---

## Wave 2 — Substrate cancellation primitive

### C-S1 — `CallOptions::cancel_token` + `Mesh::cancel(token)` + per-mesh registry

**Rationale.** Three bindings reinvent the same cancel-token registry today (napi, pyo3, Go FFI), each with its own race-condition fixes (Q18 / CR-13 / orphan-TTL pattern). Promote the primitive to the SDK once so the bindings shed their state.

**Design.**

- **New field on `net::adapter::net::mesh_rpc::CallOptions`:**
  ```rust
  /// Caller-side cancel token. Mint via [`Mesh::reserve_cancel_token`];
  /// pair with [`Mesh::cancel`] from any thread to abort the in-flight
  /// call. `None` (or `Some(0)`) → no cancel slot is reserved and the
  /// call has no abort path beyond Drop-on-future-cancellation.
  ///
  /// Honored by every call shape: `call`, `call_service`, `call_streaming`,
  /// `call_client_stream`, `call_duplex`. The substrate registers the
  /// token's abort handle in a per-mesh registry at call construction
  /// and removes it on call resolution (success, error, or Drop).
  pub cancel_token: Option<u64>,
  ```
- **New `Mesh` API:**
  ```rust
  impl Mesh {
      /// Reserve a fresh cancel token. Monotonically-increasing,
      /// process-global, never reused. An unused reservation is
      /// harmless (no entry inserted until paired with a call).
      pub fn reserve_cancel_token(&self) -> u64;

      /// Abort the in-flight call associated with `token`. Idempotent —
      /// no-op if the token was never used, the call already resolved,
      /// or the token == 0. Triggers Drop on the call's inner future,
      /// which fires CANCEL on the wire via the existing primitives
      /// (UnaryCallGuard::Drop, ClientStreamCallRaw::Drop,
      /// DuplexCallRaw::Drop).
      ///
      /// Race-safe: a cancel that arrives BEFORE the call's abort
      /// handle is registered (the gap between reserve and call
      /// construction) latches a `cancelled = true` flag on the
      /// orphan entry; when the call later registers, it observes
      /// the flag and aborts immediately. Mirrors the napi
      /// binding's CR-13 fix at the SDK layer.
      pub fn cancel(&self, token: u64);
  }
  ```
- **Registry implementation.** Per-Mesh `cancel_registry: parking_lot::Mutex<HashMap<u64, CancelEntry>>` where:
  ```rust
  struct CancelEntry {
      cancelled: bool,                          // CR-13: cancel before register
      handle: Option<tokio::task::AbortHandle>, // unary + streaming-construction
      close_notify: Option<Weak<tokio::sync::Notify>>, // streaming post-construction
      marked_at: Option<Instant>,               // Q18: orphan TTL
  }
  ```
  Lifted directly from the napi binding's existing pattern (`bindings/node/src/mesh_rpc.rs:NEXT_CANCEL_TOKEN` + `lock_cancel_registry`) — same shape, same race fixes, just at the SDK layer. Includes the orphan-TTL GC (default 120s) from the Go FFI's Q18 fix.
- **Per-call-shape integration.**
  - `Mesh::call` / `Mesh::call_service`: spawn the inner future inside `tokio::spawn`, register the abort handle keyed by `cancel_token` (when set), remove on resolution. Drop on registry-aborted = CANCEL on the wire via the SDK's existing UnaryCallGuard.
  - `Mesh::call_streaming`: the returned `RpcStream` already has Drop-on-drop CANCEL; register a `Weak<Notify>` against the stream's internal close-notify so `Mesh::cancel(token)` triggers the same drop path.
  - `Mesh::call_client_stream` / `Mesh::call_duplex`: same as `call_streaming` — register against the call's internal `close_notify` (the substrate's `ClientStreamCallRaw::close_notify` and `DuplexCallRaw::close_notify` already exist for the napi binding's `Notify::notify_one()`-on-close pattern).

**Files touched.**
- `net/src/adapter/net/mesh_rpc.rs` — `CallOptions::cancel_token` field + `Default` impl update.
- `net/src/adapter/net/mesh.rs` — `Mesh::reserve_cancel_token` / `Mesh::cancel` + `cancel_registry` field + GC method.
- `net/src/adapter/net/mesh_rpc.rs::call` / `::call_service` / `::call_streaming` / `::call_client_stream` / `::call_duplex` — registration + removal hooks at each call shape's construction site.

**Risks.**
- **Drop-on-cancel race in `call_client_stream`.** The streaming construction returns a handle; if cancel fires AFTER the future returns but BEFORE the caller binds the handle to a variable (a vanishingly-small window), the handle gets dropped on caller side anyway — no behavior change. If cancel fires DURING construction (the `block_on` inside the stream's `new`), the abort handle fires and the construction future drops cleanly. Drive both via integration tests in C-S2.
- **Orphan GC interval.** The 120s TTL matches the existing Go FFI's Q18 value. If a downstream user reserves tokens at a rate exceeding ~10/s for tokens that never get used, the registry grows to ~1200 entries before GC catches up. Acceptable; the registry is a single HashMap, not on any hot path.

**Migration.** The napi binding's `lock_cancel_registry`, pyo3's `Cancellable`, and rpc-ffi's `cancel_registry` all keep their public surface (so downstream consumers don't see API breakage). Their internals delegate to `Mesh::reserve_cancel_token` + `Mesh::cancel` instead of maintaining a local HashMap. This is the C-A1 / C-A2 / C-A3 work in Wave 3.

### C-S2 — SDK-level cancel-contract integration tests

**Rationale.** Pin the substrate-level cancel contract before any binding migration depends on it. Once C-A1 / C-A2 / C-A3 ship, the bindings test only their pass-through layer; the SDK tests pin the contract that the pass-through relies on.

**Test cases.**
- `cancel_unary_mid_flight_emits_cancel_on_wire` — `call` with `cancel_token = Some(t)`; concurrent `mesh.cancel(t)` after the request is in flight; assert the server-side handler observes CANCEL.
- `cancel_streaming_mid_drain_emits_cancel` — same for `call_streaming`.
- `cancel_client_stream_mid_send_emits_cancel` — same for `call_client_stream`.
- `cancel_duplex_mid_send_emits_cancel` — same for `call_duplex`.
- `cancel_before_construction_aborts_cleanly` — reserve token, immediately cancel, THEN issue the call; assert the call resolves to a `Cancelled` error without ever reaching the server.
- `cancel_after_resolution_is_noop` — reserve, issue, await success, then cancel; assert no crash, no double-CANCEL.
- `cancel_zero_token_is_noop` — `mesh.cancel(0)` is a no-op.
- `orphan_ttl_gc_evicts_unused_reservations` — pin the 120s TTL behavior (use `tokio::time::pause()` to skip the wait).

**Files touched.**
- `net/tests/integration_mesh_cancel.rs` — new file.

## Wave 3 — Binding cancellation migration (over the substrate primitive)

### C-A1 — napi: migrate `lock_cancel_registry` to substrate pass-through; populate `cancel_token` for streaming

**Rationale.** Once C-S1 lands, the napi binding's local `cancel_registry` is duplicative. Migrate it to a thin pass-through over `Mesh::cancel`, and populate `CallOptions::cancel_token` for streaming entries so they get the same semantics for free.

**Design.**
- `reserve_cancel_token` napi method → `self.node.reserve_cancel_token()` (delegates).
- `cancel_call(token)` napi method → `self.node.cancel(token)` (delegates).
- Delete the file-local `NEXT_CANCEL_TOKEN` AtomicU64 + the `lock_cancel_registry` HashMap.
- `call_client_stream` and `call_duplex` napi methods: extract `cancel_token` from incoming `CallOptions` and populate the inner `InnerCallOptions::cancel_token` field. The substrate side handles the rest.
- The user's typed wrapper drops `stripSignal` and instead uses `wireAbortSignal` for the streaming entries too — the existing helper at `mesh_rpc.ts:wireAbortSignal` already mints a token + registers a listener; with the napi raw side now honoring `cancel_token` on streaming calls, the wrapper is end-to-end.

**Files touched.**
- `bindings/node/src/mesh_rpc.rs` — delete `NEXT_CANCEL_TOKEN` + `lock_cancel_registry`; `reserve_cancel_token` / `cancel_call` become pass-throughs.
- `bindings/node/src/mesh_rpc.rs:call_client_stream / call_duplex` — populate `opts.cancel_token` when set.

### C-A2 — pyo3: migrate `Cancellable` watcher to substrate pass-through

**Rationale.** Same as C-A1 for pyo3. The `Cancellable` pyclass surface stays (user-facing), but its `cancel()` method delegates to `Mesh::cancel` instead of latching a local flag + waking a Notify.

**Design.**
- `Cancellable.__init__` calls `mesh.reserve_cancel_token()` (stash the mesh handle at construction).
- `Cancellable.cancel()` calls `self._mesh.cancel(self._token)`.
- `call_client_stream` / `call_duplex` extract `opts['cancel']` and populate `opts.cancel_token` on the inner `CallOptions`.
- The Notify-based `close_notify` path on `PyClientStreamCall` / `PyDuplexCall` stays as an internal implementation detail; the substrate registers a `Weak<Notify>` against it on registration (per C-S1's design).

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — `PyCancellable` delegation; `call_client_stream` / `call_duplex` populate `cancel_token`.
- `bindings/python/python/net/mesh_rpc.py` — docstring updates.

### C-A3 — Go FFI: migrate rpc-ffi `cancel_registry` to substrate pass-through; add cancellable streaming variants

**Rationale.** Same as C-A1 for the Go FFI. ABI bump still required for the new cancellable streaming symbols (so Go consumers can compile against them); the registry migration is internal.

**Design.**
- Delete the file-local `cancel_registry` HashMap + `CancelEntry` struct + orphan-TTL GC in `bindings/go/rpc-ffi/src/lib.rs`.
- `net_rpc_reserve_cancel_token` / `net_rpc_cancel_call` become pass-throughs to `Mesh::reserve_cancel_token` / `Mesh::cancel` (need a static `Mesh` handle — bind to the first-constructed MeshRpc, or thread the mesh handle through the call site explicitly).
- Add two new FFI functions:
  - `net_rpc_call_client_stream_cancellable(handle, target, service, deadline_ms, request_window, cancel_token, out_call, out_err) -> c_int`
  - `net_rpc_call_duplex_cancellable(handle, target, service, deadline_ms, stream_window, request_window, cancel_token, out_call, out_err) -> c_int`
  Both populate `InnerCallOptions::cancel_token` and forward to the SDK. No spawn/registry work at the FFI layer.
- ABI version bumps `0x0003 → 0x0004` because we're adding new exported symbols; existing 0x0003 symbols stay unchanged.

**Files touched.**
- `bindings/go/rpc-ffi/src/lib.rs` — delete the local cancel_registry; two new exported functions + ABI version constant + doc-comment update.
- `bindings/go/net/mesh_rpc.go` — cgo `extern` declarations; extend `CallClientStream` / `CallDuplex` to call the cancellable variant and install the cancel watcher.
- `bindings/go/net/mesh_rpc_typed.go` — `TypedCallClientStream` / `TypedCallDuplex` pass `ctx` through unchanged (the raw layer now honors it).

**Risks.**
- ABI version cascade: the reference `ExpectedABIVersion` pin in `mesh_rpc.go:586-595` flips from `0x0003` → `0x0004`. Downstream Go binding consumers compiled against `0x0003` panic at process init (`mesh_rpc.go:618-625`) — same cascade as v1's `0x0001 → 0x0003` bump. Release notes for the next downstream Go binding cut MUST call out the override env-var `NET_RPC_SKIP_ABI_CHECK=1` for in-development consumers.

---

## Wave 4 — Typed wrapper pass-throughs

### C-B1 — Node TS: wire `AbortSignal` for streaming

**Design.**
- Remove the `stripSignal` call from `callClientStream` and `callDuplex` in `bindings/node/mesh_rpc.ts`. Replace with `wireAbortSignal` (the same helper unary calls already use). The helper mints a token, attaches an abort listener that calls `raw.cancelCall(token)`, and pairs detach with the call's lifetime.
- The streaming entries' `opts.signal` now propagates end-to-end. Update the docstring to remove the "v1: close()-only" caveat.
- Streaming-typed-call's `close()` continues to work as the explicit-cancel surface — the two paths are complementary (signal is "ambient cancel context", close is "explicit drop now").

**Files touched.**
- `bindings/node/mesh_rpc.ts` — drop `stripSignal` usages in `callClientStream` + `callDuplex`; wire `wireAbortSignal` instead.
- `bindings/node/test/mesh_rpc.test.ts` — add streaming-cancel stub tests (signal-aborted causes `raw.cancelCall(token)` to fire).

### C-C1 — Python: extend `Cancellable` to streaming

**Design.**
- The pyo3 raw side now honors `opts['cancel']` for streaming (C-A2). The typed wrapper's `call_client_stream` / `call_duplex` accept and propagate `opts['cancel']` directly (no extraction work needed — the raw layer handles it).
- Update `mesh_rpc.py` docstrings to remove the "cancellation contract: close-only" caveat.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — docstring updates only; the wrapper already accepts `opts: Optional[dict]` and forwards.

### C-D1 — Go: wire `ctx` to streaming entries

**Design.**
- The Go raw side now honors `ctx` for streaming (C-A3). The typed wrapper's `TypedCallClientStream` / `TypedCallDuplex` already pass `ctx` through; with C-A3 landed, this becomes effectively zero-line — the existing signatures gain real cancel propagation under the hood.
- Update the doc-comments in `bindings/go/net/mesh_rpc_typed.go` to remove the "ctx used only for the wire deadline" caveat for streaming entries.

**Files touched.**
- `bindings/go/net/mesh_rpc_typed.go` — docstring updates.

---

## Wave 5 — Tests + fixture

### O-T1 — Update fixture

**Design.**
- `tests/cross_lang_nrpc/golden_vectors_streaming.json::observer_invariants.firing_contract` — rewrite the per-binding entries:
  - `napi`: "Bounded-mpsc (1024 events) + dedicated worker task pumping to the TSFN. Drop counter increments on overflow; surfaced via `metricsSnapshot.observerDroppedTotal`."
  - `pyo3`: "Bounded-mpsc (1024 events) + dedicated worker task acquiring GIL once per drained event. Drop counter increments on overflow; surfaced via `metrics_snapshot.observer_dropped_total`."
  - `c_abi`: "Bounded-mpsc (1024 events) + Rust-side worker invoking the C function pointer. Drop counter increments on overflow; surfaced via the JSON snapshot's `observer_dropped_total` field and via `net_rpc_observer_dropped_total`."
  - Drop the `v1_scope` caveat that said "callbacks must be cheap" was the contract; the new contract is "callbacks should be cheap, the substrate is no longer on fire when they aren't."
- `metrics_snapshot_invariants.envelope` — add a sibling field documentation for the new top-level `observer_dropped_total: u64` field.
- Bump fixture `abi_version_expected` from `3 → 4` to match the rpc-ffi ABI bump (C-A3).

**Files touched.**
- `tests/cross_lang_nrpc/golden_vectors_streaming.json`.
- `tests/integration_nrpc_cross_lang_streaming.rs` — bump `ABI_VERSION_EXPECTED = 4` and extend the field-count assertion in `metrics_snapshot_invariants_fixture_is_well_formed` to account for the new envelope-level field.

### O-T2 — Drop counter under load

**Design.**
- New test in `bindings/node/src/mesh_rpc.rs::tests` (or as an integration test in the napi crate's test/ dir): construct a `NodeRpcObserver` with a TSFN whose synchronous queue-drain is instrumented to block; fire 2000 events; assert the drop counter increments to ≈ 2000 - 1024 = 976.
- Mirror in `bindings/python/src/mesh_rpc.rs::tests` and `bindings/go/rpc-ffi/src/lib.rs::tests`.

### C-T1 — Mid-stream cancel propagates to server-observed `Cancelled`

**Design.**
- Extend `tests/integration_nrpc_cross_lang_streaming.rs` with two new in-process round-trip tests using the same direct-fold-dispatch pattern as the existing `client_streaming_ok_cases_match_fixture` / `duplex_ok_cases_match_fixture`:
  - `client_stream_cancel_mid_send_observed_as_cancelled` — drive a 3-item send loop, cancel after the 2nd item, assert the server fold's emit closure observes the call's CANCEL frame (the existing `error_cases` fixture entry `client_stream_cancel_mid_send` documents this contract; this is the Rust-side reference assertion).
  - `duplex_cancel_from_caller_observed_as_cancelled` — similar shape for the duplex `error_cases` entry.

### C-T2 — Per-binding cancel stub tests

**Design.**
- Stub-level tests in each binding's test suite (Node `mesh_rpc.test.ts`, Python `tests/test_mesh_rpc.py`, Go `mesh_rpc_typed_test.go`) asserting that signal/Cancellable/ctx cancellation triggers `raw.close()` on the inner call handle.
- Each binding writes its own stub — they don't share test infrastructure, but they share the assertion shape (capture-the-close-call).

---

## Deferred follow-ups (post-v3)

Items deliberately deferred from v3; same convention as the v1 plan's deferred section.

1. **Per-service observer drops.** Right now the drop counter is per-mesh, not per-service. If operators need to know which service's observer is dropping (e.g. "events from `echo` are dropping; events from `lookup` are fine"), a per-service drop counter could fit into `ServiceMetrics`. Wait until production users surface the need.
2. **Server-side `direction=='inbound'` observer events.** Carried over from the v1 plan; same scope.
3. **Live multi-process cross-language harness.** Carried over from the v1 plan; same scope.
4. **`Range` iterator for Go streams.** Carried over from the v1 plan; gated on Go 1.23+ workspace bump.
5. **Cancellation propagation INTO the streaming send path.** v3 wires cancel into the call construction + the `close_notify`-driven inner close. A future refinement could ALSO interrupt a `send()` that's mid-flight on a credit await (this already works on the napi side via the existing `close_notify`; check pyo3 / Go parity).
6. **Coordinated mpsc bound across bindings.** Hard-coded `1024` per binding is fine for v3; a future tunable (via env var or per-mesh config) lets ops staff size the queue to their observer's actual cost. Wait until a user actually files a "1024 is too small" issue.
