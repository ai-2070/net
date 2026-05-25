# Python SDK: side-by-side `async` + sync (whole-surface)

Branch: `python-async-sdk` (suggested).
Predecessor: [`NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md`](./NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md) — v3 promoted cancellation to a substrate primitive (`Mesh::reserve_cancel_token` / `Mesh::cancel(token)`). This plan reuses that primitive for asyncio cancellation propagation, plus the same pattern at every other module that today calls `runtime.block_on(...)` from a pyo3 method.

## Scope

Add an `Async` variant of **every Python SDK surface that performs I/O** — not just nRPC. Today the pyo3 binding spans 16 modules totaling ~16.8k LOC; ~141 method bodies wrap a `runtime.block_on(...)` to bridge from sync Python into async Rust. Each one is an opportunity to offer an `async def` sibling that returns a Python awaitable instead of blocking a thread.

The existing sync API is **unchanged**. Every method, class name, signature, and import stays put. Every existing `from net import ...` keeps working. The new surface adds parallel `Async`-prefixed classes; users mix freely on the same `NetMesh` instance.

### Surface audit (modules → block-on count → async-worthiness)

Counted via `grep -c "runtime.block_on\|py.detach\|py.allow_threads"` per pyo3 source file:

| Module                                     | block_on | Tier | Notes                                                              |
|--------------------------------------------|---------:|:----:|--------------------------------------------------------------------|
| `mesh_rpc.rs` (nRPC raw + typed wrapper)   |       32 | T1   | Already detailed below; foundational for service-typed callers.   |
| `deck.rs` (operator-side client + streams) |       21 | T3   | Snapshot / status streams are server-pushed — natural async-iter. |
| `cortex.rs` (NetDb / Redex / Memories / Tasks) | 18   | T2   | Watch iterators today fake-sync over async tails.                  |
| `compute.rs` (DaemonRuntime + handles)     |       14 | T2   | Daemon spawn / migrate / wait all block on RPC.                    |
| `lib.rs` (Net event bus + NetMesh + NetStream) | 12   | T1   | `NetMesh.connect` / handshake / `NetStream.send` are network I/O. |
| `aggregator.rs` + `capability_aggregation.rs` | 11    | T3   | `FoldQueryClient` / `RegistryClient` are RPC clients.              |
| `groups.rs` (Replica / Fork / Standby)     |       11 | T2   | Group spawn / health / migrate.                                    |
| `redis_dedup.rs`                           |        9 | T3   | Redis stream consumer (network I/O).                               |
| `meshos.rs` (daemon-author SDK)            |        6 | T3   | Daemon receive / publish_log / shutdown.                           |
| `blob.rs` (dataforts)                      |        5 | T2   | `blob_publish` / `blob_resolve` cross-mesh.                        |
| `meshdb.rs`                                |        2 | T3   | `MeshQueryRunner.execute` is RPC-backed.                           |
| `capabilities.rs`                          |        0 |  —   | Sync local helpers; no async value.                                |
| `capability_aggregation.rs`                |        0 |  —   | Sync helpers.                                                      |
| `identity.rs`                              |        0 |  —   | Sync crypto.                                                       |
| `placement.rs`                             |        0 |  —   | Sync helpers.                                                      |
| `subnets.rs`                               |        0 |  —   | Sync helpers.                                                      |

11 modules get async siblings; 5 stay sync-only (no I/O to await over).

## Why now

1. **The substrate is already async — every block_on is a thread-pool tax.** Each `runtime.block_on(...)` in the pyo3 binding takes a Python thread + a tokio worker hostage for the call's duration. In an asyncio-native consumer (FastAPI, LangGraph, an `aiohttp` sidecar, an `asyncio.gather` fan-out across 100 services), that pattern serializes what should be concurrent and burns the application's thread budget. Exposing the underlying futures lets `asyncio.gather` actually parallelize.
2. **The audience is bimodal.** Standard-library Python (scripts, CLI tools) is sync-natural; modern Python servers and agent frameworks are async-natural. The same library can serve both, but only if both shapes exist as first-class surfaces — not "sync only, run it in a `ThreadPoolExecutor` if you're async."
3. **Server-pushed streams are awkward as sync iterators.** `RedexTailIter`, `MemoryWatchIter`, `TaskWatchIter`, `SnapshotStream`, `StatusSummaryStream` are all server-pushed — the current sync iterator wraps an async `Stream` in a `runtime.block_on` per `__next__`. As async iterators they'd be the natural shape: one tokio future per `__anext__`, no blocking, no per-call thread block.
4. **v3 made cancellation tractable.** Before v3 each pyo3 method would have needed its own cancel adapter to propagate `asyncio.Task.cancel()` through. With v3's `Mesh::reserve_cancel_token` + `Mesh::cancel(token)`, the asyncio-cancel hook is the same shape as napi's `AbortSignal` listener — mint a token, on Python-task-cancel call `Mesh::cancel(token)`. One pattern, applied uniformly across every async method.
5. **Tiering ships value early.** T1 (nRPC + NetMesh + NetStream) is the foundation everything else builds on; users who only consume those see immediate value. T2 (cortex / compute / groups / blob) is the production-essentials block. T3 (deck / meshos / meshdb / aggregator / redis_dedup) is operator/specialty surface. Each tier is independently releasable.

