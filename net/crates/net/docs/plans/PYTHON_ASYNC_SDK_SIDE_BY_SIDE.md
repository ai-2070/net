# Python SDK: side-by-side `async` + sync

Branch: `python-async-sdk` (suggested).
Predecessor: [`NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md`](./NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md) — v3 promoted cancellation to a substrate primitive (`Mesh::reserve_cancel_token` / `Mesh::cancel(token)`), which this plan reuses verbatim for `asyncio.Task.cancel()` propagation. No new substrate surface; everything lands in the pyo3 binding + Python typed wrapper.

## Scope

Add an `async` variant of the entire Python nRPC surface — `AsyncMeshRpc`, `AsyncTypedMeshRpc`, async streaming iterators, async handlers — alongside the existing sync `MeshRpc` / `TypedMeshRpc`. Both classes share the same `MeshNode` and tokio runtime; users mix as they like (sync caller + async server is fine; async caller + sync server is fine).

The existing sync API is **unchanged**. No method gets renamed, no signature shifts. Every user who imports `from net import MeshRpc` keeps working. The new surface adds, never replaces.

## Why now

1. **The substrate is already async.** The pyo3 binding wraps every call in `runtime.block_on(...)` with `py.detach(...)` to release the GIL — every sync call already pays the bridge cost of blocking a thread on a tokio task. Exposing the underlying async future to Python is a small mechanical step on top, not a redesign.
2. **The audience is bimodal.** Standard-library Python code (scripts, CLI tools, single-threaded daemons) is sync-natural; modern Python servers (FastAPI, ASGI workers, asyncio-based agent frameworks like LangGraph) are async-natural and pay a real cost for sync calls — every blocking call burns a thread-pool slot. The same caller fits both shapes today only by spawning threads around the sync API, which defeats async's whole point. Splitting at the SDK boundary lets each consumer use the idiom they already wrote everything else in.
3. **v3 made this safe to ship.** Before v3 the bindings each carried their own cancel registry and the async story would have needed a parallel cancel adapter. With v3's `Mesh::reserve_cancel_token` + `Mesh::cancel(token)` substrate primitive, an `asyncio.Task.cancel()` listener is the exact same shape as v3's `AbortSignal` listener in napi — mint a token, register a callback, on cancel call `Mesh::cancel(token)`. No new substrate work needed.
4. **No upstream breaking change.** Side-by-side means side-by-side. `AsyncMeshRpc` is additive at the pyo3 layer, additive at the typed-wrapper layer, additive at the typed-tests layer. Rollback is a `git revert` per slice; sync users see nothing.

## Locked decisions

