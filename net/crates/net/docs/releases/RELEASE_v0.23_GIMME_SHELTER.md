# Net v0.23 — "Gimme Shelter"

*Named after the Rolling Stones' 1969 opener on Let It Bleed — the one Keith Richards wrote during a thunderstorm at his Robert Fraser flat, the one Merry Clayton recorded in a single overnight session. Same wire, same semantics, same surface — gimme shelter, or I'm gonna fade away.*

## Three waves, one substrate primitive, every binding finally idiomatic

The v0.23 release is the result of three planning passes against the user-facing nRPC + Python surfaces. The first wave — Slice 2 + Slice 1 from `NRPC_STREAMING_PARITY_AND_GO_BINDING.md` — closes the streaming-typed gap on Node and Python (client-streaming + duplex typed wrappers + observer + metrics) and ships a Go typed binding from day one with the same shapes. The second wave — `NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md` — promotes cancellation to a substrate primitive (`Mesh::reserve_cancel_token` / `Mesh::cancel(token)`) and routes every binding through it instead of letting three parallel binding-local cancel registries diverge further; in the same wave, every observer hook gets a bounded mpsc + drop counter so a slow callback can no longer pin the substrate's dispatch thread. The third wave — `PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md` — adds `Async`-prefixed siblings to every Python class that today calls `runtime.block_on(...)`, so asyncio-native consumers (FastAPI, LangGraph, an `aiohttp` sidecar, an `asyncio.gather` fan-out) don't have to fall back to a `ThreadPoolExecutor`.

The release's organizing observation: every binding had been growing its own shim layer to compensate for substrate gaps. The napi binding owned a `cancel_registry: HashMap<u64, AbortHandle>` with its own CR-13 race fix. The pyo3 binding owned a `Cancellable` pyclass with its own `close_notify` + `tokio::select!` pattern. The Go FFI owned a `cancel_registry` with its own Q18 orphan-TTL GC. Three implementations of the same idea, each with its own subtle bug fixes. v0.23 promotes the idea once, at the substrate, and the bindings stop holding their own state. Same shape for the observer machinery: three "callbacks must be cheap" footguns collapse into one bounded queue at the substrate boundary with a single drop counter that surfaces on every language's `metricsSnapshot` / `metrics_snapshot` / `MetricsSnapshot`. And the Python async surface lifts every `block_on` site once instead of asking users to wrap each binding call in a thread pool.

Below: the wins, grouped by where they fire.

---

## Streaming parity across Node, Python, Go (one typed shape, four call shapes)

Before v0.23, the typed-nRPC matrix had holes. Node + Python had unary + server-streaming typed wrappers but no client-streaming or duplex typed surface. Go had no typed wrapper at all. v0.23 fills the matrix.

**Node `TypedMeshRpc.serveClientStream` + `callClientStream` + `TypedClientStreamCall`.** Mirror of the Rust SDK's `serve_rpc_client_stream_typed` + `call_client_stream_typed`. JSON encode on `send`, JSON decode on `finish`; encode failures throw `nrpc:codec_encode`, decode failures throw `nrpc:codec_decode`, all through the existing `classifyError` mapping. The server-side handler shim decodes each chunk and surfaces a malformed-request as `RpcAppError(NRPC_TYPED_BAD_REQUEST, ...)` so callers observe typed Application status instead of generic Internal.

**Node `TypedMeshRpc.serveDuplex` + `callDuplex` + duplex typed wrappers.** `TypedDuplexCall<Req, Resp>` with `send` / `finishSending` / `next` / `intoSplit` / `close`; `TypedDuplexSink<Req>` + `TypedDuplexStream<Resp>` for the split halves; `TypedResponseSink<Resp>` for the server side. Handler signature is the JS-idiomatic `(stream, sink) =>` form, not the napi-binding's destructured `[stream, sink]` tuple — the typed wrapper destructures before invoking the user handler.

**Python `TypedMeshRpc.serve_client_stream` + `call_client_stream` + `TypedClientStreamCall`.** Same shape as Node. Sync iterator + context-manager (`__enter__` / `__exit__`); decode failure on a chunk raises `RpcCodecError` and closes the underlying stream. Handler signature is `(stream: TypedRequestStream) -> Resp`; decode failure on the first request chunk surfaces as `RpcAppError(NRPC_TYPED_BAD_REQUEST, ...)`.