## Locked decisions

1. **Naming: `AsyncFoo` parallel to `Foo`** for every sync class with async-worthy methods. Matches `httpx.Client` / `httpx.AsyncClient`, `sqlalchemy.Session` / `AsyncSession`. Rejected: `acall` / `async_method` on the existing class (clutters every class; forces users to learn both surfaces), `net.async_` import path (drives module-name drift). Module-level functions get an `async_` prefix when they have async I/O (`async_blob_publish`, etc.) — Python lacks first-class function pairing, so the prefix is the cleanest disambiguator.
2. **Async bridge: `pyo3-async-runtimes` (tokio backend).** Maintained successor to `pyo3-asyncio`; version-locks to pyo3 0.28 (current). One process-wide bridge initialized in the pymodule init.
3. **Shared tokio runtime across sync + async.** The `Arc<Runtime>` `NetMesh` constructs is the same runtime used for `block_on` (sync calls) and `future_into_py` (async calls). One process = one tokio runtime.
4. **Shared `MeshNode` across sync + async classes.** `AsyncMeshRpc(net_mesh)` and `MeshRpc(net_mesh)` reach the same `Arc<MeshNode>`; a server registered via `MeshRpc.serve(...)` is callable from `AsyncMeshRpc.call(...)` and vice versa. Same for every other adapter (NetDb, Redex, DaemonRuntime, etc.) — the async class is a different Python-side wrapper over the same Rust state.
5. **Cancellation: asyncio task cancel → substrate `Mesh::cancel(token)`** at every async I/O entrypoint. Implemented once as a shared helper (`async_with_cancel!` macro or a `fn await_with_cancel<F: Future<Output = Result<T, E>>>(py: Python, node: &MeshNode, fut: F) -> PyResult<...>` adapter) and applied uniformly. For non-mesh subsystems that don't have a CancelRegistry (e.g., redis_dedup), use a per-call `Arc<Notify>` that the Python-task-cancel closure trips.
6. **Streaming: PEP 525 async iterators** (`__aiter__` + `__anext__`) for every push-stream class. Server-pushed streams (`RedexTailIter`, `MemoryWatchIter`, `TaskWatchIter`, `SnapshotStream`, `StatusSummaryStream`) get parallel `AsyncRedexTailIter`, etc. The sync iterators stay; no class implements both protocols.
7. **Async server handlers / callbacks where applicable.** Modules that take user-supplied callables (`MeshRpc.serve` handlers, daemon factories, `MeshBlobAdapter` hooks, deck operator-policy verifiers, redis dedup callbacks) detect `inspect.iscoroutinefunction(fn)` at register time and route async fns through a coroutine-driving path; sync fns keep their `spawn_blocking` path. Branch is per-callback at register time, no per-invocation cost.
8. **Typed wrappers ride along.** Where a sync typed wrapper exists (today: only `mesh_rpc.py::TypedMeshRpc`), an `AsyncTypedFoo` parallel class lives in the same file. Future typed wrappers (MeshDB query DSL, etc.) get async siblings under the same convention.
9. **Single wheel, no feature flag.** `pyo3-async-runtimes` becomes a default dep on the `net` Cargo feature (everything else cascades). Wheel grows ~50 KB; not worth the install-path bifurcation.
10. **No public API change to existing classes.** No method moves from sync `Foo` to async `Foo` even if it would be philosophically cleaner — the goal is "side by side," and existing users seeing a method disappear is a regression. Cleanups land in a separate v0.x+1 plan if they're worth doing.