1. **Naming: `AsyncMeshRpc` + `AsyncTypedMeshRpc`** (parallel classes). Matches the strongest precedent in the Python ecosystem (`httpx.Client` / `httpx.AsyncClient`, `sqlalchemy.Session` / `sqlalchemy.AsyncSession`). Rejected: `acall`/`async_call` methods on the existing class (clutters the sync class and forces every user to learn both surfaces), separate `net.async_mesh_rpc` module (drives import-path drift), async-only (the whole point of "side-by-side" is keeping sync working).
2. **Async bridge: `pyo3-async-runtimes` (tokio backend).** The maintained successor to `pyo3-asyncio`, tracks pyo3 releases. We're already on pyo3 0.28; `pyo3-async-runtimes` 0.28 is API-compatible. Rejected: hand-rolling the future-to-coroutine bridge (loose timing semantics; pyo3-async-runtimes has solved the runtime-coordination problem already).
3. **Shared tokio runtime.** The existing `Arc<Runtime>` on `NetMesh` is the same runtime both sync `block_on` and async `future_into_py` use. One process = one tokio runtime, regardless of how many `MeshRpc` / `AsyncMeshRpc` instances exist. Rejected: per-instance runtimes (memory waste, defeats the substrate's per-process MeshNode model).
4. **Cancellation: asyncio cancellation fires `Mesh::cancel(token)`.** When a Python `asyncio.Task` is cancelled, the in-flight Rust future receives the cancel signal via the v3 `CancelRegistry`. Mirrors the napi `AbortSignal` → `Mesh::cancel(token)` wiring shipped in C-A1. The Python typed `Cancellable` class stays as the explicit-cancel surface; asyncio cancellation becomes the implicit/ambient surface. Both reduce to the same substrate primitive.
5. **Streaming: `async for chunk in stream` (PEP 525 async iterators).** Every streaming response (`AsyncRpcStream`, `AsyncTypedRpcStream`, the async duplex stream half) implements `__aiter__` + `__anext__`. The sync iterators (`__iter__` + `__next__`) stay on the sync classes; no class implements both protocols.
6. **Async server handlers are first-class.** A handler registered with `async_rpc.serve_async("svc", async_handler_fn)` runs as a coroutine inside the tokio runtime, never blocking the substrate dispatch task. Sync handlers registered against `MeshRpc.serve("svc", sync_fn)` still run under `tokio::task::spawn_blocking` (existing behavior). Both can run against the same mesh.
7. **Observer / metrics: unchanged.** v3's bounded-mpsc + drop-counter design already isolates observer dispatch from the call hot path; the observer callable runs in a tokio worker that acquires the GIL. Whether the callable is `def` or `async def` is an internal question — for now keep `def`-only and surface async observers as a deferred follow-up if real consumers need it.
8. **Typed wrapper is its own class.** `AsyncTypedMeshRpc(raw: AsyncMeshRpc)` parallels `TypedMeshRpc(raw: MeshRpc)`. The `__init__` accepts either the raw async pyo3 class or a duck-typed test stub (mirrors the sync typed wrapper's pattern). No shared base class — Python's inheritance model would force protocol-level abstractions that hurt readability without buying composition value.
9. **Single wheel ships both.** No feature flag, no opt-in, no `pip install net-mesh[async]`. The async surface is part of `net-mesh` from the first release that lands it. The dependency footprint is ~1 crate (`pyo3-async-runtimes`) which doesn't grow the wheel measurably.

Tagged `[A | B | C | T | D]`:

- **A** — raw pyo3 async client (caller-side: unary + streaming + cancel)
- **B** — Python typed async wrapper (`AsyncTypedMeshRpc` + streaming wrappers)
- **C** — async server handlers (raw + typed)
- **T** — tests (raw-layer pytest-asyncio + typed-layer round-trip + sync/async interop)
- **D** — docs + migration notes

---

## Status

| ID    | Pri | Area            | Title                                                                                                                  |
|-------|-----|-----------------|------------------------------------------------------------------------------------------------------------------------|
| A-1   | H   | pyo3 / deps     | Add `pyo3-async-runtimes` dep + initialize the tokio↔asyncio bridge in `_net::register`                                |
| A-2   | H   | pyo3 raw        | `_net.AsyncMeshRpc` class wrapping `MeshNode`; async `call` / `call_service` / `find_service_nodes` returning awaitables |
| A-3   | H   | pyo3 raw        | Async streaming: `call_streaming` returns an async iterator; `AsyncRpcStream.__anext__` awaits one chunk from the SDK stream |
| A-4   | H   | pyo3 raw        | Async client-streaming: `call_client_stream` → `AsyncClientStreamCall` with awaitable `send` + `finish`                |
| A-5   | H   | pyo3 raw        | Async duplex: `call_duplex` → `AsyncDuplexCall` with awaitable `send` / `finish_sending` / async-iter receive          |
| A-6   | H   | pyo3 raw        | asyncio cancellation propagation: cancel-on-task-cancel reuses the v3 `Mesh::cancel(token)` primitive                  |
| B-1   | H   | typed wrapper   | `AsyncTypedMeshRpc(raw)` with async typed `call` / `call_service` (codec + raw await)                                  |
| B-2   | H   | typed wrapper   | `AsyncTypedMeshRpc.call_streaming` returning `AsyncTypedRpcStream` (typed `async for`)                                 |
| B-3   | H   | typed wrapper   | `AsyncTypedMeshRpc.call_client_stream` → `AsyncTypedClientStreamCall`                                                   |
| B-4   | H   | typed wrapper   | `AsyncTypedMeshRpc.call_duplex` → `AsyncTypedDuplexCall` / `AsyncTypedDuplexSink` / `AsyncTypedDuplexStream`           |
| C-1   | M   | pyo3 server     | Detect `async def` handlers on `AsyncMeshRpc.serve` / `serve_client_stream` / `serve_duplex`; run as coroutines        |
| C-2   | M   | typed wrapper   | `AsyncTypedMeshRpc.serve` / `serve_client_stream` / `serve_duplex` accept async handlers                                |
| T-1   | H   | tests           | pytest-asyncio round-trip — async caller × sync server, sync caller × async server, async × async (all four shapes)   |
| T-2   | H   | tests           | Async cancellation: `asyncio.wait_for(..., timeout)` cancels in-flight call cleanly, server observes CANCEL            |
| T-3   | M   | tests           | Async streaming back-pressure: slow consumer doesn't starve the substrate                                              |
| D-1   | M   | docs            | `bindings/python/README.md` async section + migration cookbook (mixing sync + async, sharing one NetMesh)              |

**ABI / SDK surface impact.** No substrate change. No rpc-ffi ABI bump. The Python `_net` pyo3 module gains new exported symbols (`AsyncMeshRpc`, `AsyncRpcStream`, etc.) — additive only; nothing existing is renamed or removed. Wheel size +1 dependency (`pyo3-async-runtimes`, ~50 KB compiled).

---

## Phasing

**Recommended order: foundations → caller-side → server-side → tests.**

1. **Wave 1 — pyo3 foundations + raw async caller-side (A-1 → A-6).** A-1 lands the dep + module-level runtime wiring; A-2 establishes the pattern for awaiting Rust futures from Python; A-3/A-4/A-5 extend the pattern to streaming shapes; A-6 wires asyncio cancellation to the v3 substrate cancel-token. Each slice is independently testable against the pyo3 layer.
2. **Wave 2 — typed wrapper (B-1 → B-4).** Pure Python (no Rust). Each slice mirrors the sync typed wrapper structurally; the change set is small per slice (~50 lines) and entirely additive.
3. **Wave 3 — async server handlers (C-1, C-2).** Detect `inspect.iscoroutinefunction(handler)` at `serve()` time; route async handlers through a coroutine-driving worker instead of `spawn_blocking`. Server-side async unlocks the FastAPI-style pattern where a handler's body awaits downstream calls.
4. **Wave 4 — tests + docs (T-1, T-2, T-3, D-1).** pytest-asyncio fixtures, four interop combinations, async-cancel pinning, docs.

Waves 1+2 can land in one release cycle. Wave 3 can land alongside or as a follow-up. Wave 4 lands incrementally per slice but blocks the v0.x release marker.

---

## Wave 1 — pyo3 foundations + raw async caller-side

### A-1 — `pyo3-async-runtimes` dep + runtime bridge

**Design.**
- Add `pyo3-async-runtimes = { version = "0.28", features = ["tokio-runtime"] }` to `bindings/python/Cargo.toml`. The version tracks the pyo3 we're already on.
- In `bindings/python/src/lib.rs::register`, before any pymodule setup, call `pyo3_async_runtimes::tokio::init_with_runtime(&runtime)` exactly once. The runtime is the same `Arc<Runtime>` `PyNetMesh` constructs; `pyo3-async-runtimes` borrows it (no double-runtime tax).
- The bridge is global per pyo3 process — there's only one Python process per `_net` import, so a one-shot `OnceLock` guard around `init_with_runtime` is correct.
- Document in the module docstring that any consumer constructing their own `NetMesh` shares the global tokio runtime with the async surface.

**Files touched.**
- `bindings/python/Cargo.toml`.
- `bindings/python/src/lib.rs` — add `init_with_runtime` call.

**Risk.** Low. `pyo3-async-runtimes` is stable and widely deployed (Polars, RustPython embeddings, etc.). The `init_with_runtime` API has been stable across the past two minor versions.

### A-2 — `_net.AsyncMeshRpc`: async `call` / `call_service` / `find_service_nodes`

**Design.**
- New `#[pyclass(name = "AsyncMeshRpc", module = "_net")]` in `bindings/python/src/mesh_rpc.rs`. Same constructor shape as `PyMeshRpc` — takes a `&NetMesh`, clones the underlying `Arc<MeshNode>`.
- Methods return `PyResult<Bound<PyAny>>` — pyo3-async-runtimes' `future_into_py` converts a Rust future into a Python awaitable. The async function body is the same `node.call(...)` future the sync class block-on's, just unwrapped instead of awaited synchronously.
- `find_service_nodes` is non-async on the SDK side (it's a sync DashMap read) — expose as a normal `def` method on `AsyncMeshRpc` too. Don't fake-async sync work.
- Error mapping reuses the existing `rpc_error_to_pyerr` helper.
- Drop-on-cancel comes for free: when the Python coroutine is cancelled, pyo3-async-runtimes aborts the underlying tokio task, which drops the Rust future and fires the SDK's `UnaryCallGuard::Drop` → CANCEL on the wire. (A-6 builds on this with explicit substrate cancel for the cancel-on-task-cancel race window.)

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — new `PyAsyncMeshRpc` struct + 3 methods.
- `bindings/python/src/lib.rs::register` — add `PyAsyncMeshRpc` to the pymodule.
- `bindings/python/python/net/__init__.py` — re-export `AsyncMeshRpc` from `_net`.

### A-3 — Async streaming: `call_streaming` → `AsyncRpcStream`

**Design.**
- `AsyncMeshRpc.call_streaming(target, service, req, opts)` returns an awaitable that resolves to an `AsyncRpcStream` (a new pyo3 class). The construction-side `block_on` is replaced by `future_into_py` over the same `node.call_streaming(...).await`.
- `AsyncRpcStream` holds `Arc<Mutex<Option<InnerRpcStream>>>` (same shape as the sync `PyRpcStream`). Exposes:
  - `__aiter__(slf)` returns `slf` (PEP 525 async-iterable contract).
  - `__anext__(slf)` returns an awaitable that resolves to a `bytes` chunk or raises `StopAsyncIteration` on clean EOF. Internally: take the stream lock, pull `InnerRpcStream::next().await`, release lock, return Python `bytes`.
  - `close()` runs synchronously (just drops the inner stream); `async def aclose()` is a thin awaitable alias for consistency.
- The async-iter contract requires that `__anext__` is callable repeatedly; each call returns a fresh awaitable. The Rust side wraps each pull in `future_into_py` per call.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — new `PyAsyncRpcStream` + the `call_streaming` method.

### A-4 — Async client-streaming: `call_client_stream` → `AsyncClientStreamCall`

**Design.**
- Mirror of A-3 for the client-streaming shape. `AsyncMeshRpc.call_client_stream(target, service, opts)` returns an awaitable yielding `AsyncClientStreamCall`.
- `AsyncClientStreamCall` exposes:
  - `async def send(body: bytes)` — awaits one upload credit + sends.
  - `async def finish() -> bytes` — drains the terminal response.
  - `def close()` / `async def aclose()` — drop the call.
  - `call_id` property (sync read — just returns the cached u64).
- Each method body is `future_into_py(py, async move { call.send(body).await })` pattern.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — new `PyAsyncClientStreamCall` + the method.

### A-5 — Async duplex: `call_duplex` → `AsyncDuplexCall`

**Design.**
- `AsyncMeshRpc.call_duplex(target, service, opts)` → awaitable yielding `AsyncDuplexCall`.
- `AsyncDuplexCall`:
  - `async def send(body: bytes)`.
  - `async def finish_sending()`.
  - `__aiter__` / `__anext__` for the receive half (yields `bytes`).
  - `async def into_split() -> Tuple[AsyncDuplexSink, AsyncDuplexStream]` — splits like the sync version. The two halves are independent classes; cancellation transfers (per the substrate's keep-alive pattern from v3 C-S1 pt2).
- `AsyncDuplexSink`: `send`, `finish`, `close`/`aclose`.
- `AsyncDuplexStream`: `__aiter__` + `__anext__`, `close`/`aclose`.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — three new pyclasses + the method.

### A-6 — asyncio cancellation propagation

**Design.**
- pyo3-async-runtimes' `future_into_py_with_locals` (or the equivalent) takes a `CancellationToken` from `tokio_util::sync::CancellationToken` — when the Python task is cancelled, the token fires. We bind that token to a substrate cancel-token: before each call, mint `cancel_token = node.reserve_cancel_token()`, populate `opts.cancel_token`, and on the Python task's cancel signal call `node.cancel(cancel_token)`.
- Implementation pattern (mirrors the napi v3 C-A1 wiring):
  ```rust
  let cancel_token = self.node.reserve_cancel_token();
  let mut opts = opts.clone();
  opts.cancel_token = Some(cancel_token);
  let node_for_cancel = self.node.clone();
  let fut = async move {
      tokio::select! {
          result = node.call(target, service, req, opts) => result,
          _ = py_cancel_signal.cancelled() => Err(InnerRpcError::Cancelled),
      }
  };
  future_into_py(py, fut)
  ```
- On the substrate side this needs no new work — `CancelRegistry` already pre-arms `Notify` on race-with-register, and the call's `select!` arm already short-circuits to `RpcError::Cancelled`. The "cancel signal" plumbing is purely on the pyo3 side.
- Streaming variants get the same treatment: construction is cancellable via the same mechanism, and the substrate's `spawn_stream_cancel_watcher` (from C-S1 pt2) handles mid-stream cancel through the same `CancelRegistry` entry.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — extend every async method body with the cancel-token plumbing.

**Risk.** pyo3-async-runtimes' cancellation hook surface may need a small adapter — confirm the exact API on the chosen version before implementing. If the API doesn't expose a `CancellationToken` directly, fall back to a per-call `Arc<Notify>` that the Python-task-cancellation closure trips.

---

## Wave 2 — Python typed async wrapper

### B-1 — `AsyncTypedMeshRpc` shell + async `call` / `call_service`

**Design.**
- New class in `bindings/python/python/net/mesh_rpc.py` (same file as the sync `TypedMeshRpc` — pure-Python wrappers all colocate).
- `AsyncTypedMeshRpc.__init__(self, raw)` accepts an `AsyncMeshRpc` (the new pyo3 class) or any duck-typed equivalent for testing.
- `async def call(target, service, req, opts=None) -> Resp`: encode `req` with the existing `_json_encode` helper, await `self._raw.call(target, service, body, opts)`, decode the response with `_json_decode`. The encode/decode error paths reuse the existing `RpcCodecError` mapping — no change.
- `async def call_service` is structurally identical.
- The sync `TypedMeshRpc` stays untouched.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — new class.
- `bindings/python/python/net/__init__.py` — re-export `AsyncTypedMeshRpc`.

### B-2 — `AsyncTypedMeshRpc.call_streaming` → `AsyncTypedRpcStream`

**Design.**
- Same encoding shape as B-1. The `AsyncRpcStream` (pyo3) returns raw `bytes`; `AsyncTypedRpcStream` decodes each chunk to the user's `Resp` type with `_json_decode`.
- Implements `__aiter__` + `__anext__` — `__anext__` awaits `self._raw.__anext__()`, decodes the bytes, returns the typed value. Codec failure raises `RpcCodecError` and closes the underlying stream (mirrors the sync wrapper's contract).

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — new `AsyncTypedRpcStream`.

### B-3 — `AsyncTypedMeshRpc.call_client_stream` → `AsyncTypedClientStreamCall`

**Design.**
- `AsyncTypedClientStreamCall.send(value)` encodes `value` and awaits `raw.send(body)`.
- `AsyncTypedClientStreamCall.finish() -> Resp` awaits `raw.finish()` and decodes.
- `async def aclose()` for explicit close; `__aenter__` / `__aexit__` for `async with` support.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — new `AsyncTypedClientStreamCall`.

### B-4 — `AsyncTypedMeshRpc.call_duplex` → `AsyncTypedDuplexCall` + sink/stream halves

**Design.**
- `AsyncTypedDuplexCall.send(value)` encodes + awaits.
- `__aiter__` + `__anext__` for the receive side.
- `async def into_split() -> Tuple[AsyncTypedDuplexSink, AsyncTypedDuplexStream]`.
- `AsyncTypedDuplexSink`: `send(value)`, `finish()`, `aclose()`.
- `AsyncTypedDuplexStream`: async iterator over decoded `Resp` values.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — three new classes.

---

## Wave 3 — Async server-side handlers

### C-1 — Raw async handler detection + coroutine bridge

**Design.**
- `AsyncMeshRpc.serve(service, handler)` accepts either a `def` or `async def` handler. Detection at register time: `inspect.iscoroutinefunction(handler)` (Python side) or, since the Rust side receives a `Py<PyAny>`, check `handler.is_coroutine_function(py)` via a helper.
- For an async handler, the existing `PyRpcHandler` adapter changes its `on_request` to: acquire the GIL, call the handler (returning a coroutine), pass the coroutine to `pyo3_async_runtimes::tokio::into_future` to convert it to a Rust future, await that future. No `spawn_blocking` — the coroutine awaits cooperatively, freeing the thread between awaits.
- Sync handlers continue to use the existing `spawn_blocking` path. The branch is per-handler at register time, so no per-request cost for the sync majority.
- `serve_client_stream` and `serve_duplex` get the same treatment — async handlers receive `AsyncRequestStream` / `AsyncResponseSink` wrappers instead of the sync flavors.

**Files touched.**
- `bindings/python/src/mesh_rpc.rs` — branch `PyRpcHandler::on_request` on handler-coroutine-ness; new async-context shims for the streaming server shapes.

### C-2 — Typed async server wrappers

**Design.**
- `AsyncTypedMeshRpc.serve(service, handler)` accepts `async def handler(req: Req) -> Resp`. Wraps the user's coroutine with codec encode/decode (`_json_encode` outbound, `_json_decode` inbound), passes the wrapped coroutine to the raw `serve` from C-1.
- `serve_client_stream` and `serve_duplex` similarly.

**Files touched.**
- `bindings/python/python/net/mesh_rpc.py` — extend `AsyncTypedMeshRpc` with server methods.

---

## Wave 4 — Tests + docs

### T-1 — pytest-asyncio round-trip suite

**Design.**
- New test file `bindings/python/tests/test_mesh_rpc_async.py` (sibling to the existing `test_mesh_rpc.py`).
- pytest-asyncio fixture builds a two-node mesh + handshakes them (reuses `test_mesh_rpc.py`'s `mesh_pair` fixture if exportable; otherwise duplicate).
- Four interop tests for every call shape (unary, server-streaming, client-streaming, duplex):
  - async caller × async server
  - async caller × sync server
  - sync caller × async server
  - sync caller × sync server (regression — the original case continues to work)
- Each test asserts the response shape; same fixture data as the existing sync tests.

**Files touched.**
- `bindings/python/tests/test_mesh_rpc_async.py` (new).
- `bindings/python/pyproject.toml` — add `pytest-asyncio` to dev deps if not already there.

### T-2 — Async cancellation pinning

**Design.**
- `asyncio.wait_for(async_rpc.call(...), timeout=0.1)` on a long-running call raises `asyncio.TimeoutError` (which under the hood cancels the inner task); the test asserts:
  - the call surfaces `asyncio.CancelledError` or `TimeoutError` to the caller,
  - the server's observer fires a `Cancelled` status event for the call,
  - no orphan registry entries linger on the substrate (assert via `cancel_registry().len() == 0` after the test settles — exposes via a debug-only test helper if needed).
- Mirror for streaming: `async for chunk in stream: ...` inside a `wait_for` that times out mid-stream → server observes `Cancelled`.

**Files touched.**
- `bindings/python/tests/test_mesh_rpc_async.py`.

### T-3 — Back-pressure under slow async consumer

**Design.**
- Server emits 10k chunks to a client whose `async for` body sleeps 1ms per chunk. Assert:
  - the call completes (no deadlock),
  - the substrate's per-call response-stream credit window throttles the producer (`ServiceMetrics.streaming_chunks_dropped_total == 0` since flow control should keep emissions bounded),
  - the consumer ultimately receives all 10k chunks.

**Files touched.**
- `bindings/python/tests/test_mesh_rpc_async.py`.

### D-1 — Docs

**Design.**
- New section in `bindings/python/README.md`: "Async API".
  - Quickstart with `AsyncMeshRpc` example (await unary + async-for streaming).
  - Mixing sync + async on the same `NetMesh` ("they share a runtime — pick whichever fits the caller").
  - Cancellation snippet (`asyncio.wait_for`).
  - Migration cookbook for users who built on the sync API and want to flip a service to async: "the typed wrapper is duck-typed, so an `async def` handler just works inside a sync test as long as the test runs the coroutine via `asyncio.run`."
- Add an async section to the Python module docstring (`net/__init__.py` and `mesh_rpc.py`).

**Files touched.**
- `bindings/python/README.md`.
- `bindings/python/python/net/__init__.py` — module docstring extension.

---

## Deferred follow-ups (post-this-plan)

1. **Async observers.** `set_observer(async_callable)` — the observer worker would drive the coroutine instead of calling under GIL. Wait for a real consumer who wants this (likely a FastAPI sidecar exporting metrics over async I/O).
2. **`trio` / `anyio` compatibility.** Right now `pyo3-async-runtimes` is asyncio-only. `anyio` could be supported with a second backend. Wait for a consumer ask — the JS / Go side use platform-native primitives, not a portable abstraction.
3. **Async circuit breaker + retry helpers.** The sync SDK has `RetryPolicy` / `CircuitBreaker` / `HedgePolicy` orchestration. Async equivalents — `AsyncRetryPolicy` etc. — are mostly mechanical mirrors but defer until the async caller-side is stable and users start asking for them.
4. **Async `Cancellable` ergonomics.** The current `Cancellable` class is a sync construct (`.cancel()` from another thread). An `AsyncCancellable` that integrates with `asyncio.Event` is possible, but the natural Python-async cancel idiom is `task.cancel()` — wait until there's a demonstrated need.
5. **Coroutine-handler timeout / deadline mapping.** When a sync handler exceeds its deadline, the substrate aborts the `spawn_blocking` task on its return. With async handlers, deadline expiration could fire `asyncio.CancelledError` inside the coroutine — but only if the handler's body cooperates with cancel points (`await`s on cancellable primitives). Document the caveat; consider an opt-in `@deadline_aware` decorator later.
6. **AsyncIO + `_net` C extension import-order pitfalls.** If a Python process imports `_net` after asyncio's event loop is already running on a different policy, the `init_with_runtime` call may race. A-1 should pin: `init_with_runtime` is called from the pymodule init, which happens before any user code runs. If a real reproducer surfaces, add a defensive `try_init` that's idempotent.

---

## Acceptance criteria

- `python -c "from net import AsyncMeshRpc, AsyncTypedMeshRpc"` succeeds on the published wheel.
- All four `(sync|async) caller × (sync|async) server` combinations pass round-trip in pytest-asyncio.
- `asyncio.wait_for(async_rpc.call(...), timeout=...)` propagates to a substrate cancel within 50ms (server's observer fires `Cancelled`).
- Wheel size delta < 200 KB compared to the v3-final wheel.
- No sync test regressions — `test_mesh_rpc.py` continues to pass unchanged.