**Python `TypedMeshRpc.serve_duplex` + `call_duplex` + duplex typed wrappers.** `TypedDuplexCall` / `TypedDuplexSink` / `TypedDuplexStream` / `TypedResponseSink`; `__next__` raises `StopIteration` on EOF, decode failure closes the call and raises `RpcCodecError`.

**`TypedMeshRpc.setObserver` + `metricsSnapshot` on Node and Python.** The raw napi + pyo3 `MeshRpc` classes gain `setObserver(handler)` + `metricsSnapshot()` (and `set_observer(callable)` + `metrics_snapshot()` for Python); the typed wrappers expose the same surface. `RpcCallEvent` is a JS interface / Python dataclass with tagged-union status (`Ok` / `Error(message)` / `Timeout` / `Canceled`), per-call latency, request/response byte counts, and direction. The mid-call swap is atomic via the substrate's `ArcSwapOption<RpcObserverHandle>`.

**Go typed binding — full surface in one file.** New `bindings/go/net/mesh_rpc_typed.go` ships every shape from day one:

- `TypedCall[Req, Resp]` + `TypedCallService[Req, Resp]` + `TypedServe[Req, Resp]` — unary, mirror of `rpc.call<Req, Resp>(...)` ergonomics with one extra positional argument (the `*TypedMeshRpc` itself), free-function shape because Go forbids type parameters on methods.
- `TypedCallStreaming[Req, Resp]` + `*TypedRpcStream[Resp]` — server-streaming with `Recv()` returning `(Resp, error)` + `ErrStreamDone` sentinel on EOF.
- `TypedCallClientStream[Req, Resp]` + `*TypedClientStreamCall[Req, Resp]` + `TypedServeClientStream[Req, Resp]` — client-streaming.
- `TypedCallDuplex[Req, Resp]` + `*TypedDuplexCall[Req, Resp]` + `Split()` halves + `TypedServeDuplex[Req, Resp]` — duplex.
- `(*TypedMeshRpc).SetObserver(handler)` + `MetricsSnapshot()` — observer + metrics through the new `net_rpc_set_observer` + `net_rpc_metrics_snapshot` FFI symbols.

`RpcAppError(code, detail)` minted through `NewRpcAppError(NrpcTypedBadRequest, ...)` / `NewRpcAppError(NrpcTypedHandlerError, ...)` matches the canonical `nrpc:app_error:0x<code>:<body>` shape; the Rust binding's `parse_js_app_error` reuses the same parser for Go consumers. The existing `*RpcError` Go type classifies codec failures as `RpcKindCodecEncode` / `RpcKindCodecDecode` — no `RpcError` changes needed.

**Cross-language streaming round-trip test.** `tests/cross_lang_nrpc/` grows golden vectors for client-stream + duplex Application-status round-trips; Rust-side reference asserts the typed-handler-raising-`RpcAppError` shape lands at the caller as the expected wire-level error.

---

## Observer dispatch as bounded mpsc + drop counter

The v1 typed-nRPC observer contract was "callbacks must be cheap; the substrate dispatch thread blocks until your callback returns." That contract lasts about a week in production before a user wires a Prometheus exporter or a disk-flushing log sink into `setObserver` and mesh-wide RPC latency spikes. v0.23 fixes the contract.

**Bounded-mpsc per mesh, drop counter as a monotonic u64.** Each binding now wires a 1024-event bounded mpsc between the substrate's dispatch path and the observer worker. Substrate `on_call` does `try_send`; full → atomic-counter increment, never blocks. The dispatch thread's per-event cost drops from "TSFN Mutex acquire" (napi) / "fresh `spawn_blocking` per event" (pyo3) / "synchronous C function pointer call" (Go FFI) to "atomic counter inc on a single `AtomicU64`."

**One worker per binding installs the observer.** The worker drains the receiver and pumps each event to the registered consumer: napi → TSFN; pyo3 → GIL-acquired Python callable; Go FFI → C function pointer. One worker = serialized callback invocation, matching each language's natural threading model. The worker dies when the sender drops (i.e. when `setObserver(None)` is called and the observer `Arc` is released).