Tagged `[F | T1 | T2 | T3 | TX | D]`:

- **F** — foundations (pyo3-async-runtimes setup, cancel-bridge helper, async-iter adapter pattern, the migration template)
- **T1** — connection foundations (NetMesh, NetStream, mesh_rpc raw + typed) — everything else builds on these
- **T2** — production essentials (cortex, compute, groups, blob)
- **T3** — operator / specialty (deck, meshos, meshdb, aggregator, redis_dedup)
- **TX** — cross-cutting tests (sync × async interop matrix, observer back-pressure under async load, cancellation)
- **D** — docs (per-tier docstring updates + a top-level migration guide)

---

## Status

### Wave 0 — Foundations

| ID    | Pri | Area               | Title                                                                                                                |
|-------|-----|--------------------|----------------------------------------------------------------------------------------------------------------------|
| F-1   | H   | deps / module init | Add `pyo3-async-runtimes` dep; init `tokio::init_with_runtime(&runtime)` in the pymodule init (once per process)     |
| F-2   | H   | shared helper      | `await_with_cancel<F>` adapter that mints a substrate cancel-token, binds it to asyncio task cancellation, awaits F  |
| F-3   | H   | shared helper      | `AsyncIter` derive-style trait helper for streaming classes (`__aiter__` returns self, `__anext__` returns awaitable) |
| F-4   | M   | code-review        | Document the `block_on → future_into_py` migration template + checklist in `bindings/python/src/README.md`           |

### Wave T1 — Connection foundations

| ID     | Pri | Area              | Title                                                                                                              |
|--------|-----|-------------------|--------------------------------------------------------------------------------------------------------------------|
| T1-A1  | H   | pyo3 / NetMesh    | `AsyncNetMesh`: `connect` / `accept` / async streams enumeration; share the same `MeshNode` as `NetMesh`           |
| T1-A2  | H   | pyo3 / NetStream  | `AsyncNetStream`: awaitable `send`, async-iter `recv`; back-pressure-aware                                          |
| T1-A3  | H   | pyo3 / mesh_rpc   | `AsyncMeshRpc.call` / `call_service` / `find_service_nodes` (async unary + sync local lookup)                       |
| T1-A4  | H   | pyo3 / mesh_rpc   | `AsyncMeshRpc.call_streaming` → `AsyncRpcStream` with `__aiter__` / `__anext__`                                    |
| T1-A5  | H   | pyo3 / mesh_rpc   | `AsyncMeshRpc.call_client_stream` → `AsyncClientStreamCall` (awaitable `send` / `finish`)                          |
| T1-A6  | H   | pyo3 / mesh_rpc   | `AsyncMeshRpc.call_duplex` → `AsyncDuplexCall` / `AsyncDuplexSink` / `AsyncDuplexStream`                            |
| T1-A7  | H   | pyo3 / mesh_rpc   | `AsyncMeshRpc.serve` / `serve_client_stream` / `serve_duplex`: detect `async def` handlers; coroutine path         |
| T1-B1  | H   | typed wrapper     | `AsyncTypedMeshRpc.call` / `call_service` (codec + raw await) in `python/net/mesh_rpc.py`                          |
| T1-B2  | H   | typed wrapper     | `AsyncTypedMeshRpc.call_streaming` / `AsyncTypedRpcStream` (typed `async for`)                                     |
| T1-B3  | H   | typed wrapper     | `AsyncTypedMeshRpc.call_client_stream` / `AsyncTypedClientStreamCall`                                              |
| T1-B4  | H   | typed wrapper     | `AsyncTypedMeshRpc.call_duplex` / `AsyncTypedDuplexCall` / sink / stream halves                                    |
| T1-B5  | M   | typed wrapper     | `AsyncTypedMeshRpc.serve*` accept `async def` handlers; codec-wrap them                                            |

### Wave T2 — Production essentials

