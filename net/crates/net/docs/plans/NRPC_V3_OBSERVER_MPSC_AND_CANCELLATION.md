# nRPC v3: Bounded-mpsc Observer Dispatch + Unified Streaming Cancellation

Branch: `nrpc-v3-observer-and-cancel` (suggested).
Predecessor: [`NRPC_STREAMING_PARITY_AND_GO_BINDING.md`](./NRPC_STREAMING_PARITY_AND_GO_BINDING.md) — this plan revises that plan's locked decisions #1 and #2 from "documented-sync, ship later" to "ship in v3 across all three bindings."

Scope: close the two real DX gaps the v1 typed-nRPC surface left open before downstream consumers can file production-pinning issues against them. Both are small per-binding extensions ("~30 lines per binding" was the reviewer estimate that prompted this plan), but skipping them in v1 was load-bearing for shipping the larger plan on time. Doing them now — before the next downstream binding cuts — keeps the surface idiomatic on day one.

## Why now

1. **Observer mpsc + drop counter.** The v1 contract documents "callbacks must be cheap; the substrate dispatch thread blocks until the call returns." In a 3-month adoption cycle, that contract gets violated within the first week — a user wires a Prometheus exporter or a disk-flushing log sink into `setObserver`, the substrate dispatch thread pins, mesh-wide RPC latency spikes, and the issue lands as "the observer hook is broken." Retrofitting bounded-mpsc-with-drop-counter into an already-documented sync observer contract means either breaking the contract (callers who depended on synchronous fire ordering get confused) or layering a parallel `setObserverBuffered` path (twice the API surface for one shape). Flipping the default to mpsc-with-drop-counter for v3 keeps the surface single-shape and gives operators a free "observer drops since last snapshot" diagnostic.

2. **Unified streaming cancellation.** v1 shipped three different cancellation stories across the bindings — Node `close()`-only, pyo3 `close()` via the SDK's `close_notify`, Go ctx-honored-for-unary-but-not-for-streaming. The DX gap is real: every Node user discovers within an hour of writing a `callClientStream` loop that their `AbortSignal` is silently ignored. The fix is small — the napi `MeshRpc::call_client_stream` and `::call_duplex` need the same `cancel_token` shim the unary `call` already has (line range flagged at `bindings/node/src/mesh_rpc.rs:1572-1594`). Same for pyo3's `Cancellable` and Go's `ctx.Context`. Doing this now keeps `AbortSignal` and `context.Context` working from day one, which is what JS and Go users expect by reflex.

## Locked decisions for this plan

1. **mpsc bound = 1024 events per mesh.** Matches the existing `RpcResponseSink`'s pump-side mpsc bound (`mesh_rpc.rs:1326-1334`). Big enough that a momentarily-slow observer doesn't lose events under normal load; small enough that an actually-broken observer surfaces drops within seconds rather than minutes. Single shared queue per mesh (not per-binding-instance) so the drop counter is meaningful at the operator level.
2. **Drop counter is a single u64 on the snapshot, not per-service.** Observer dispatch is per-mesh, not per-service — bucketing the drop counter by service would require a second tier of mpsc queues with no diagnostic benefit. Add as `RpcMetricsSnapshot::observer_dropped_total`.
3. **Cancellation continues to thread through the existing `cancel_token` plumbing on the napi side.** No new substrate-level concept — the typed wrappers wire `AbortSignal` / `Cancellable` / `context.Context` into the binding-local cancel-token registry, then the binding's `call_client_stream` / `call_duplex` spawn the inner SDK call inside an abortable task (mirroring the unary path at `bindings/node/src/mesh_rpc.rs:1498-1506`).
4. **No SDK-level changes.** All three bindings handle cancellation at their own layer (the SDK's `ClientStreamCallRaw` / `DuplexCallRaw` already drop-emit CANCEL on Drop; the bindings' abortable-spawn wrapper drops the call when the cancel token fires). This keeps the v3 plan self-contained at the binding layer and avoids cascading substrate changes.

Tagged `[A | B | C | D | T]`:

- **A** — observer mpsc + drop-counter (napi binding + pyo3 binding + C ABI / Go FFI).
- **B** — Node TS typed wrapper changes (re-wire `AbortSignal` for streaming; drop `stripSignal` for streaming entries).
- **C** — Python typed wrapper changes (extend `Cancellable` to streaming).
- **D** — Go typed wrapper changes (wire `ctx.Context` to streaming).
- **T** — cross-binding tests (fixture extensions + per-binding cancel + drop-counter).

---

## Status