**`observerDroppedTotal` / `observer_dropped_total` / `ObserverDroppedTotal` on every snapshot.** Process-global `AtomicU64`; reads-and-leaves (monotonic) for Prometheus exporter ergonomics. Surfaces as a top-level field on every binding's `RpcMetricsSnapshot`. Go consumers additionally get a `net_rpc_observer_dropped_total() -> u64` FFI symbol so they can read the counter without paying the JSON-decode cost on the snapshot path.

**Cortex-side consolidation.** The mpsc plumbing lives in the substrate's `cortex` module (`ObserverChannel<E>` + `OBSERVER_BUFFER_CAPACITY` constant) — one centralized implementation, three thin per-binding wrappers. A future tunable on the buffer capacity is a one-place change instead of three.

**`Arc<RpcCallEvent>` through the channel.** Observer events now flow as `Arc<RpcCallEvent>` from the substrate's emit site, deferring the per-binding POD-conversion work to the drain worker. The dispatch thread allocates the event once, increments the Arc, and moves on; the worker pays the per-binding conversion cost on a non-hot-path thread.

---

## Cancellation as a substrate primitive

Three bindings, three cancel registries. Promoted to one substrate primitive in v0.23.

**`CallOptions::cancel_token: Option<u64>` + `Mesh::reserve_cancel_token` + `Mesh::cancel(token)`.** Reserve a token from the mesh; pair with `cancel(token)` from any thread to abort the in-flight call. Honored uniformly by `call` / `call_service` / `call_streaming` / `call_client_stream` / `call_duplex` — the substrate registers the token's abort handle at construction and removes it on resolution. Drop-on-cancel emits CANCEL on the wire via the existing per-call-shape Drop impls (`UnaryCallGuard::Drop`, `ClientStreamCallRaw::Drop`, `DuplexCallRaw::Drop`).

**Per-mesh `cancel_registry`.** `parking_lot::Mutex<HashMap<u64, CancelEntry>>` keyed by token. `CancelEntry` carries `cancelled: bool` (CR-13: cancel before register), `handle: Option<AbortHandle>` (unary + streaming construction), `close_notify: Option<Weak<Notify>>` (streaming post-construction), `marked_at: Option<Instant>` (Q18: orphan TTL). Lifted from the napi binding's existing pattern with the Go FFI's orphan-TTL GC (default 120s) merged in.

**Race-safe across the reserve-then-call gap.** A cancel that arrives BEFORE the call's abort handle is registered (the gap between `reserve` and call construction) latches a `cancelled = true` flag on the orphan entry; when the call later registers, it observes the flag and aborts immediately. Mirrors the napi binding's CR-13 fix at the SDK layer once instead of three times.

**Binding migration: thin pass-through over the substrate primitive.**

- **napi.** `lock_cancel_registry()` and `NEXT_CANCEL_TOKEN: AtomicU64` are deleted. `reserveCancelToken` / `cancelCall` napi methods now delegate to `Mesh::reserve_cancel_token` / `Mesh::cancel`. `callClientStream` and `callDuplex` populate `opts.cancel_token` from the incoming `CallOptions`. The typed wrapper drops `stripSignal` for streaming entries and wires `wireAbortSignal` end-to-end.
- **pyo3.** `Cancellable.__init__` reserves a token from the mesh; `Cancellable.cancel()` calls `mesh.cancel(token)`. `call_client_stream` / `call_duplex` extract `opts['cancel']` and populate `CallOptions::cancel_token`. The Notify-based `close_notify` path on `PyClientStreamCall` / `PyDuplexCall` becomes an internal implementation detail; the substrate registers a `Weak<Notify>` against it.
- **Go FFI.** The file-local `cancel_registry` is deleted. `net_rpc_reserve_cancel_token` / `net_rpc_cancel_call` become pass-throughs. New cancellable FFI variants — `net_rpc_call_client_stream_cancellable` + `net_rpc_call_duplex_cancellable` — populate `opts.cancel_token` and forward to the SDK. The Go typed wrapper's `TypedCallClientStream` / `TypedCallDuplex` now propagate `ctx.Context` through unchanged; the raw layer honors it.