| ID     | Pri | Area               | Title                                                                                                              |
|--------|-----|--------------------|--------------------------------------------------------------------------------------------------------------------|
| T2-C1  | H   | pyo3 / cortex      | `AsyncNetDb`: async `get` / `put` / `delete` / `list` / `batch_put`                                                |
| T2-C2  | H   | pyo3 / cortex      | `AsyncRedex` + `AsyncRedexFile` + `AsyncRedexTailIter` (`async for evt in file.tail(from_seq)`)                    |
| T2-C3  | H   | pyo3 / cortex      | `AsyncMemoriesAdapter` + `AsyncMemoryWatchIter` (async `put` / `get` / `watch`)                                    |
| T2-C4  | H   | pyo3 / cortex      | `AsyncTasksAdapter` + `AsyncTaskWatchIter` (async `submit` / `claim` / `complete` / `watch`)                       |
| T2-D1  | H   | pyo3 / compute     | `AsyncDaemonRuntime`: async `register_kind` / `spawn` / `wait` / `migrate`                                          |
| T2-D2  | H   | pyo3 / compute     | `AsyncDaemonHandle` + `AsyncMigrationHandle` (await on lifecycle events)                                            |
| T2-E1  | M   | pyo3 / groups      | `AsyncReplicaGroup` / `AsyncForkGroup` / `AsyncStandbyGroup`: async `spawn` / `health` / `migrate` / `await_member` |
| T2-F1  | H   | pyo3 / blob        | `AsyncMeshBlobAdapter` + module-level `async_blob_publish` / `async_blob_resolve`                                  |
| T2-F2  | M   | pyo3 / blob        | Async hook support in `register_blob_adapter` (detect `async def` adapter hooks)                                    |

### Wave T3 — Operator / specialty

| ID     | Pri | Area                | Title                                                                                                              |
|--------|-----|---------------------|--------------------------------------------------------------------------------------------------------------------|
| T3-G1  | M   | pyo3 / deck         | `AsyncDeckClient`: async `connect` / `admin` / `snapshot` / `status` / `audit` / `logs`                            |
| T3-G2  | M   | pyo3 / deck         | `AsyncSnapshotStream` + `AsyncStatusSummaryStream` (async-iter the server-pushed streams)                          |
| T3-G3  | L   | pyo3 / deck         | `AsyncAdminCommands` / `AsyncIceCommands` (async admin-action commits)                                              |
| T3-H1  | M   | pyo3 / meshos       | `AsyncMeshOsDaemonSdk` + `AsyncMeshOsDaemonHandle` (async `receive` / `publish_log` / `graceful_shutdown`)         |
| T3-I1  | M   | pyo3 / meshdb       | `AsyncMeshQueryRunner.execute` returning an awaitable; query AST classes stay sync (pure data)                     |
| T3-J1  | M   | pyo3 / aggregator   | `AsyncFoldQueryClient` (async `query` + async-iter `subscribe`); `AsyncRegistryClient`                              |
| T3-K1  | L   | pyo3 / redis_dedup  | `AsyncRedisStreamDedup` (async `consume` / `ack` / `next`)                                                          |

### Wave TX — Cross-cutting tests

| ID     | Pri | Area               | Title                                                                                                                |
|--------|-----|--------------------|----------------------------------------------------------------------------------------------------------------------|
| TX-1   | H   | tests              | `(sync\|async) caller × (sync\|async) server` matrix on every shape (unary, streaming, client-stream, duplex)        |
| TX-2   | H   | tests              | asyncio cancel propagation: `asyncio.wait_for(..., timeout)` → substrate cancel; orphan registry stays at 0          |
| TX-3   | M   | tests              | Async observer back-pressure: slow async callable doesn't starve substrate; v3 drop-counter increments under load     |
| TX-4   | M   | tests              | Async streaming back-pressure: slow async consumer doesn't starve producer; flow-control keeps drops at 0            |
| TX-5   | M   | tests              | Per-module round-trip: every T2/T3 async class has a smoke test pinning the basic happy path                          |

### Wave D — Docs

| ID     | Pri | Area               | Title                                                                                                                |
|--------|-----|--------------------|----------------------------------------------------------------------------------------------------------------------|
| D-1    | M   | docs               | `bindings/python/README.md`: top-level "Async API" section, migration cookbook, mixing-sync-and-async patterns        |
| D-2    | M   | docs               | Per-module docstring updates (each `Async*` class cross-references its sync sibling; sync sibling notes the async alt)|
| D-3    | L   | docs               | Type stubs: `_net.pyi` extended with every new `Async*` class so users get IDE completion                            |

**ABI / wheel impact.** No substrate change. No rpc-ffi ABI bump (still 0x0004 per v3). Wheel grows ~50 KB from `pyo3-async-runtimes`. The pyo3 module gains ~25 new `Async*` exported classes; nothing existing is renamed or removed.