| ID    | Pri | Area                | Title                                                                                          |
|-------|-----|---------------------|------------------------------------------------------------------------------------------------|
| O-A1  | H   | napi binding        | Replace sync `NodeRpcObserver` with bounded-mpsc + drop counter; surface `observerDroppedTotal` in `metricsSnapshot` |
| O-A2  | H   | pyo3 binding        | Replace per-event `spawn_blocking` with bounded-mpsc + worker task + drop counter; surface in `metrics_snapshot` |
| O-A3  | H   | C ABI / Go FFI      | Replace direct dispatcher invocation with Rust-side bounded-mpsc + worker; surface drop counter in JSON of `net_rpc_metrics_snapshot` |
| C-A1  | H   | napi binding        | Raw `MeshRpc::call_client_stream` / `::call_duplex` honor `cancel_token` opt; abortable-spawn wrapper                              |
| C-A2  | H   | pyo3 binding        | Raw `MeshRpc::call_client_stream` / `::call_duplex` honor `Cancellable` opts; cancel via Notify                                   |
| C-A3  | H   | Go FFI              | Add `net_rpc_call_client_stream_cancellable` + `net_rpc_call_duplex_cancellable` FFI symbols mirroring the existing `net_rpc_call_streaming_cancellable` shape |
| C-B1  | M   | Node TS wrapper     | `TypedMeshRpc.callClientStream` / `callDuplex` wire `AbortSignal` end-to-end (drop `stripSignal` for streaming) |
| C-C1  | M   | Python wrapper      | `TypedMeshRpc.call_client_stream` / `call_duplex` extract `opts['cancel']` and propagate                       |
| C-D1  | M   | Go wrapper          | `TypedCallClientStream` / `TypedCallDuplex` propagate `ctx` through the cancellable FFI variant                |
| O-T1  | M   | fixture + tests     | Update `golden_vectors_streaming.json::observer_invariants.firing_contract` for mpsc shape; add `observerDroppedTotal` to `metrics_snapshot_invariants` |
| O-T2  | M   | tests               | Rust-side reference: drop counter increments under sustained load when the observer is intentionally slow |
| C-T1  | M   | tests               | Rust-side reference: cancel mid-stream observed by server as `RpcStatus::Cancelled` for client-stream + duplex |
| C-T2  | L   | per-binding tests   | Stub-level test in each binding: signal-aborted / cancellable-cancelled / ctx-cancelled streaming call propagates to `close()` on the inner call |

ABI version: this plan bumps `NET_RPC_ABI_VERSION` from `0x0003` → `0x0004` because of the new cancellable FFI symbols. Additive; 0x0003 consumers keep working.

---

## Phasing

**Recommended order: O-A then C-A then wrappers then tests.**

1. **Wave 1 — Observer mpsc dispatch (O-A1 / O-A2 / O-A3 in parallel).** Independent files; safe to land same PR cycle.
2. **Wave 2 — Cancellation plumbing (C-A1 / C-A2 / C-A3 in parallel).** Same independence story. C-A3 bumps the ABI version; downstream Go consumers update at the same cut.
3. **Wave 3 — Wrappers (C-B1, C-C1, C-D1 in parallel).** Thin pass-throughs once the raw layers honor cancel.
4. **Wave 4 — Fixture + tests (O-T1, O-T2, C-T1, C-T2).** Pin the new contract; per-binding tests land alongside their wrappers.

Wave 1 and Wave 2 can land same-PR-cycle as Wave 3 — the wrapper changes don't depend on the observer mpsc landing first, and the raw cancel surface depends only on its own binding's raw layer.

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

## Wave 2 — Streaming cancellation plumbing

### C-A1 — napi raw `call_client_stream` + `call_duplex` honor `cancel_token`

**Rationale.** The v1 typed wrapper's `stripSignal` helper drops `opts.signal` for streaming entries because the raw napi side doesn't honor it. The fix is to mirror the unary path's `run_cancellable_call` pattern at `bindings/node/src/mesh_rpc.rs:1498-1506`: when `opts.cancel_token` is non-zero, spawn the inner SDK call inside an `AbortHandle`-instrumented task, register the abort handle in the cancel registry, drop on cancel.

**Design.**
- Extend `call_client_stream` and `call_duplex` (napi) to extract `cancel_token` from `CallOptions` and wrap the inner SDK call construction in `run_cancellable_call` (the existing helper).
- The returned `ClientStreamCall` / `DuplexCall` napi class instances participate in the cancel registry: their `close()` method also cancels via the token if one was reserved. The user's typed wrapper drops `stripSignal` and instead uses `wireAbortSignal` for the streaming entries too — the existing helper at `mesh_rpc.ts:wireAbortSignal` already mints a token + registers a listener.
- One subtle: the unary path's cancel-aborts-the-spawn-task pattern works because the spawn task IS the call. For client-stream / duplex, the "call" is a long-lived handle; the spawn task only constructs it. So cancel needs to (a) abort the construction task if cancel fires pre-construction, AND (b) call `close()` on the returned handle if cancel fires post-construction. Resolution: the `cancel_registry` entry stores both an abort handle (for the construction task) AND a `Weak<Notify>` pointing at the handle's `close_notify` — cancel fires both. The `ClientStreamCall` / `DuplexCall` napi classes already have `close_notify: Arc<Notify>` (`mesh_rpc.rs:611`); reuse it.

