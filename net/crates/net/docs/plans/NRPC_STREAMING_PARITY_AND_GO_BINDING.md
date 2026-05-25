# nRPC Streaming Parity (Node + Python) and Go Typed Binding Parity

Branch: `nrpc-streaming-parity` (suggested).
Predecessors: `docs/misc/NRPC_BINDINGS_PLAN.md` (typed wrapper contract), `docs/plans/NRPC_BIDI_STREAMING_PLAN.md` (ABI 0x0002 surface shipped substrate-side and at every raw binding layer).
Scope: close two cross-language gaps in the user-facing typed nRPC surface:

1. **Slice 2 — Streaming parity (Node + Python).** Both bindings ship unary + server-streaming typed RPC + full resilience (`RetryPolicy` / `HedgePolicy` / `CircuitBreaker`). What is missing is the typed wrapper layer for **client-streaming** + **duplex**, and the `set_rpc_observer` / `rpc_metrics_snapshot()` hooks. The raw napi / pyo3 classes for those streaming primitives already exist (`ClientStreamCall` / `DuplexCall` / `DuplexSink` / `DuplexStream` / `JsRequestStream` / `JsResponseSink` / `PyClientStreamCall` / `PyDuplexCall` / etc.), so this is pure JS / Python wrapper work plus a small substrate-binding addition to surface the observer / metrics getters.
2. **Slice 1 — Go typed binding.** Today `bindings/go/net/mesh_rpc.go` ships only the raw CGo surface (`*MeshRpc`, `*ClientStreamCall`, `*DuplexCall`, etc.) — no `TypedMeshRpc` analog exists. Design a Go-idiomatic typed surface mirroring the Node/Python shape from day one: unary `Call` / `Serve` / `CallService`, server-streaming `CallStreaming`, client-streaming `CallClientStream`, duplex `CallDuplex` / `Split`, observer + metrics. The raw FFI surface is already complete (verified at `bindings/go/net/mesh_rpc.go:169-313` — every duplex / client-stream / request-stream / response-sink C symbol is declared and Go-side wrappers exist), so Go is pure user-facing wrapper work plus one FFI addition for observer + metrics.

Explicitly **out of scope**: codec selection. JSON stays the only codec across all three bindings (no `Codec` enum extension, no protobuf bridge, no `CallOptionsTyped::codec` parity). The Rust SDK's `Codec::JsonPretty` is the only encoder option exposed at the SDK layer, and the bindings stay JSON-only per the existing contract pinned in `tests/cross_lang_nrpc/golden_vectors.json`.

## Locked decisions

Three risks were flagged during planning and are resolved before any slice merges. Each decision below is the contract every slice in this plan codes against:

1. **Observer firing-thread cost — synchronous + documented.** Each binding's observer trampoline fires the user callback synchronously on the substrate's dispatch path. The docstring on every binding's `setObserver` / `set_observer` / `SetObserver` method must spell out "callbacks must be cheap; pushing into a queue is the safe shape for slow consumers." No bounded-mpsc + drop-on-overflow layer ships in v1.
2. **Streaming cancellation — `close()`-only for v1.** `AbortSignal` (Node), cancel-token (pyo3), and `context.Context` (Go) are **not** wired into the typed `callClientStream` / `callDuplex` surfaces. Callers cancel a streaming RPC by invoking `.close()` on the typed handle. Per-language docstrings on the streaming entry points must call this out. Unifying the three raw-binding cancel stories so context/signal/token propagate uniformly is a deliberate post-v1 follow-up.
3. **Go generics — free-function shape, no method-based generics.** Every typed surface in the Go binding ships as a free function with type parameters (`TypedCall[Req, Resp](ctx, rpc, target, svc, req)`) rather than a method on `*TypedMeshRpc` (Go forbids type parameters on methods). Compile-time type safety wins over reflection-based ergonomics. Streams + calls remain type-parameterized structs (`*TypedDuplexCall[Req, Resp]`) so their methods can use the struct-level type params without violating the no-method-generics rule.

Tagged `[A | B | C | D]`:

- **A** — Rust binding-layer changes (napi `bindings/node/src/mesh_rpc.rs` + pyo3 `bindings/python/src/mesh_rpc.rs` + C ABI `bindings/go/rpc-ffi/src/lib.rs` / `include/net_rpc.h`). Only the substrate-binding additions for observer + metrics need this slice; the streaming primitives are already exposed.
- **B** — Node TypeScript typed wrapper (`bindings/node/mesh_rpc.ts`).
- **C** — Python typed wrapper (`bindings/python/python/net/mesh_rpc.py`).
- **D** — Go typed wrapper (new file: `bindings/go/net/mesh_rpc_typed.go`).

---

## Status