---

## Phasing

**Release strategy:** each tier ships as a separate `net-mesh` PyPI release.

1. **Release v0.x — Foundations + T1.** Ship F-1..F-4 + T1-A1..T1-B5 in one cut. After this, asyncio consumers can build entire applications on `AsyncNetMesh` + `AsyncMeshRpc` / `AsyncTypedMeshRpc` without ever touching the sync API. This is the load-bearing release.
2. **Release v0.x+1 — T2.** Production essentials: cortex (NetDb/Redex/Memories/Tasks), compute (DaemonRuntime), groups, blob. Adds async sibling for every persistent-state and daemon-supervision surface.
3. **Release v0.x+2 — T3.** Operator + specialty: deck, meshos, meshdb, aggregator, redis_dedup. Wraps up the SDK surface.
4. **Release v0.x+3 — TX + D polish.** Cross-cutting test matrix lands incrementally during T1/T2/T3 (each tier ships with its own happy-path tests via TX-5); TX-1..TX-4 land alongside D-1..D-3 as the surface stabilizes.

Within each tier, slices are independent — they can land in any order. Mesh-RPC slices (T1-A3..T1-A7) have a natural sequencing (raw before typed) but no other hard dependencies.

---

## Wave 0 — Foundations

### F-1 — `pyo3-async-runtimes` dep + module bridge

**Design.**
- Add `pyo3-async-runtimes = { version = "0.28", features = ["tokio-runtime"] }` to `bindings/python/Cargo.toml`. Version-locked to pyo3 0.28.
- In `bindings/python/src/lib.rs::_net` pymodule init, before any other setup: call `pyo3_async_runtimes::tokio::init_with_runtime(&runtime)` under a `OnceLock` guard. The runtime is the shared `Arc<Runtime>` `PyNetMesh` already owns.
- Document the global-bridge invariant in the lib.rs module docstring: "any consumer constructing their own NetMesh shares one tokio runtime with the async surface."

**Files touched.** `bindings/python/Cargo.toml`, `bindings/python/src/lib.rs`.

### F-2 — `await_with_cancel` shared helper

**Design.**
- New helper in a new module `bindings/python/src/async_bridge.rs`:
  ```rust
  pub fn await_with_cancel<F, T, E>(
      py: Python<'_>,
      node: &Arc<MeshNode>,
      build_fut: impl FnOnce(Option<u64>) -> F,
  ) -> PyResult<Bound<'_, PyAny>>
  where
      F: Future<Output = Result<T, E>> + Send + 'static,
      T: IntoPyObject + Send + 'static,
      E: Into<PyErr> + Send + 'static,
  ```
  Mints `cancel_token = node.reserve_cancel_token()`, builds the call's future via the caller-supplied closure (which populates `opts.cancel_token = Some(token)`), wraps in a `tokio::select!` between the future and an asyncio-cancel-signal channel, returns a Python awaitable.
- For subsystems without a `MeshNode` cancel registry (redis_dedup, blob-adapter hooks), parallel helper `await_with_notify` uses an `Arc<Notify>` instead.
- The asyncio-cancel signal comes from `pyo3-async-runtimes`' `tokio::future_into_py_with_locals` cancellation hook (verify exact API surface — if not directly supported, register a custom callback at coroutine construction time that trips a `tokio_util::sync::CancellationToken`).

**Files touched.** `bindings/python/src/async_bridge.rs` (new), `bindings/python/src/lib.rs` (mod declaration).

### F-3 — Async-iter trait helper

**Design.**
- Many streaming classes need the same `__aiter__` (returns self) + `__anext__` (awaitable yielding the next item) shape. Factor into a documented pattern + an optional helper macro.
- Pattern:
  ```rust
  #[pyclass(name = "AsyncFooIter")]
  pub struct PyAsyncFooIter { inner: Arc<Mutex<Option<InnerStream>>> }

  #[pymethods]
  impl PyAsyncFooIter {
      fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> { slf }
      fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
          let inner = self.inner.clone();
          pyo3_async_runtimes::tokio::future_into_py(py, async move {
              let mut guard = inner.lock();
              let Some(stream) = guard.as_mut() else {
                  return Err(PyStopAsyncIteration::new_err(()));
              };
              match stream.next().await {
                  Some(item) => Ok(item),
                  None => Err(PyStopAsyncIteration::new_err(())),
              }
          })
      }
  }
  ```