**Typed-wrapper pass-throughs.** Node `AbortSignal` for streaming, Python `Cancellable` for streaming, Go `ctx.Context` for streaming — all wired end-to-end. The v1-era "v1: close()-only" caveat is gone from every streaming-entry docstring.

**SDK-level cancel-contract integration tests.** `tests/integration_mesh_cancel.rs` pins the contract before any binding depends on it: `cancel_unary_mid_flight_emits_cancel_on_wire`, `cancel_streaming_mid_drain_emits_cancel`, `cancel_client_stream_mid_send_emits_cancel`, `cancel_duplex_mid_send_emits_cancel`, `cancel_before_construction_aborts_cleanly`, `cancel_after_resolution_is_noop`, `cancel_zero_token_is_noop`, `orphan_ttl_gc_evicts_unused_reservations`. The bindings then test only their pass-through layer.

**Cancellation cookbook (cross-binding).** Documented in the v3 plan and surfaced in the per-binding README: Node `AbortSignal`, Python `Cancellable`, Go `context.Context` — three idiomatic surfaces, one substrate primitive, the same wire-level outcome. Power users in every language can also reserve tokens directly via the raw FFI surface for cross-call cancel sharing.

---

## Python async SDK — side-by-side `Async*` surface for every I/O class

The pyo3 binding spans 16 modules and ~141 `runtime.block_on(...)` call sites. Every one of those took a Python thread + a tokio worker hostage for the call's duration; in an asyncio-native consumer that pattern serializes what should be concurrent and burns the application's thread budget. v0.23 adds `Async`-prefixed siblings for every class with async-worthy I/O. Existing sync API is **unchanged**.

**`AsyncNetMesh` + `AsyncNetStream`.** Shared `MeshNode` with the sync `NetMesh` — `AsyncNetMesh(mesh)` constructs against the existing peer-connection state without re-handshaking. `connect` / `accept` / stream enumeration return awaitables; `peer_count` / `node_id` / `public_key` are sync (in-memory reads).

**`AsyncMeshRpc` — full raw client + server.**

- `call` / `call_service` / `find_service_nodes` (unary + service-discovery unary + local lookup).
- `call_streaming` → `AsyncRpcStream` with `__aiter__` + `__anext__` (server-streaming).
- `call_client_stream` → `AsyncClientStreamCall` with awaitable `send` / `finish` (client-streaming).
- `call_duplex` → `AsyncDuplexCall` / `AsyncDuplexSink` / `AsyncDuplexStream` (duplex + split halves).
- `serve` / `serve_client_stream` / `serve_duplex` — accept EITHER sync `def` or `async def` handlers, detected via `inspect.iscoroutinefunction` at register time. Sync handlers run on the substrate's `spawn_blocking` path; async handlers run as coroutines on a dedicated dispatcher event loop so the tokio worker can drive them without a Python loop on its own thread.

**`AsyncTypedMeshRpc` + every typed streaming companion.** `AsyncTypedMeshRpc`, `AsyncTypedRpcStream`, `AsyncTypedClientStreamCall`, `AsyncTypedDuplexCall`, `AsyncTypedDuplexSink`, `AsyncTypedDuplexStream`, `AsyncTypedRequestStream`, `AsyncTypedResponseSink` — JSON-encode/decode the same way the sync wrappers do; only difference is `await self._raw.foo(...)` vs `self._raw.foo(...)`.

**Wave T2 production essentials.** `AsyncNetDb` (get/put/delete/list/batch_put), `AsyncRedex` + `AsyncRedexFile` + `AsyncRedexTailIter` (`async for evt in file.tail(from_seq)`), `AsyncMemoriesAdapter` + `AsyncMemoryWatchIter`, `AsyncTasksAdapter` + `AsyncTaskWatchIter` (every push-stream becomes a PEP-525 async iterator). `AsyncDaemonRuntime` + `AsyncDaemonHandle` + `AsyncMigrationHandle` (daemon spawn / migrate / wait). `AsyncMeshBlobAdapter` + module-level `async_blob_publish` / `async_blob_resolve`.