**Files touched.**
- `bindings/node/src/mesh_rpc.rs:1572-1594` (`call_client_stream`) — wrap in `run_cancellable_call` + thread `close_notify` into the cancel registry.
- `bindings/node/src/mesh_rpc.rs:1598-1621` (`call_duplex`) — same pattern.
- `bindings/node/src/mesh_rpc.rs:reserve_cancel_token / cancel_call` — extend the registry to track per-token weak refs to `close_notify`.

**Test plan.**
- Stub-level: mock `MeshRpc` that captures the constructed `ClientStreamCall`; signal the abort token; assert the call's `close_notify` fires (and `Close` is observed).
- Integration: C-T1's mid-stream cancel test exercises the full path.

### C-A2 — pyo3 raw `call_client_stream` + `call_duplex` honor `Cancellable`

**Rationale.** Same as C-A1 for pyo3. The pyo3 side already has `Cancellable` (mirror of napi's cancel token) wired into unary calls — extend to streaming.

**Design.**
- Mirror the napi pattern: extract `opts['cancel']` from the optional dict, attach a watcher that fires `call.close()` via the SDK's `close_notify` on cancel. The existing `extract_cancellable` + `run_cancellable_call` helpers at `bindings/python/src/mesh_rpc.rs` accept the same pattern; extend their plumbing.
- The returned `PyClientStreamCall` / `PyDuplexCall` already have `close_notify: Arc<Notify>` — reuse.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — extend `call_client_stream` + `call_duplex` to accept `opts: Option<dict>` (matching the unary signature) and propagate cancel.
- `bindings/python/python/net/mesh_rpc.py` — drop the "cancel is not propagated to streaming layer" docstring caveat from `call_client_stream` / `call_duplex`.

### C-A3 — Go FFI: `net_rpc_call_client_stream_cancellable` + `net_rpc_call_duplex_cancellable`

**Rationale.** The Go FFI already has the `net_rpc_call_streaming_cancellable` precedent (line range flagged in `rpc-ffi/src/lib.rs:1163-1218`); ship the same shape for the client-stream and duplex entry points so Go's idiomatic `ctx.Context` cancellation works on those surfaces too. ABI version bumps to `0x0004`.

**Design.**
- Add two new FFI functions:
  - `net_rpc_call_client_stream_cancellable(handle, target, service, deadline_ms, request_window, cancel_token, out_call, out_err) -> c_int`
  - `net_rpc_call_duplex_cancellable(handle, target, service, deadline_ms, stream_window, request_window, cancel_token, out_call, out_err) -> c_int`
- Internally these mirror the cancellable unary pattern: spawn the construction onto the tokio runtime inside a `run_cancellable` block; the resulting `*ClientStreamCallHandleC` / `*DuplexCallHandleC` is stored with a back-reference to the cancel token so a subsequent `net_rpc_cancel_call(token)` triggers `Close` on the handle.
- The Go wrapper `MeshRpc.CallClientStream(ctx, ...)` and `MeshRpc.CallDuplex(ctx, ...)` install a cancel-watcher exactly like the unary path's `installCancelWatcher(ctx)` (`mesh_rpc.go:654`).
- ABI version bumps `0x0003 → 0x0004` because we're adding new exported symbols; existing 0x0003 symbols stay unchanged.

**Files touched.**
- `bindings/go/rpc-ffi/src/lib.rs` — two new exported functions + ABI version constant + doc-comment update.
- `bindings/go/net/mesh_rpc.go` — cgo `extern` declarations; extend `CallClientStream` / `CallDuplex` to call the cancellable variant and install the cancel watcher.
- `bindings/go/net/mesh_rpc_typed.go` — `TypedCallClientStream` / `TypedCallDuplex` pass `ctx` through unchanged (the raw layer now honors it).

**Risks.**
- ABI version cascade: the reference `ExpectedABIVersion` pin in `mesh_rpc.go:586-595` flips from `0x0003` → `0x0004`. Downstream Go binding consumers compiled against `0x0003` panic at process init (`mesh_rpc.go:618-625`) — same cascade as v1's `0x0001 → 0x0003` bump. Release notes for the next downstream Go binding cut MUST call out the override env-var `NET_RPC_SKIP_ABI_CHECK=1` for in-development consumers.

---

## Wave 3 — Typed wrapper pass-throughs

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

## Wave 4 — Tests + fixture

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