- Document in `bindings/python/src/README.md` so every wave-T2/T3 slice applies the same shape.

**Files touched.** `bindings/python/src/README.md` (new pattern doc — code is per-class, no shared module needed beyond the convention).

### F-4 — Migration template + checklist

**Design.**
- One-page doc in `bindings/python/src/README.md`:
  - Before: `fn foo(&self, py: Python<'_>, ...) -> PyResult<...>` with `runtime.block_on(...)` inside `py.detach`.
  - After: `fn foo<'py>(&self, py: Python<'py>, ...) -> PyResult<Bound<'py, PyAny>>` calling `await_with_cancel`.
  - Error mapping reuses the existing `rpc_error_to_pyerr` / `cortex_error_to_pyerr` / etc. helpers — async path uses the same mappers, no duplication.
- Checklist for code review:
  - [ ] `Async*` class name matches the `Foo` it parallels.
  - [ ] Constructor accepts the same arguments as the sync sibling (so `NetMesh` instances can be passed to either).
  - [ ] Every awaitable method uses `await_with_cancel` (no raw `future_into_py`) — cancellation is non-negotiable.
  - [ ] Streaming class has `__aiter__` + `__anext__`, not `__iter__` + `__next__`.
  - [ ] Sync sibling docstring cross-references the async class; async docstring cross-references the sync class.

**Files touched.** `bindings/python/src/README.md`.

---

## Wave T1 — Connection foundations

### T1-A1 / T1-A2 — `AsyncNetMesh` + `AsyncNetStream`