**Wave T3 operator / specialty.** `AsyncDeckClient` + `AsyncSnapshotStream` + `AsyncStatusSummaryStream` + `AsyncAdminCommands` + `AsyncIceCommands`. `AsyncMeshOsDaemonSdk` + `AsyncMeshOsDaemonHandle` (async `receive` / `publish_log` / `graceful_shutdown`). `AsyncMeshQueryRunner.execute` (async-RPC backed; the query AST classes stay sync — they're pure data builders). `AsyncFoldQueryClient` + `AsyncRegistryClient`.

**Shared tokio runtime + shared `MeshNode`.** Sync and async classes share one runtime per process and one `Arc<MeshNode>` per `NetMesh`. A server registered via `MeshRpc.serve(...)` is callable from `AsyncMeshRpc.call(...)`; an `async def` handler registered via `AsyncMeshRpc.serve(...)` is callable from `MeshRpc.call(...)`. Same wire, same identity, same cap-index entries.

**Async substrate-cancel propagation via the v3 primitive.** Every async I/O method mints a substrate cancel token, attaches an asyncio-cancel listener that calls `Mesh::cancel(token)`, detaches on call resolution. `asyncio.wait_for(async_rpc.call(...), timeout=0.1)` on a long-running call surfaces `asyncio.TimeoutError` on the caller and `Cancelled` on the server's observer — same shape as Node's `AbortSignal` and Go's `ctx.Context`, just driven by the Python task lifecycle.

**Server-side dispatcher event loop.** `async def` server handlers dispatched from a tokio worker need a Python asyncio loop to drive the coroutine. The bridge lazily spawns a single daemon Python thread running `asyncio.run_forever()` on a fresh loop and routes every handler coroutine through `pyo3_async_runtimes::into_future_with_locals(&dispatcher_locals, coro)` — one loop per process, serialized GIL acquisition on the drain worker, no per-handler thread costs.

**`pyo3-async-runtimes` (tokio backend) as a default dep.** Bridges init-once via `init_with_runtime(&runtime)` in the pymodule init. Wheel grows ~50 KB; no feature flag bifurcation.

**Test surface — TX cross-cutting matrix.** `tests/test_async_interop.py` pins the `(sync | async) caller × (sync | async) server` matrix on the unary shape (4 tests; full-shape coverage lands incrementally as a follow-up). `AsyncNetMesh(mesh)` shared-handshake invariant pinned (no re-handshake when constructed against an already-connected mesh). Per-module smoke tests cover the T2 + T3 surfaces.

---

## Test hygiene

- **Lib suite continues to expand.** New tests across the v3 cancel contract (`integration_mesh_cancel.rs` — every call shape, the orphan-TTL GC, the CR-13 race), the bounded-mpsc drop counter (per-binding under-load tests in napi + pyo3 + rpc-ffi), the cross-language streaming round-trip (`tests/integration_nrpc_cross_lang_streaming.rs` — client-stream + duplex + observer firing), and the Python async surface (`tests/test_async_interop.py` + per-module smoke tests for every `Async*` class).
- **Cross-language wire contract test.** Every binding now also pins the canonical observer event shape and metrics-snapshot envelope (`observer_dropped_total` field present, `abi_version_expected = 4`). A binding that drifts fails its compatibility test.
- **ABI version bump to `0x0004` (rpc-ffi).** Additive — existing 0x0003 symbols stay unchanged; new symbols are `net_rpc_call_client_stream_cancellable`, `net_rpc_call_duplex_cancellable`, `net_rpc_set_observer`, `net_rpc_metrics_snapshot`, `net_rpc_observer_dropped_total`.
- **`cargo clippy --features meshos,deck,aggregator --all-features --all-targets -- -D warnings` clean.** Strict floor from v0.20.2 stays armed.
- **`cargo doc --features meshos,deck,aggregator --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`.** Intra-doc links across the new substrate cancel API + the per-binding cancel cookbook + the Python async surface all resolved.
- **Codecov coverage** unchanged in posture — ~90% substrate, informational on CI status.

---

## Breaking changes

### `NET_RPC_ABI_VERSION` bumps from `0x0003` to `0x0004`

Additive symbol additions only; existing 0x0003 functions are unchanged. Downstream Go binding consumers compiled against the pre-bump version panic at process init via the `ExpectedABIVersion` check at `bindings/go/net/mesh_rpc.go:586-595`. Override with `NET_RPC_SKIP_ABI_CHECK=1` for in-development consumers; the next downstream Go binding cut should pin `0x0004`.

### Observer dispatch is no longer synchronous on the substrate dispatch thread

The v1 contract "callbacks must be cheap; the dispatch thread blocks until your callback returns" no longer holds. Observer events flow through a 1024-event bounded mpsc + worker task per binding. Behavior change: slow callbacks no longer pin the substrate; on overflow, events are dropped and the `observer_dropped_total` counter increments. Consumers that relied on the sync ordering (none in tree) get the new shape; callbacks should still be cheap, but the substrate is no longer on fire when they aren't.

### `CallOptions::cancel_token` lands on the substrate `CallOptions` struct

Additive Rust-side field with `Default::default() == None`. Existing `..Default::default()` callers continue to compile. Direct struct-literal callers that named every field need to add `cancel_token: None`.

### Per-binding cancel registries are gone (internal-only break)

The napi binding's `lock_cancel_registry` + `NEXT_CANCEL_TOKEN`, the pyo3 binding's `Cancellable` internal state, and the Go FFI's `cancel_registry` + `CancelEntry` are deleted. Public APIs on each binding (`reserveCancelToken` / `cancelCall` napi methods, `Cancellable` pyclass, `net_rpc_reserve_cancel_token` / `net_rpc_cancel_call` FFI symbols) are preserved as pass-throughs; consumers that reached into the binding's private state directly need to switch to the substrate primitive.

### Node `TypedMeshRpc.callClientStream` / `callDuplex` honor `signal` (was previously ignored)

The streaming entries' `opts.signal` was documented v1-and-only as `close()`-only; the wrapper called `stripSignal` to drop it. v0.23 wires `wireAbortSignal` end-to-end, so `signal.aborted` now fires `raw.cancelCall(token)` and the call rejects with `RpcCancelledError`. Consumers passing `opts.signal` to streaming entries were no-ops before; they'll start firing on cancel now.

### Python `TypedMeshRpc.call_client_stream` / `call_duplex` honor `opts['cancel']` (was previously ignored)

Same shape as Node. Passing a `Cancellable` to a streaming entry was a no-op in v1; it now propagates to substrate-level cancel. Consumers that were relying on "Cancellable is ignored for streaming" need to invoke `cancel()` only when they actually want cancel — the previous behavior of silent ignoring is no longer the default.

### Go `TypedCallClientStream` / `TypedCallDuplex` honor `ctx.Done()` (was previously deadline-only)

The streaming entries previously honored `ctx.Deadline()` for the wire deadline but did not wire `ctx.Done()` to a cancel propagation path. v0.23 wires both. Cancelling the context now fires CANCEL on the wire.

### `RpcMetricsSnapshot` grows `observer_dropped_total` (envelope-level u64)

Wire shape additive — postcard appends; existing readers tolerate the additional field. SDK consumers that built the struct by hand grow one field; consumers that rendered the snapshot via the per-binding `MetricsSnapshot` POD see the new field populated.

### New Python `Async*` classes are exported from `net`

`from net import AsyncMeshRpc, AsyncTypedMeshRpc, AsyncNetMesh, AsyncNetStream, AsyncNetDb, AsyncRedex, AsyncRedexFile, AsyncRedexTailIter, AsyncMemoriesAdapter, AsyncMemoryWatchIter, AsyncTasksAdapter, AsyncTaskWatchIter, AsyncDaemonRuntime, AsyncMigrationHandle, AsyncMeshBlobAdapter, AsyncDeckClient, AsyncAdminCommands, AsyncIceCommands, AsyncMeshOsDaemonSdk, AsyncMeshOsDaemonHandle, AsyncMeshQueryRunner, AsyncRegistryClient, AsyncFoldQueryClient` all succeed. Existing sync imports are unchanged. Consumers that introspect `dir(net)` may see the new names.

### Go typed binding lives in a new file

`bindings/go/net/mesh_rpc_typed.go` is new. Existing consumers of the raw `*MeshRpc` are unaffected; consumers who want the typed surface import the new symbols from the same `net` package.

---

## How to upgrade

1. **Rust consumers — update the dependency to `0.23`.** Direct struct-literal callers of `CallOptions` add `cancel_token: None`; everyone else recompiles unchanged.

2. **Operators with custom observer callbacks — re-read the contract.** The "callbacks must be cheap" guidance still applies (don't intentionally make the worker the bottleneck), but the substrate no longer pins the dispatch thread when a callback misbehaves. Monitor `observerDroppedTotal` / `observer_dropped_total` / `ObserverDroppedTotal` in `metricsSnapshot` to see if your callback is too slow for production load; size the upstream queue or push events into an `asyncio.Queue` / Go channel / Node async iterator if drops are appearing.

3. **Cancellation users — migrate from binding-specific cancel surfaces to idiomatic per-language ones.**
   - **Node:** `new AbortController()` + pass `signal` in `CallOptions`. Streaming entries now honor it end-to-end.
   - **Python:** `Cancellable()` + pass `opts={'cancel': cancellable}`. Streaming entries now honor it. Async callers use `asyncio.wait_for(...)` or `task.cancel()` for free — the bridge converts asyncio cancellation into a substrate `Mesh::cancel`.
   - **Go:** `context.WithCancel(parent)` + pass `ctx` to every call. Streaming entries now honor `ctx.Done()`.
   - **Power users in any language:** raw `reserveCancelToken` / `reserve_cancel_token` / `net_rpc_reserve_cancel_token` + pass the token across multiple calls for shared-cancel scenarios.

4. **Node + Python typed users — adopt client-streaming + duplex.** `TypedMeshRpc.serveClientStream` / `serveDuplex` (Node) and `serve_client_stream` / `serve_duplex` (Python) are the new entry points. Handler signatures match the Rust SDK's typed surface. JSON codec is unchanged.

5. **Go users — adopt `TypedMeshRpc` from day one.** `import "net/bindings/go/net"`; `t := NewTypedMeshRpc(rawMesh)`; `result, err := TypedCall[Req, Resp](ctx, t, target, "svc.echo", req)`. Streaming + observer / metrics work the same way as the Rust SDK.

6. **Python asyncio consumers — flip every blocking call to its async sibling.** `MeshRpc(mesh)` → `AsyncMeshRpc(mesh)`; `rpc.call(...)` → `await arpc.call(...)`; `for chunk in stream` → `async for chunk in astream`. Sync API unchanged; both APIs coexist on the same `NetMesh`. The migration cookbook in `bindings/python/README.md` walks through a service-by-service move. asyncio cancellation works transparently via `asyncio.wait_for` / `task.cancel()`.

7. **`async def` server handlers — Python only.** Register an `async def` handler with `AsyncMeshRpc.serve(...)` or `AsyncMeshRpc.serve_client_stream(...)` or `AsyncMeshRpc.serve_duplex(...)`. The bridge detects the coroutine function at register time and dispatches every invocation through a server-side dispatcher event loop. Sync handlers continue to run on `spawn_blocking`; the choice is per-handler, not per-class.

8. **No CI config change required.** Strict clippy floor stays armed; rustdoc warnings stay denied; the test-side allow-list is unchanged. CI adds the cross-language streaming round-trip job and bumps the Go binding's pinned ABI version to `0x0004`.

9. **Operators — bump the binary.** Pre-built `net-mesh`, `net-deck`, `net-aggregator-daemon` archives land for every supported target (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64). Wire format is additive from v0.22; mixed-version fleets handshake cleanly. The new `RpcMetricsSnapshot.observer_dropped_total` field and the `CallOptions::cancel_token` field are postcard-appended; pre-v0.23 readers ignore them.

10. **Downstream Go binding consumers — update or override the ABI pin.** `bindings/go/net/mesh_rpc.go::ExpectedABIVersion` is now `0x0004`. Consumers compiled against `0x0003` panic at init. `NET_RPC_SKIP_ABI_CHECK=1` is the development override.

---

Released 2026-05-25.

## License

See [LICENSE](../../LICENSE-APACHE).