**Plan delivered (2026-05-24).** All 16 slices merged across 10 commits on `nrpc-sdks`. Per-slice landing commits documented in the `Done` column below; deferred follow-ups (live multi-binding harness, bounded-mpsc + drop-counter observer queue, unified streaming cancel propagation, server-side `direction=='inbound'` events) are pinned in the [Deferred follow-ups](#deferred-follow-ups-post-v1) section.

| ID    | Pri | Area                | Title                                                                                          | Done |
|-------|-----|---------------------|------------------------------------------------------------------------------------------------|------|
| S2-A1 | H   | napi binding        | Expose `MeshRpc.setObserver(handler)` + `MeshRpc.metricsSnapshot()` on raw napi class          | ✅ `98a2a25e` |
| S2-A2 | H   | pyo3 binding        | Expose `MeshRpc.set_observer(callable)` + `MeshRpc.metrics_snapshot()` on raw pyo3 class       | ✅ `98a2a25e` |
| S2-B1 | H   | Node TS wrapper     | `TypedMeshRpc.serveClientStream` + `callClientStream` + `TypedClientStreamCall`                | ✅ `2c5456ce` |
| S2-B2 | H   | Node TS wrapper     | `TypedMeshRpc.serveDuplex` + `callDuplex` + `TypedDuplexCall` / `TypedDuplexSink` / `TypedDuplexStream` | ✅ `e55bd791` |
| S2-B3 | M   | Node TS wrapper     | `TypedMeshRpc.setObserver` + `TypedMeshRpc.metricsSnapshot` + `RpcCallEvent` JS type           | ✅ `07b5fca6` |
| S2-C1 | H   | Python wrapper      | `TypedMeshRpc.serve_client_stream` + `call_client_stream` + `TypedClientStreamCall`            | ✅ `a85a2199` |
| S2-C2 | H   | Python wrapper      | `TypedMeshRpc.serve_duplex` + `call_duplex` + `TypedDuplexCall` / `TypedDuplexSink` / `TypedDuplexStream` | ✅ `2bcc7530` |
| S2-C3 | M   | Python wrapper      | `TypedMeshRpc.set_observer` + `TypedMeshRpc.metrics_snapshot` + `RpcCallEvent` dataclass       | ✅ `6cdb30db` |
| S2-X  | M   | cross-binding tests | Cross-language streaming round-trip test under `tests/cross_lang_nrpc/`                        | ✅ `d1edd3f0` |
| S1-A1 | M   | C ABI               | `net_rpc_set_observer` + `net_rpc_metrics_snapshot` FFI symbols in `rpc-ffi/src/lib.rs`        | ✅ `98a2a25e` |
| S1-D1 | H   | Go wrapper          | `TypedMeshRpc` Go struct + `Call` / `CallService` / `Serve` (unary)                            | ✅ `39b73f6b` |
| S1-D2 | H   | Go wrapper          | `TypedMeshRpc.CallStreaming` + `TypedRpcStream`                                                | ✅ `39b73f6b` |
| S1-D3 | H   | Go wrapper          | `TypedMeshRpc.ServeClientStream` + `CallClientStream` + `TypedClientStreamCall`                | ✅ `39b73f6b` |
| S1-D4 | H   | Go wrapper          | `TypedMeshRpc.ServeDuplex` + `CallDuplex` + `TypedDuplexCall` / `TypedDuplexSink` / `TypedDuplexStream` | ✅ `39b73f6b` |
| S1-D5 | M   | Go wrapper          | `TypedMeshRpc.SetObserver` + `MetricsSnapshot` + observer trampoline                           | ✅ `39b73f6b` |
| S1-D6 | M   | Go tests            | `bindings/go/net/mesh_rpc_typed_test.go` — JSON round-trip + streaming round-trip + observer fire | ✅ `39e99d15` |

---

## Phasing

**Recommended order: Slice 2 first, in two waves.**

1. **Wave 1 — Substrate exposure (S2-A1, S2-A2, S1-A1 in parallel).** Surface `set_observer` + `metrics_snapshot` on the raw napi / pyo3 / C ABI layer. These three slices are independent because they touch three different `mesh_rpc.rs` files (`bindings/{node,python,go-rpc-ffi}/src/`) and a header. Land before any wrapper work so the wrappers can target stable raw shapes.
2. **Wave 2 — Wrapper work.**
   - **Node wrappers (S2-B1 → S2-B2 → S2-B3) — strictly sequential.** B2 (duplex) uses the same `wireAbortSignal` + `appError` patterns B1 (client-stream) introduces; B3 (observer) needs the TSFN bridge B1/B2 standardize.
   - **Python wrappers (S2-C1 → S2-C2 → S2-C3) — strictly sequential, same reason.** Python and Node can run in parallel (separate files, separate maintainers); recommend pairing them so the contracts stay 1:1.
3. **Wave 3 — Cross-language test (S2-X).** Extend `tests/cross_lang_nrpc/` with golden-vector + round-trip coverage of the new streaming surfaces. Lands after Node + Python typed wrappers are in.
4. **Wave 4 — Go typed wrapper (S1-A1 + S1-D1..D6).** Sequential within itself; D1 introduces the JSON codec, error classification, and the `*TypedMeshRpc` shell every subsequent slice extends. D5 depends on S1-A1 (FFI observer hook). Goes LAST so the Go binding's typed surface lands with full client-stream + duplex + observer coverage from day one.

Wave 1 can land same-PR-cycle as Wave 2's first slice (B1 / C1) — the napi observer hook is small and additive, and B1's review doesn't depend on B3 being merged.

---

## Slice 2 — Streaming parity (Node + Python)

### S2-A1 — napi `MeshRpc.setObserver(handler)` + `metricsSnapshot()`

**Rationale.** The raw napi `MeshRpc` class at `bindings/node/src/mesh_rpc.rs:1395-1700` does not expose either substrate hook. The user-facing `TypedMeshRpc.setObserver` (S2-B3) needs a raw-binding seam.

**Design.**
- Add two `#[napi]` methods on `MeshRpc`:
  - `set_observer(handler: Option<Function<RpcCallEventJs, ()>>) -> Result<()>` — wires the JS callback through a TSFN to an `Arc<dyn RpcObserver>` on the substrate side. `None` clears the observer (substrate already supports `Option<RpcObserverHandle>` at `crates/net/src/adapter/net/mesh.rs:5258`).
  - `metrics_snapshot() -> RpcMetricsSnapshotJs` — calls `self.node.rpc_metrics_snapshot()` and serializes the `RpcMetricsSnapshot` into a napi POD (services + per-service counters + the existing latency histogram fields).
- New `#[napi(object)]` POD structs `RpcCallEventJs` (mirroring `RpcCallEvent` at `crates/net/src/adapter/net/cortex/rpc_observer.rs:59-90`: `caller`, `callee`, `method`, `latencyMs`, `status` as string-tagged union, `requestBytes`, `responseBytes`, `direction` as `"outbound" | "inbound"`, `tsUnixMs`) and `RpcMetricsSnapshotJs` (`services: ServiceMetricsJs[]` with the `crates/net/src/adapter/net/mesh_rpc_metrics.rs:316-330` fields flattened to numeric/array napi types).
- Observer trampoline: same pattern as `NodeRpcHandler` but called synchronously from the substrate's dispatch path (the observer trait is sync — see `rpc_observer.rs:97-101`). Because the substrate observer fires from a hot path, the TSFN call must be non-blocking; spawn-call-into-JS via `ThreadsafeFunction::call(..., NonBlocking)` and discard the JS return value.

**Files touched.**
- `bindings/node/src/mesh_rpc.rs` — two new `#[napi]` methods on `impl MeshRpc`, the POD structs, and a `NodeRpcObserver` `RpcObserver` impl.
- `bindings/node/src/lib.rs` — re-export the POD types if napi-rs's auto-generated `index.d.ts` doesn't pick them up.

**Test plan.**
- `bindings/node/test/mesh_rpc.test.ts` — stub-level test that constructs a `MeshRpc`-shaped object exposing `setObserver` / `metricsSnapshot` and asserts the typed wrapper forwards correctly. Live observer firing belongs in S2-X.

**Risks.**
- TSFN backpressure: the substrate fires observers synchronously, so a slow JS observer pins the dispatch thread. Per the locked decision (#1), v1 ships with documented-sync semantics: the docstring on `MeshRpc.setObserver` must spell out "callbacks must be cheap; push into a queue if the consumer is slow." The TSFN dispatch uses `ThreadsafeFunction::call(..., NonBlocking)` so a TSFN-queue overflow surfaces as a dropped event rather than blocking the substrate; the napi default queue size is fine for the documented contract. Bounded-mpsc + drop-counter is a deliberate post-v1 follow-up.

### S2-A2 — pyo3 `MeshRpc.set_observer(callable)` + `metrics_snapshot()`

**Rationale.** Same as S2-A1, for the pyo3 binding.

**Design.**
- Two new `#[pymethods]` on `PyMeshRpc` (declared at `bindings/python/src/mesh_rpc.rs`):
  - `set_observer(callable: Option<PyObject>) -> PyResult<()>` — takes a Python callable; clears with `None`. Wires via a stored `Py<PyAny>` inside a `RpcObserver` impl whose `on_call` re-acquires the GIL, constructs a Python dict for the event, and calls the user callable. GIL contention is acceptable here — the observer is a sync trait and the Python user already accepts sync semantics.
  - `metrics_snapshot() -> PyResult<PyObject>` — call `node.rpc_metrics_snapshot()`, build a Python dict (services list of dicts) and return.
- Python-side: surface as `_RawMeshRpc.set_observer` / `metrics_snapshot` via the existing import block in `python/net/mesh_rpc.py:42-56`.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — two new methods + `PyRpcObserver` struct.
- `bindings/python/python/net/_net.pyi` — type stubs.

**Test plan.**
- `bindings/python/tests/test_mesh_rpc.py` — stub-level forwarding test, mirroring the Node S2-A1 pattern.

**Risks.**
- GIL re-acquisition on every observer fire is non-trivial overhead. Per the locked decision (#1), v1 ships synchronously with the `set_observer` docstring spelling out "callbacks must be cheap; push into a `queue.Queue` for slow consumers." An async-batched variant is a deliberate post-v1 follow-up.

### S2-B1 — Node `TypedMeshRpc.serveClientStream` + `callClientStream` + `TypedClientStreamCall`

**Rationale.** Mirror the Rust SDK's `serve_rpc_client_stream_typed` + `call_client_stream_typed` (sdk/src/mesh_rpc.rs:553-597). The raw `ClientStreamCall` napi class at `bindings/node/src/mesh_rpc.rs:597-703` exposes `send` / `finish` / `callId` / `flowControlled` / `close`; we wrap it with JSON encode/decode on each side.

**Design.**
- New raw shape on `RawMeshRpc` interface (mesh_rpc.ts:124-148):
  ```typescript
  callClientStream(targetNodeId: bigint, service: string, opts?: CallOptions): Promise<RawClientStreamCall>
  serveClientStream(service: string, handler: (stream: RawRequestStream) => Promise<Buffer>): ServeHandle
  ```
  Plus `RawClientStreamCall { send(b: Buffer): Promise<void>; finish(): Promise<Buffer>; callId(): Promise<bigint>; flowControlled(): Promise<boolean>; close(): Promise<void> }` and `RawRequestStream { next(): Promise<Buffer | null>; readonly callerOrigin: bigint; readonly callId: bigint; readonly deadlineNs: bigint; readonly headers: [string, Buffer][] }` (matches the napi `JsRequestStream` shape at `bindings/node/src/mesh_rpc.rs:1017-1113`).
- New `TypedClientStreamCall<Req, Resp>` class:
  - `async send(value: Req): Promise<void>` — `jsonEncode(value)` then `raw.send(buf)`. Encode failure throws `nrpc:codec_encode:` (matches `classifyError` mapping to `RpcCodecError`).
  - `async finish(): Promise<Resp>` — `raw.finish()` returns the terminal Buffer; `jsonDecode(buf)` and return.
  - `callId() / flowControlled() / close()` — pass-through.
- New typed-handler shape on `TypedMeshRpc`:
  ```typescript
  serveClientStream<Req, Resp>(
    service: string,
    handler: (stream: TypedRequestStream<Req>) => Promise<Resp>,
  ): ServeHandle
  ```
  The handler shim decodes each chunk via `jsonDecode` (decode failure throws `appError(NRPC_TYPED_BAD_REQUEST, ...)` so the caller observes typed Application status — mirrors the unary `serve` shim at `mesh_rpc.ts:264-287`). Handler return value goes through `jsonEncode`.
- `TypedRequestStream<Req>` exposes `async next(): Promise<Req | null>` + `Symbol.asyncIterator` (mirror `TypedRpcStream` at `mesh_rpc.ts:413-485`).

**Error classification.** Existing `classifyError` (errors.ts) handles `nrpc:codec_encode` / `nrpc:codec_decode` / `nrpc:server_error: status=0x8000...` already; no errors.ts changes needed.

**Observer / metrics interaction.** Substrate fires `RpcCallEvent` on the dispatch path; the typed wrapper layer adds no firing of its own. The observer set via S2-B3 sees client-stream + duplex events automatically.

**Files touched.**
- `bindings/node/mesh_rpc.ts` — new `TypedClientStreamCall`, `TypedRequestStream`, two new `TypedMeshRpc` methods, `RawClientStreamCall` + `RawRequestStream` interfaces.

**Test plan.**
- `bindings/node/test/mesh_rpc.test.ts` — stub-level: construct a `StubRawMeshRpc` that returns a stub `ClientStreamCall`, exercise:
  - Round-trip — three `send({n: 1})` calls accumulate JSON-encoded Buffers; `finish()` returns a JSON-decoded reply.
  - Encode failure on `send(BigInt)` throws `nrpc:codec_encode`.
  - Decode failure on malformed reply throws `nrpc:codec_decode`.
  - Server-side: stub `RawRequestStream` yielding `Buffer.from('{"n":1}')` then `null`; the wrapped handler observes `{n: 1}` then `null` (EOF).
- `bindings/node/test/integration_*.test.ts` (alongside the existing integration tests) — live test against a real `MeshRpc`, deferred to S2-X.

**Risks.**
- AbortSignal integration: client-stream calls are long-lived; user wants `signal.aborted → close()` semantics. The raw `ClientStreamCall` has a `close()` method but the napi binding doesn't route a `cancelToken` through `callClientStream` (verify at `bindings/node/src/mesh_rpc.rs:1572-1594` — `CallOptions` carries `cancel_token` but `call_client_stream` doesn't honor it). Per the locked decision (#2), v1 documents `close()`-only cancellation: `callClientStream`'s `CallOptions` parameter still accepts `signal` for surface parity, but the wrapper ignores it for streaming calls (with a one-line docstring callout) and the user invokes `typedCall.close()` directly when aborting. Unifying signal/token/context across the raw bindings is a deliberate post-v1 follow-up.

### S2-B2 — Node `TypedMeshRpc.serveDuplex` + `callDuplex` + duplex typed wrappers

**Rationale.** Mirror the Rust SDK's `serve_rpc_duplex_typed` + `call_duplex_typed` (sdk/src/mesh_rpc.rs:631-673). The raw `DuplexCall` / `DuplexSink` / `DuplexStream` napi classes at `bindings/node/src/mesh_rpc.rs:722-979` are already exposed.

**Design.**
- New raw interface extensions on `RawMeshRpc` for `callDuplex(...) → RawDuplexCall` and `serveDuplex(svc, handler) → ServeHandle`. `RawDuplexCall` adds `send`/`finishSending`/`next`/`intoSplit`/`callId`/`flowControlled`/`close`; `RawDuplexSink` and `RawDuplexStream` mirror their napi shapes.
- `TypedDuplexCall<Req, Resp>` class:
  - `async send(value: Req): Promise<void>` — encode then `raw.send`.
  - `async finishSending(): Promise<void>` — pass-through.
  - `async next(): Promise<Resp | null>` — `raw.next()` + decode; decode failure terminates the call (close the underlying duplex call) and rethrows `nrpc:codec_decode`.
  - `Symbol.asyncIterator` over `next`.
  - `async intoSplit(): Promise<[TypedDuplexSink<Req>, TypedDuplexStream<Resp>]>` — peels off the underlying split and wraps each half.
- `TypedDuplexSink<Req>` and `TypedDuplexStream<Resp>` are the obvious split halves.
- Server-side: `serveDuplex<Req, Resp>(svc, handler)` where handler signature is `(stream: TypedRequestStream<Req>, sink: TypedResponseSink<Resp>) => Promise<void>`. `TypedResponseSink<Resp>.send(value)` JSON-encodes then `raw.send(buf)` (non-async, matches the napi `JsResponseSink.send` at `bindings/node/src/mesh_rpc.rs:1142-1151`).

**Files touched.**
- `bindings/node/mesh_rpc.ts` — five new classes (`TypedDuplexCall`, `TypedDuplexSink`, `TypedDuplexStream`, `TypedResponseSink`), three new `TypedMeshRpc` methods.

**Test plan.**
- `bindings/node/test/mesh_rpc.test.ts` — stub-level: duplex round-trip (send 3 reqs, recv 3 resps), `intoSplit` produces working halves, decode failure on response chunk terminates the stream.

**Risks.**
- Handler signature shape. The napi binding handles destructure as `(args: [stream, sink]) => ...` (a tuple param — see `mesh_rpc.rs:1666-1684`). The typed wrapper should NOT propagate that shape; expose the JS-idiomatic `(stream, sink) =>` form and have the typed wrapper destructure the tuple before invoking the user handler. Pin in the wrapper's docstring + a test that the user signature is `(stream, sink)` not `([stream, sink])`.

### S2-B3 — Node `TypedMeshRpc.setObserver` + `metricsSnapshot` + observer type

**Rationale.** The observer + metrics hooks are the missing diagnostic seam in the Node typed wrapper. Once S2-A1 ships them on the raw napi class, the typed wrapper is a thin shim.

**Design.**
- `interface RpcCallEvent { caller: bigint; callee: bigint; method: string; latencyMs: number; status: { kind: 'ok' } | { kind: 'error', message: string } | { kind: 'timeout' } | { kind: 'canceled' }; requestBytes: number; responseBytes: number; direction: 'outbound' | 'inbound'; tsUnixMs: number }` — mirror of `RpcCallEventJs` from S2-A1.
- `TypedMeshRpc.setObserver(handler: ((evt: RpcCallEvent) => void) | null): void` — forwards to `this._raw.setObserver(handler)`. The handler receives the event already-decoded by the napi POD-to-JS conversion.
- `TypedMeshRpc.metricsSnapshot(): RpcMetricsSnapshot` — forwards. The returned shape is a plain object literal (services as `ServiceMetrics[]`), no class wrapping.
- Question to weigh: should `setObserver` / `metricsSnapshot` live on `TypedMeshRpc` (encoding-aware) or on the raw `MeshRpc`? **Recommend `TypedMeshRpc` for parity with how the Rust SDK exposes them on `Mesh` (sdk/src/mesh.rs:377 — `set_rpc_observer`).** The wrapper does no codec work for these methods; they're just lifecycle / introspection.

**Files touched.**
- `bindings/node/mesh_rpc.ts` — two new `TypedMeshRpc` methods, two new interfaces.

**Test plan.**
- Stub-level: install an observer, push synthetic events into the raw stub, assert the JS callback fires with the decoded event shape. Live observer test belongs in S2-X.

**Risks.**
- Mid-call swap semantics. The substrate uses `ArcSwapOption` so observer swaps are atomic, but a swap mid-call means some events fire against the old observer, some against the new. Document; pin a test that asserts `setObserver(null)` followed by a no-op call doesn't fire any event.

### S2-C1 — Python `TypedMeshRpc.serve_client_stream` + `call_client_stream` + `TypedClientStreamCall`

**Rationale.** Mirror of S2-B1 for Python. The raw `PyClientStreamCall` at `bindings/python/src/mesh_rpc.rs:651-759` exposes `send`/`finish`/`call_id`/`flow_controlled`/`close` + `__enter__` / `__exit__`. The server-side `PyRequestStream` (or equivalent) is exposed at the pyo3 layer for the handler's request stream.

**Design.**
- `TypedClientStreamCall` class in `python/net/mesh_rpc.py`:
  - `def send(self, value)` — `_json_encode(value)` then `self._raw.send(buf)`.
  - `def finish(self)` — `self._raw.finish()` returns terminal bytes; `_json_decode`.
  - `call_id()` / `flow_controlled()` / `close()` — pass-through.
  - `__enter__` / `__exit__` for context-manager support (mirror raw class).
- `TypedMeshRpc.call_client_stream(target_node_id, service, opts=None) -> TypedClientStreamCall` — straight wrap.
- `TypedMeshRpc.serve_client_stream(service, handler)` — handler signature is `(stream: TypedRequestStream) -> Resp`. Decode failure on the FIRST request chunk surfaces as `RpcAppError(NRPC_TYPED_BAD_REQUEST, ...)` (matches the unary `serve` shim at `python/net/mesh_rpc.py:266-278`). Handler return value goes through `_json_encode`.
- `TypedRequestStream` mirrors `TypedRpcStream` (line 153) — implements `__iter__` / `__next__`, GIL-friendly sync semantics. Decode failure on a chunk raises `RpcCodecError` and closes the underlying stream.

**Error classification.** Existing `classify_error` (line 415) handles all the relevant kinds; no changes.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — two new classes (`TypedClientStreamCall`, `TypedRequestStream`), two new `TypedMeshRpc` methods.

**Test plan.**
- `bindings/python/tests/test_mesh_rpc.py` — stub-level: define a stub raw class with `send` / `finish`; round-trip + encode failure + decode failure. Mirrors the existing JSON codec test (around line 200-260 of the test file).

**Risks.**
- Sync iterator semantics under client cancellation. The pyo3 raw class uses `tokio::select!` on `close_notify` so a `close()` during `send` unblocks (verified at `bindings/python/src/mesh_rpc.rs:685-702`). The typed wrapper inherits this for free.

### S2-C2 — Python `TypedMeshRpc.serve_duplex` + `call_duplex` + duplex typed wrappers

**Rationale.** Mirror of S2-B2 for Python. Raw `PyDuplexCall` / `PyDuplexSink` / `PyDuplexStream` are at `bindings/python/src/mesh_rpc.rs:769-1090`.

**Design.**
- `TypedDuplexCall` class — `send` / `finish_sending` / `__next__` / `__iter__` / `into_split` / context-manager support. `__next__` raises `StopIteration` on clean EOF (mirrors `PyDuplexCall.__next__` at `bindings/python/src/mesh_rpc.rs:843-876`); decode failure raises `RpcCodecError` after closing the call.
- `TypedDuplexSink<Req>` / `TypedDuplexStream<Resp>` — split halves.
- `TypedResponseSink` — server-side outbound. `def send(self, value)`: `_json_encode` + `self._raw.send(buf)`. Non-async (matches PyResponseSink raw shape).
- `TypedMeshRpc.serve_duplex(service, handler)` — handler signature is `(stream: TypedRequestStream, sink: TypedResponseSink) -> None`.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — five new classes, three new `TypedMeshRpc` methods.

**Test plan.**
- `bindings/python/tests/test_mesh_rpc.py` — duplex round-trip + split-halves test, mirroring the stub style.

**Risks.**
- Threading: hedging tests use `threading.Thread` (see test file lines around 569-625); the typed duplex wrapper should be thread-safe in the same shape (a single owner per `TypedDuplexCall`).

### S2-C3 — Python `TypedMeshRpc.set_observer` + `metrics_snapshot` + dataclass

**Rationale.** Mirror of S2-B3 for Python.

**Design.**
- `@dataclass class RpcCallEvent: caller: int; callee: int; method: str; latency_ms: int; status: RpcCallStatus; request_bytes: int; response_bytes: int; direction: str; ts_unix_ms: int` — Python dataclass with the same fields as the JS interface.
- `class RpcCallStatus`: tagged-union via a discriminated dataclass (`Ok` / `Error(message: str)` / `Timeout` / `Canceled`).
- `TypedMeshRpc.set_observer(callable: Optional[Callable[[RpcCallEvent], None]]) -> None` — forwards to `self._raw.set_observer(...)` after wrapping in a shim that constructs the dataclass from the dict the raw side passes (or pyo3 builds the dataclass directly via `Py<RpcCallEvent>` — pin at S2-A2 review time).
- `TypedMeshRpc.metrics_snapshot() -> RpcMetricsSnapshot` — pass-through; return a dataclass.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — two new dataclasses, two new methods.

**Test plan.**
- Stub-level forwarding test in `tests/test_mesh_rpc.py`.

**Risks.**
- Pyo3 GIL acquisition overhead per observer fire is non-trivial. Document. Acceptable for the diagnostic / development use case the observer targets; production high-throughput observers should keep callbacks trivial (e.g. push into a queue).

### S2-X — Cross-language streaming round-trip test

**Rationale.** The existing `tests/cross_lang_nrpc/` harness pins golden vectors for unary + server-streaming. Add coverage for client-stream + duplex + observer hooks.

**Design.**
- Extend `tests/cross_lang_nrpc/golden_vectors.json` with vectors for client-stream + duplex Application-status round-trips (e.g. typed handler raising `RpcAppError(NRPC_TYPED_HANDLER_ERROR, ...)` mid-stream).
- New cross-language tests:
  - Node calls Python's `serve_client_stream` handler — Python decodes 3 typed Requests, returns a Response sum.
  - Python calls Node's `serveDuplex` handler — Node echoes 5 chunks back.
  - Rust caller against Node observer — assert the observer fires once per request with the expected `RpcCallEvent` fields.
- Reuse the existing test bootstrap (`tests/integration_nrpc_cross_lang.rs` + `bindings/python/tests/test_cross_lang_compat.py`).

**Files touched.**
- `tests/cross_lang_nrpc/golden_vectors.json` — append new vectors.
- New `tests/integration_nrpc_streaming_cross_lang.rs` (or extend the existing cross-lang test file).
- `bindings/node/test/integration_streaming.test.ts` — new file or extend existing.
- `bindings/python/tests/test_cross_lang_streaming.py` — new file or extend existing.

**Risks.**
- Streaming tests are flakier than unary (timing-dependent). Mirror the existing pattern of bounded `await` + explicit `close()` — no `setTimeout`-based delays. Reuse `tokio::time::timeout` in the Rust runner.

---

## Slice 1 — Go typed binding

Lands AFTER Slice 2 so the Go binding can mirror Node + Python's typed shape exactly. Goes in seven sequential slices.

### S1-A1 — FFI surface for observer + metrics

**Rationale.** Today `bindings/go/rpc-ffi/src/lib.rs` exposes only call / serve / streaming primitives. Go's typed wrapper needs `net_rpc_set_observer` + `net_rpc_metrics_snapshot` symbols.

**Design.**
- Two new exported C functions in `rpc-ffi/src/lib.rs`:
  - `pub extern "C" fn net_rpc_set_observer(handle: *mut MeshRpcHandle, dispatcher: Option<RpcObserverFn>, observer_id: u64) -> c_int` — installs an `Arc<dyn RpcObserver>` that fires `dispatcher(observer_id, &RpcCallEventC)` synchronously on each event. Passing `None` clears the observer. The trampoline pattern mirrors the existing `net_rpc_set_handler_dispatcher` (rpc-ffi/src/lib.rs).
  - `pub extern "C" fn net_rpc_metrics_snapshot(handle: *mut MeshRpcHandle, out_json_ptr: *mut *mut c_char) -> c_int` — serializes `RpcMetricsSnapshot` to JSON, returns via the `out_json_ptr` (freed by `net_rpc_free_cstring`). JSON-over-FFI mirrors the aggregator FFI pattern at `go/aggregator.go:43-46`.
- New C header type `RpcCallEventC` — POD struct mirroring `RpcCallEvent` field-by-field with `*const c_char` for `method` + a `status_kind: u32` discriminant + `status_message: *const c_char`. Tear-down: Rust owns the strings; the C caller MUST NOT free them (the trampoline is sync, strings only live for the duration of the call).
- ABI version bumps from `0x0001` to `0x0002` (existing convention — see `bindings/go/net/mesh_rpc.go:590`).

**Files touched.**
- `bindings/go/rpc-ffi/src/lib.rs` — two new exported fns, the trampoline + observer struct.
- `bindings/go/net/include/net_rpc.h` (or wherever the C header lives — confirm at implementation time; the inline cgo prelude in `mesh_rpc.go:52-314` may be the only declaration site) — add the two extern symbols.
- ABI version constant bump.

**Test plan.**
- `bindings/go/rpc-ffi/tests/observer.rs` (new) — instantiate a Rust mesh + RPC, set a Rust-side mock observer through the FFI, call a unary, assert the observer fired with expected fields.

**Risks.**
- ABI bump cascades: `ExpectedABIVersion` in `mesh_rpc.go:590` flips from `0x0001` to `0x0002`; any downstream Go consumer compiled against the old version panics at process init (`mesh_rpc.go:618-625`). Document in the release notes; the `NET_RPC_SKIP_ABI_CHECK` env-var override is the escape hatch for in-development consumers.

### S1-D1 — Go `TypedMeshRpc` shell + unary `Call` / `CallService` / `Serve`

**Rationale.** Mirror Node's `TypedMeshRpc.fromMesh(mesh)` (mesh_rpc.ts:236) — a thin envelope around the raw `*MeshRpc` (already defined at `mesh_rpc.go:499-572`) that adds JSON encode/decode + typed handler shims.

**Design.**
- New file: `bindings/go/net/mesh_rpc_typed.go`. (Keep raw + typed in separate files for review clarity, mirroring how the Python wrapper splits across `src/mesh_rpc.rs` + `python/net/mesh_rpc.py`.)
- Top-level:
  ```go
  type TypedMeshRpc struct {
      raw *MeshRpc
  }
  
  func NewTypedMeshRpc(raw *MeshRpc) *TypedMeshRpc {
      return &TypedMeshRpc{raw: raw}
  }
  
  // Raw exposes the underlying *MeshRpc for users who need the
  // []byte-level surface (cross-codec interop, raw streams).
  func (t *TypedMeshRpc) Raw() *MeshRpc { return t.raw }
  ```
- `TypedCall[Req, Resp any]` — per the locked decision (#3), typed surfaces are free functions, not methods. Go forbids type parameters on methods, so this is the only shape that gives compile-time type safety:
  ```go
  func TypedCall[Req, Resp any](
      ctx context.Context,
      t *TypedMeshRpc,
      targetNodeID uint64,
      service string,
      req Req,
  ) (Resp, error)
  ```
  Reads cleanly at the call site: `result, err := TypedCall[EchoReq, EchoResp](ctx, t, target, "echo", req)`. Mirrors the Node `rpc.call<Req, Resp>(...)` ergonomics with one extra positional argument (the `*TypedMeshRpc` itself).
- `TypedCallService[Req, Resp]` — same shape, calls through to `t.raw.CallService`.
- `TypedServe[Req, Resp](rpc *TypedMeshRpc, service string, handler func(Req) (Resp, error)) (*ServeHandle, error)` — handler shim:
  ```go
  rawHandler := func(reqBytes []byte) ([]byte, error) {
      var req Req
      if err := json.Unmarshal(reqBytes, &req); err != nil {
          return nil, NewRpcAppError(NrpcTypedBadRequest,
              fmt.Sprintf(`{"error":"invalid_request","detail":%q}`, err.Error()))
      }
      resp, err := handler(req)
      if err != nil {
          // Surface as Application(NRPC_TYPED_HANDLER_ERROR), mirroring
          // the Node binding's appError shape.
          return nil, NewRpcAppError(NrpcTypedHandlerError, err.Error())
      }
      return json.Marshal(resp)
  }
  return rpc.raw.Serve(service, rawHandler)
  ```
- `RpcAppError` Go type — minted similarly to the JS `appError(...)` helper. The error message uses the canonical `nrpc:app_error:0x<code>:<body>` shape; the Rust binding's `parse_js_app_error` (mesh_rpc.rs around line 1274) reuses the same parser for Go.
- Constants: `const NrpcTypedBadRequest = 0x8000; const NrpcTypedHandlerError = 0x8001` — mirrors the Node + Python exports.

**Error classification.** The existing `*RpcError` Go type at `mesh_rpc.go:354-381` already classifies by kind (mesh_rpc.go:368). The typed wrapper layer can `errors.As(err, &rpcErr)` to inspect; the JSON encode/decode failures map to `RpcKindCodecEncode` / `RpcKindCodecDecode`. No `RpcError` changes needed.

**Files touched.**
- New `bindings/go/net/mesh_rpc_typed.go`.
- `bindings/go/net/mesh_rpc.go` — small addition: a `RpcAppError` helper (or move to a new `bindings/go/net/typed_errors.go`).

**Test plan.**
- New file `bindings/go/net/mesh_rpc_typed_test.go` (analog of the Python `test_mesh_rpc.py` stub-level coverage).
- Round-trip: `TypedCall[EchoReq, EchoResp]` against a stub raw mesh + handler. Use a Go interface for stub-injectability (extract a `rawMeshRpc` interface from the `*MeshRpc` concrete type if not already done).
- Encode failure (channel value in `Req`) → `RpcKindCodecEncode`.
- Decode failure (malformed response Buffer) → `RpcKindCodecDecode`.
- `RpcAppError(0x8000, "...")` → server returns; caller observes `*RpcError{Kind: RpcKindServerError, Message: "status=0x8000 message=..."}`.

**Risks.**
- Stub-injectability. The current `*MeshRpc` exposes its methods directly; tests can't swap a fake CGo handle in. Either (a) extract an interface `rawMeshRpc` and have `TypedCall` take the interface (with `*MeshRpc` satisfying it), or (b) wrap the cgo-bound concrete type behind a function-pointer registry like the Node `RawMeshRpc` interface. **Recommend (a)** — cleaner for tests, minimal cost.

### S1-D2 — Go `CallStreaming` + `TypedRpcStream`

**Rationale.** Mirror Node's `TypedMeshRpc.callStreaming` + `TypedRpcStream` (mesh_rpc.ts:347-485).

**Design.**
- `TypedCallStreaming[Req, Resp]` returns a `*TypedRpcStream[Resp]`.
- `*TypedRpcStream[Resp]`:
  - `Recv() (Resp, error)` — `r.raw.Recv()` → bytes → `json.Unmarshal` → typed value. On `ErrStreamDone` return zero + the sentinel; on decode failure return zero + `*RpcError{Kind: RpcKindCodecDecode}` and close the underlying stream.
  - `Grant(n uint32)` — pass-through to `raw.Grant`.
  - `Close()` — pass-through.
  - Optional: `Range(yield func(Resp) bool)` — Go 1.23+ range-over-func style (mirror `Symbol.asyncIterator`). Document as a follow-up if the workspace doesn't target Go 1.23 yet.

**Files touched.**
- `bindings/go/net/mesh_rpc_typed.go` — extend.

**Test plan.**
- `mesh_rpc_typed_test.go::stream_round_trip` — stub raw stream yielding 3 JSON-encoded buffers + EOF; `TypedRpcStream.Recv` returns typed values then `ErrStreamDone`.
- Decode failure mid-stream → next `Recv` returns `RpcKindCodecDecode` and subsequent returns `ErrStreamDone`.

**Risks.**
- Per the locked decision (#3), the typed stream is generic over `Resp` at the struct level: `*TypedRpcStream[Resp]`. `(*TypedRpcStream[Resp]).Recv() (Resp, error)` is a non-generic method on a generic struct — legal Go, reads cleanly, and avoids the `any`-shape footgun.

### S1-D3 — Go client-streaming typed wrapper

**Rationale.** Mirror Node's `TypedClientStreamCall` (S2-B1).

**Design.**
- `TypedCallClientStream[Req, Resp]` — wraps `r.raw.CallClientStream`.
- `*TypedClientStreamCall[Req, Resp]`:
  - `Send(value Req) error` — `json.Marshal(value)` then `c.raw.Send(buf)`.
  - `Finish() (Resp, error)` — `c.raw.Finish()` → bytes → `json.Unmarshal`.
  - `CallID() uint64` / `Close()` — pass-through.
- `TypedServeClientStream[Req, Resp](rpc *TypedMeshRpc, service string, handler func(stream *TypedRequestStream[Req]) (Resp, error)) (*ServeHandle, error)`:
  ```go
  rawHandler := func(stream *RequestStreamRecv) ([]byte, error) {
      typedStream := &TypedRequestStream[Req]{raw: stream}
      resp, err := handler(typedStream)
      if err != nil { return nil, NewRpcAppError(NrpcTypedHandlerError, err.Error()) }
      return json.Marshal(resp)
  }
  return rpc.raw.ServeClientStream(service, rawHandler)
  ```
- `*TypedRequestStream[Req]`:
  - `Recv() (Req, error)` — `raw.Recv()` → bytes → `json.Unmarshal`. Returns `ErrStreamDone` on EOF.
  - First-chunk decode failure surfaces a `RpcAppError(NRPC_TYPED_BAD_REQUEST, ...)` (parallel to the unary `TypedServe` shim).

**Files touched.**
- `bindings/go/net/mesh_rpc_typed.go` — extend.

**Test plan.**
- `mesh_rpc_typed_test.go::client_stream_round_trip` — stub raw client-stream call accepting 3 sends + returning a Buffer on finish. Round-trip + encode failure + decode failure.

**Risks.**
- Same generic-method footgun as S1-D2; the stream itself carries the type param, but the `Send` method on `*TypedClientStreamCall[Req, Resp]` works fine because `Req` is a struct-level type param, not a method-level one.

### S1-D4 — Go duplex typed wrapper

**Rationale.** Mirror Node's `TypedDuplexCall` / `TypedDuplexSink` / `TypedDuplexStream` (S2-B2).

**Design.**
- `TypedCallDuplex[Req, Resp]` returns `*TypedDuplexCall[Req, Resp]`.
- `*TypedDuplexCall[Req, Resp]` — `Send(Req)`, `FinishSending()`, `Recv() (Resp, error)`, `Split() (*TypedDuplexSink[Req], *TypedDuplexStream[Resp], error)`, `CallID()`, `Close()`.
- `*TypedDuplexSink[Req]` and `*TypedDuplexStream[Resp]` — split halves.
- `TypedServeDuplex[Req, Resp](rpc *TypedMeshRpc, service string, handler func(stream *TypedRequestStream[Req], sink *TypedResponseSink[Resp]) error) (*ServeHandle, error)`.
- `*TypedResponseSink[Resp]`:
  - `Send(value Resp) error` — `json.Marshal` then `s.raw.Send(buf)`. Non-blocking (matches the raw `ResponseSinkSend.Send` at `mesh_rpc.go:1885-1902`).

**Files touched.**
- `bindings/go/net/mesh_rpc_typed.go` — extend.

**Test plan.**
- `mesh_rpc_typed_test.go::duplex_round_trip` — full round-trip with split halves. Concurrent goroutines exercising send + recv.

**Risks.**
- Concurrent goroutine safety. The raw `*DuplexCall` is single-threaded for `send` (the C side serializes through the mutex); the typed wrapper inherits this. Document explicitly in the type's docstring.

### S1-D5 — Go observer + metrics

**Rationale.** Mirror Node's `TypedMeshRpc.setObserver` + `metricsSnapshot` (S2-B3).

**Design.**
- Build on S1-A1's FFI hooks.
- `type RpcCallEvent struct { ... }` — Go struct with the same fields as the Node interface / Python dataclass.
- `type RpcCallStatus interface { rpcCallStatus() }` + four concrete types `StatusOk`, `StatusError`, `StatusTimeout`, `StatusCanceled` — Go's idiomatic discriminated union shape.
- `(*TypedMeshRpc).SetObserver(handler func(RpcCallEvent)) error` — wraps the C-ABI hook from S1-A1. Registers the Go callback in a sync.Map keyed by observer ID; the trampoline (similar to the existing `go_net_rpc_handler_trampoline` at `mesh_rpc.go:412-460`) looks up the callback by ID, builds a Go `RpcCallEvent` from the `RpcCallEventC` POD, invokes the user callback.
- `(*TypedMeshRpc).MetricsSnapshot() (RpcMetricsSnapshot, error)` — calls the FFI symbol, JSON-decodes the response into a `RpcMetricsSnapshot` struct (mirrored from the Rust SDK type).

**Files touched.**
- `bindings/go/net/mesh_rpc_typed.go` — extend.
- `bindings/go/net/mesh_rpc.go` — add the observer trampoline `//export go_net_rpc_observer_trampoline` next to the existing handler trampoline.

**Test plan.**
- `mesh_rpc_typed_test.go::observer_fires_on_call` — install observer, issue a unary call against a stub raw mesh (or a real loopback mesh if test fixtures permit), assert the callback fired with expected fields.

**Risks.**
- Same firing-thread-sync-cost issue as Node + Python. Per the locked decision (#1), v1 ships synchronously with the `SetObserver` docstring spelling out "callbacks must be cheap; push into a buffered channel for slow consumers (the cgo trampoline thread is the substrate's dispatch path)."

### S1-D6 — Go tests + integration coverage

**Rationale.** A single test file at `bindings/go/net/mesh_rpc_typed_test.go` mirroring the structure of `test_mesh_rpc.py` + `mesh_rpc.test.ts`.

**Test shape.**
- Stub-level: JSON round-trip, encode/decode failures, RpcAppError shape, observer fires.
- Live: extend `bindings/go/net/netdb_test.go` pattern (it already drives a real mesh) — boot a NetMesh, register a typed serve, call from a peer mesh, assert the typed wrapper decodes correctly.

**Files touched.**
- New `bindings/go/net/mesh_rpc_typed_test.go`.
- Possibly extend `bindings/go/net/netdb_test.go` if mesh-bootstrap fixtures are reusable.

**Risks.**
- Go test fixtures for live mesh tests already exist (see `netdb_test.go`); the typed RPC test should reuse the same `NewTestMesh()` (or equivalent) helper. Confirm at implementation time.

---

## Deferred follow-ups (post-v1)

Follow-up items deliberately deferred from v1; cross-referenced by the locked decisions at the top of the doc.

1. **Bounded-mpsc observer dispatch.** Each binding currently fires the user observer synchronously on the substrate dispatch path; the v1 contract documents "callbacks must be cheap." A follow-up adds a bounded-mpsc + drop-on-overflow trampoline per binding, with the drop count surfaced through `metricsSnapshot` so operators can see when their observer is too slow. Lands when a production user surfaces an observer that has to do real work (logging to disk, exporting to Prometheus, etc.).
2. **Unified streaming cancellation.** v1 ships `close()`-only cancellation across all three bindings. A follow-up extends the raw bindings so `AbortSignal` (Node), cancel-token (pyo3), and `context.Context` (Go) propagate uniformly through `callClientStream` / `callDuplex` to a single substrate-level cancel primitive. Likely involves a small change at `bindings/node/src/mesh_rpc.rs:1572-1621` (thread `cancel_token` through `call_client_stream` + `call_duplex`) and equivalent extensions on the pyo3 / Go raw layers.
3. **`Range` iterator for Go streams.** Once the Go workspace targets Go 1.23+, add range-over-func support on `*TypedRpcStream[Resp]` and `*TypedDuplexStream[Resp]` so `for resp := range stream.Range()` works idiomatically. Until then, the explicit `Recv()` loop is the only shape.
4. **Server-side `direction=='inbound'` observer events.** v1 emits only outbound events (caller-side completion). The substrate's `RpcDirection::Inbound` variant is declared but no firing site exists in `mesh_rpc.rs` — the dispatch path's mpsc-driven handler invocation needs additional plumbing to record the dispatch-to-respond span cleanly. Fixture's `observer_invariants.direction_discriminator` already documents `emitted_in_v1: false` for inbound; the follow-up flips that flag once the substrate fires it.
5. **Live multi-process cross-language harness.** S2-X ships the cross-binding *contract* (fixture invariants + Rust reference assertions). A follow-up adds a multi-process orchestrator that spawns Node + Python + Go peers, has them serve and call each other's typed handlers, and asserts the wire-level round-trip end-to-end. Requires CI infrastructure to build all three binding artifacts (`.node` + `.so` + `cdylib`) before launching; the per-binding unit tests already cover the typed-wrapper logic in isolation, so this follow-up's value is catching wire-level drift that the fixture's structural assertions can't.
6. **ABI 0x0003 cascade to downstream Go consumers.** `bindings/go/net/mesh_rpc.go::ExpectedABIVersion` now pins `0x0003` to match the substrate. Downstream Go binding consumers compiled against the pre-S1-A1 `0x0001` will panic at process init (per `mesh_rpc.go:618-625`); release notes for the next downstream Go binding bump must call out the override path (`NET_RPC_SKIP_ABI_CHECK=1`) for in-development consumers.