**Design.**
- `AsyncNetMesh(keypair, [config])` — same constructor signature as `NetMesh`.
- Async methods: `connect(target)`, `accept_stream(...)`, anything else that today does `runtime.block_on` inside the sync `NetMesh`.
- `AsyncNetStream`: `async def send(payload)`, `async def recv()` (or `__anext__` if the underlying surface is stream-shaped). Stats accessor stays sync (it's a DashMap read).
- Sync `NetMesh.streams()` / `connect()` etc. stay untouched.

**Files touched.** `bindings/python/src/lib.rs` (the NetMesh / NetStream pyclass section — split into a `mesh.rs` if the file gets too large).

### T1-A3..T1-A7 — `AsyncMeshRpc` raw client + server

**Design.** As detailed in the original draft of this plan (Wave 1 sections, pre-revision):
- `AsyncMeshRpc.call` / `call_service` / `find_service_nodes` (T1-A3).
- `AsyncMeshRpc.call_streaming` → `AsyncRpcStream` (T1-A4).
- `AsyncMeshRpc.call_client_stream` → `AsyncClientStreamCall` (T1-A5).
- `AsyncMeshRpc.call_duplex` → `AsyncDuplexCall` + halves (T1-A6).
- `AsyncMeshRpc.serve` / `serve_client_stream` / `serve_duplex` with `inspect.iscoroutinefunction` detection (T1-A7).

Every awaitable method routes through `await_with_cancel` from F-2.

**Files touched.** `bindings/python/src/mesh_rpc.rs`.

### T1-B1..T1-B5 — `AsyncTypedMeshRpc` typed wrapper

**Design.** Pure Python in `python/net/mesh_rpc.py`. Parallel `Async`-prefixed classes for every existing sync typed class: `AsyncTypedMeshRpc`, `AsyncTypedRpcStream`, `AsyncTypedClientStreamCall`, `AsyncTypedDuplexCall`, `AsyncTypedDuplexSink`, `AsyncTypedDuplexStream`, `AsyncTypedRequestStream`, `AsyncTypedResponseSink`. Each encodes / decodes JSON the same way the sync wrapper does; the only difference is `await self._raw.foo(...)` vs `self._raw.foo(...)`.

**Files touched.** `bindings/python/python/net/mesh_rpc.py` (extend the existing file), `bindings/python/python/net/__init__.py` (re-exports).

---

## Wave T2 — Production essentials

For each module: per-class slice list + same-shape rules (mint cancel-token, return awaitable, async-iter for push streams). Detailed slice-level designs land in per-tier follow-up plan docs as the work begins; the structure here is the contract.

### T2-C1..T2-C4 — `cortex.rs` (NetDb / Redex / Memories / Tasks)

**Per-class async surface:**

- `AsyncNetDb`: async `get` / `put` / `delete` / `list` / `batch_put`. Sync iterators (`list`) become async iterators.
- `AsyncRedex`: async `open` / `append` (`AsyncRedexFile.append`), tail returns `AsyncRedexTailIter`.
- `AsyncMemoriesAdapter`: async `put` / `get` / `delete` / `list`; `watch` returns `AsyncMemoryWatchIter`.
- `AsyncTasksAdapter`: async `submit` / `claim` / `complete` / `fail` / `list`; `watch` returns `AsyncTaskWatchIter`.
- All four watch / tail iterators implement `__aiter__` + `__anext__` per F-3 pattern.

**Files touched.** `bindings/python/src/cortex.rs`.

### T2-D1..T2-D2 — `compute.rs` (DaemonRuntime + handles)

**Per-class async surface:**

- `AsyncDaemonRuntime`: async `register_kind` (probably sync — registration is local), async `spawn` (RPC under hood), async `migrate`, async `list_daemons`.
- `AsyncDaemonHandle`: async `wait`, async `migrate`, async `terminate`, async-iter `events` (causal event stream).
- `AsyncMigrationHandle`: async `wait`, async `cancel`.
- `CausalEvent` stays sync (pure data class).

**Files touched.** `bindings/python/src/compute.rs`.

### T2-E1 — `groups.rs` (Replica / Fork / Standby)

**Per-class async surface:**

- `AsyncReplicaGroup` / `AsyncForkGroup` / `AsyncStandbyGroup`:
  - async `spawn` (constructor — returns awaitable yielding the group handle).
  - async `health`, `await_member`, `migrate`, `terminate`.
  - Group iteration (`__iter__` over members) stays sync (snapshot read).

**Files touched.** `bindings/python/src/groups.rs`.

### T2-F1..T2-F2 — `blob.rs` (dataforts)

**Per-class async surface:**

- `AsyncMeshBlobAdapter`: async `publish` / `resolve` / `delete`.
- Module-level: `async_blob_publish(ref, data)`, `async_blob_resolve(ref)`. Sync `blob_publish` / `blob_resolve` stay.
- `register_blob_adapter` accepts adapter objects whose hooks are `async def` — wraps with `pyo3_async_runtimes::tokio::into_future` per call.

**Files touched.** `bindings/python/src/blob.rs`.

---

## Wave T3 — Operator / specialty

Same shape as T2 — full slice-level design lives in per-tier follow-up plan docs. Per-module class lists:

- **T3-G1..T3-G3 (deck):** `AsyncDeckClient`, `AsyncSnapshotStream`, `AsyncStatusSummaryStream`, `AsyncAdminCommands`, `AsyncIceCommands`. Operator-policy verifier hooks (`AdminVerifier` callbacks) accept `async def` verifiers.
- **T3-H1 (meshos):** `AsyncMeshOsDaemonSdk`, `AsyncMeshOsDaemonHandle`. Daemon-author surfaces (`receive`, `publish_log`, `graceful_shutdown`) become awaitable.
- **T3-I1 (meshdb):** `AsyncMeshQueryRunner.execute` returns an awaitable yielding `ResultRow` / `AggregateResult` / `JoinedRow` per shape. Query AST classes (`Predicate`, `MeshQuery`, `QueryBuilder`, `WindowBoundary`, `GroupKey`, `LineageEntry`, `ExecuteOptions`, `CachePolicy`) stay sync — they're pure data builders.
- **T3-J1 (aggregator):** `AsyncFoldQueryClient` (async `query`, async-iter `subscribe`), `AsyncRegistryClient` (async `list` / `spawn` / `scale` / `unregister`).
- **T3-K1 (redis_dedup):** `AsyncRedisStreamDedup` — Redis is network I/O; the sync surface today blocks per-call. Async-iter `next()` over the inbound stream.

---

## Wave TX — Cross-cutting tests

### TX-1 — `(sync | async) caller × (sync | async) server` matrix

**Design.** New `bindings/python/tests/test_async_interop.py`. Per call shape (unary, server-streaming, client-streaming, duplex), four tests:
- async caller × async server
- async caller × sync server
- sync caller × async server
- sync caller × sync server (regression — the original sync API continues to work)

Two-node mesh fixture; same fixture data as the existing sync `test_mesh_rpc.py`.

### TX-2 — asyncio cancellation propagation

**Design.** `asyncio.wait_for(async_rpc.call(...), timeout=0.1)` on a long-running call asserts: caller surfaces `asyncio.TimeoutError`, server's observer fires `Cancelled`, substrate `CancelRegistry::len() == 0` after settling. Mirrors for streaming shapes (cancel mid-`async for`).

### TX-3 — Async observer back-pressure

**Design.** A `set_observer(async_callable)` whose callable sleeps 100ms per event; fire 2000 events; assert the substrate's bounded-mpsc drops most (per v3 O-A1..O-A3) and the snapshot's `observer_dropped_total` increments correctly. Pins that async callables don't accidentally bypass the v3 back-pressure machinery.

### TX-4 — Async streaming back-pressure

**Design.** Server emits 10k typed chunks; client's `async for` body sleeps 1ms per chunk. Assert: complete, no drops, flow-control credits stay bounded.

### TX-5 — Per-module smoke tests

**Design.** One happy-path round-trip test per T2/T3 async class lands alongside its slice. Format: `test_async_<module>.py` per module. These are not exhaustive — they pin the surface exists and the basic path works; full coverage is a follow-up.

---

## Wave D — Docs

### D-1 — `bindings/python/README.md` async section

**Design.** New top-level section. Cover:
- Quickstart: an `AsyncNetMesh` + `AsyncMeshRpc` example with `await rpc.call(...)`.
- Async-for streaming snippet (`async for chunk in stream: ...`).
- Mixing sync + async on one `NetMesh` ("they share a runtime — pick whichever fits the caller; a `MeshRpc` server can be called from `AsyncMeshRpc.call` and vice versa").
- Cancellation: `asyncio.wait_for(...)` / `asyncio.Task.cancel()` integrate transparently.
- Migration cookbook for users on the sync API who want to move a service to async.

### D-2 — Per-module docstring updates

**Design.** Every `Async*` class docstring cross-references its sync sibling and notes the contract ("same I/O semantics; same MeshNode; awaitable instead of blocking"). The sync sibling's docstring gains a "Async equivalent: `AsyncFoo`" line.

### D-3 — Type stubs

**Design.** Extend `bindings/python/python/net/_net.pyi` with every new `Async*` class. IDE completion is the deliverable; runtime behavior is independent of stubs.

---

## Deferred follow-ups (post-this-plan)

1. **`trio` / `anyio` compatibility.** Only asyncio supported initially (via `pyo3-async-runtimes::tokio`). `anyio` could be a second backend if a real consumer asks.
2. **Async-native typed wrappers for MeshDB / aggregator.** This plan exposes the raw async surface; typed Python wrappers around the query DSL or aggregator client are a follow-up.
3. **Async `Cancellable` ergonomics.** Today's `Cancellable` is a sync construct. An `AsyncCancellable` that integrates with `asyncio.Event` is possible but the canonical async-cancel idiom is `task.cancel()` — wait for a consumer ask.
4. **`async with` everywhere.** Many Async* classes will gain `__aenter__` / `__aexit__` for `async with` ergonomics. Where the sync sibling already supports `__enter__` / `__exit__`, mirror; otherwise add only on consumer request.
5. **Async-native circuit breaker / retry / hedge orchestration.** Sync SDK has `RetryPolicy`, `CircuitBreaker`, `HedgePolicy`. Async equivalents are mostly mechanical mirrors.
6. **AsyncIO event-loop policy edge cases.** If a Python process embeds `_net` via a non-default asyncio loop policy (uvloop, custom), confirm `pyo3-async-runtimes` plays nicely. Likely fine; defensive: add an integration test under uvloop.

---

## Acceptance criteria

- `python -c "from net import AsyncMeshRpc, AsyncTypedMeshRpc, AsyncNetMesh, AsyncNetDb, AsyncDaemonRuntime, AsyncReplicaGroup, AsyncMeshBlobAdapter"` succeeds on the v0.x+2 published wheel.
- TX-1 matrix passes — all four (sync|async)² combinations work on every call shape.
- `asyncio.wait_for(...)` cancellation propagates to substrate within 50ms across every async method (TX-2).
- Wheel size delta < 200 KB compared to the v3-final wheel.
- Zero sync test regressions — `test_mesh_rpc.py` and all existing module tests continue to pass unchanged.
- Per-module type stubs land in `_net.pyi` so `mypy --strict` consumers see every new `Async*` class.
