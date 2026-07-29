# Code review — Python async SDK branch (2026-05-24)

Branch base: `master`.
Scope: 36 commits, ~7,139 LOC added / 5 removed, entirely under
`net/crates/net/bindings/python/`. Builds the side-by-side `Async*`
PyO3 surface mirroring every existing sync class — `AsyncMeshRpc` +
`AsyncTypedMeshRpc` (all 5 call shapes + serve), `AsyncMemoriesAdapter`
/ `AsyncTasksAdapter` / `AsyncRedexFile` + watch iters,
`AsyncDaemonRuntime` + `AsyncMigrationHandle`, `AsyncMeshBlobAdapter`,
`AsyncRegistryClient` + `AsyncFoldQueryClient`, `AsyncMeshQueryRunner`,
`AsyncSnapshotStream` + `AsyncStatusSummaryStream`, `AsyncDeckClient` +
`AsyncAdminCommands` + `AsyncIceCommands`, `AsyncMeshOsDaemonSdk` +
`AsyncMeshOsDaemonHandle`. Plus `src/async_bridge.rs` (~185 LOC) and
async-def `serve` handler detection.

Separate branch from `subnet-scaling` and `nrpc-sdks`; prior review
docs don't overlap.

Three review agents (reuse / quality / efficiency) were dispatched in
parallel. Findings below are organised by severity, then category.
File paths are relative to repo root; line numbers reflect the branch
tip and may drift.

---

## HIGH — correctness (cancel-propagation contract)

### P1 — Three watch iterators don't propagate asyncio cancel to the substrate

`bindings/python/src/cortex.rs:1099-1141` (`AsyncRedexTailIter`),
`:614-657` (`AsyncMemoryWatchIter`), and the `AsyncTaskWatchIter`
equivalent.

All three call `pyo3_async_runtimes::tokio::future_into_py(py, async
move { ... })` directly, bypassing the `await_with_existing_token`
helper. Result: `asyncio.wait_for(iter.__anext__(), timeout=1)` drops
only the spawned tokio task; the substrate-side stream pull is *not*
cancelled, and the iterator leaks the substrate stream until the user
remembers `aclose()`.

`PyAsyncRpcStream` at `mesh_rpc.rs:2793` got this right (uses
`await_with_existing_token`); the three watch iters did not. This is
inconsistent with the cancel-propagation contract the README promises
at `README.md:177`.

Fix: thread a construction-time cancel token through each watch iter
and call `await_with_existing_token`, OR install a `CancelGuard`-style
drop that calls `inner.shutdown.notify_waiters()` when the bridge
future is dropped.

### P2 — Six substrate-call binding files bypass `await_with_cancel` entirely

`bindings/python/src/aggregator.rs`, `blob.rs`, `compute.rs`,
`cortex.rs`, `deck.rs`, `meshos.rs` — ~62 raw `future_into_py` sites;
only `mesh_rpc.rs` uses the `async_bridge::await_with_cancel` helper.

The whole point of `async_bridge::await_with_cancel` (lines 103-130)
and the `CancelGuard` RAII is asyncio task-cancel → substrate
`MeshNode::cancel(token)` propagation. Same shape as P1:
`asyncio.wait_for(deck.admin().drain(...), timeout=2)` cancels the
Python task but the substrate call keeps running.

Fix: route every Async substrate-call method (admin / snapshot / blob
/ cortex tail / registry / foldquery) through `await_with_cancel`. For
pure-local lookups that genuinely don't need cancel propagation (e.g.
`PyAsyncFoldQueryClient::invalidate_cache`), document the exemption
inline.

---

## HIGH — per-call performance

### P3 — Async reply path does 2× the allocations + memcpys of sync

`bindings/python/src/mesh_rpc.rs:2610, 2636, 2920` (unary), plus
per-chunk at `:2810, 3038, 3206` (streaming).

**Sync path:** `PyBytes::new(py, result.body.as_ref())` is one memcpy
from `Bytes` straight into the Python bytes object.

**Async path:** `.map(|reply| reply.body.to_vec())` heap-allocates a
`Vec<u8>`, memcpys into it, then `IntoPyObject` allocates a `PyBytes`
and memcpys *again*. So every async reply is +1 heap alloc + 1 extra
memcpy of the body vs sync. On a streaming call yielding N chunks,
that's N extra allocs/memcpys.

Fix: in the `await_with_cancel` / `await_with_existing_token`
resolution, re-acquire the GIL and produce a `Py<PyBytes>` (or use a
wrapper holding `bytes::Bytes` that implements `IntoPyObject` as
`PyBytes::new(py, b.as_ref())`). The bridge already needs the GIL at
resolve time to hand the value to Python — do the bytes copy there.

### P4 — Streaming `__anext__` thrashes `Arc<Mutex<Option<...>>>` per chunk

`bindings/python/src/mesh_rpc.rs:2793-2823` (`PyAsyncRpcStream`),
`:2882-2906` (client-stream `send`), all duplex methods.

Each `__anext__` does `inner.lock().take()` → await → `*inner.lock() =
Some(stream)`. Two `parking_lot` acquires + an `Option` move + an
`Arc<Mutex<...>>` clone per chunk. Sound (justified by the "can't hold
MutexGuard across await" comment) but expensive at high chunk rates —
a 100k-chunk stream pays 200k mutex ops on a contention-free Mutex
purely to satisfy borrow-checker scoping.

Fix: replace `parking_lot::Mutex<Option<InnerRpcStream>>` with
`tokio::sync::Mutex<Option<InnerRpcStream>>` so the guard *can* be
held across await — one acquire per pull instead of two, no take/put.
`AsyncRedexTailIter` already uses `TokioMutex` at `cortex.rs:1071`;
apply the same to the mesh_rpc stream / sink classes.

---

## MEDIUM — duplication consolidation

### P5 — Handler-bridge duplication: 3 nearly identical 80-line bodies

`bindings/python/src/mesh_rpc.rs:605-873`. `PyAsyncRpcHandler`,
`PyAsyncRpcClientStreamingHandler`, `PyAsyncRpcDuplexHandler` all
follow `Python::attach → into_future → tokio::time::timeout → match
outcome`. `extract_app_error` invoked 10×; closing `match py_result`
arm appears 3× verbatim. The Internal-vs-Application mapping is
hand-copied; only the diagnostic noun ("async handler" / "async
client-streaming handler" / "async duplex handler") differs.

Fix: `async fn drive_py_coroutine(callable, timeout, args_builder,
label) -> Result<HandlerOutcome, RpcHandlerError>` returning the
discriminated outcome. Each `RpcHandler` impl becomes ~15 lines around
the call. The sync siblings (`PyRpcClientStreamingHandler` at
`:1632-1798`) can fold into one `spawn_blocking_py(callable,
args_builder, label)` helper the same way.

### P6 — `HandlerOutcome → RpcResponsePayload` mapping duplicated sync/async

`bindings/python/src/mesh_rpc.rs:561-584` (sync) vs `:663-689` (async).

Both translate `HandlerOutcome::Ok / AppError / Err(String)` into the
identical 3-arm match producing `RpcResponsePayload` +
`RpcHandlerError::{Application,Internal}`. ~25 lines copied.

Fix: `fn finalize_handler_outcome(outcome: Result<HandlerOutcome,
String>, timeout: Duration) -> Result<RpcResponsePayload,
RpcHandlerError>` called from both impls. Same applies to the
streaming-handler pairs and the duplex pair.

### P7 — Five-arm `serve*` timeout/handler-kind branch hand-duplicated 6 times

`bindings/python/src/mesh_rpc.rs:2449-2585` (Async serve / serve_client_stream
/ serve_duplex) and `:2033, 2266, 2297` (sync equivalents).

Outer `Some(0) / Some(ms) / None` timeout parsing repeated 6× per
file. Each method has the same `if is_coroutine_function(py,
&handler) { ... } else { ... }` skeleton with two parallel
`self.node.serve_rpc*(...)` calls. The inner asymmetry (which handler
struct gets boxed) is genuine — `serve_rpc` is monomorphic so the two
arms can't unify into `Arc<dyn RpcHandler>` — but the outer timeout
parsing is trivially extractable.

Fix: hoist `fn resolve_handler_timeout(handler_timeout_ms: Option<u64>)
-> Duration` once. Saves ~50 lines, removes one class of off-by-one
bugs.

### P8 — Per-shape `aclose` boilerplate × 5

`bindings/python/src/mesh_rpc.rs:2850, 2943, 3100, 3173, 3229`.

Same `self.close(); future_into_py(py, async {Ok(())})` 5×.

Fix: `impl_aclose!()` macro or a default-implemented trait method.

### P9 — `PyAsyncAdminCommands` is 8 copy-paste `future_into_py` admin wrappers

`bindings/python/src/deck.rs:1944-2057`.

Each method is structurally identical except for the
`admin().<verb>(args)` call: `let client = self.client.clone();
future_into_py(py, async move { let commit =
client.admin().<verb>(...).await.map_err(...)?;
Ok::<AsyncChainCommitWrap, PyErr>(AsyncChainCommitWrap(commit)) })`.

Fix: small `async fn await_admin_commit<F, Fut>(py, client, build: F)`
helper or `admin_method!` macro. Same shape on the sync side
(`deck.rs:510-535`).

### P10 — `mesh_rpc.py` decode-wrapper triplicate

`bindings/python/python/net/mesh_rpc.py:859-869` (`TypedMeshRpc.serve`)
and `:1294-1320` (`AsyncTypedMeshRpc.serve` — sync + async branches).

Same `try/except RpcCodecError → RpcAppError(NRPC_TYPED_BAD_REQUEST,
json.dumps(...))` shim copied 3 times.

Fix: `_decode_request_or_app_error(req_bytes: bytes) -> Any` returning
the decoded payload or raising the canonical `RpcAppError`. Same
applies to `serve_client_stream` / `serve_duplex` wrappers (lines
`961-964, 990-996` vs async equivalents).

---

## LOW — test / docs / minor

### P11 — Test gap: streaming async-iter cancel is `pytest.skip`'d

`bindings/python/tests/test_async_interop.py:271-310`.

`test_streaming_mid_iter_cancel_terminates_stream` is wholly skipped
(`L305: pytest.skip("streaming-server-side cancel test requires a
Python-side serve_streaming handler API")`). The 4-cell sync×async
matrix at `L103-188` is **unary-only** — 4 cells × 1 shape, not 4
cells × 4 shapes as TX-1 implies. The README claims streaming async-for
cancel works (`L177`); no test pins it.

Fix: either land the streaming round-trip test against
`MeshRpc.call_streaming` server-side, or rename TX-1 in the docstring
to "unary matrix" and file the streaming cell as a tracked task. P1's
fix should unblock this test.

### P12 — Test fixture `_mesh_pair` / `_next_port` / `PSK` is the 4th copy

`bindings/python/tests/test_async_interop.py:39-78`.

Already duplicated across `test_compute.py`, `test_groups.py`,
`test_capability_aggregation_e2e.py`; this branch makes it 4.

Fix: `tests/conftest.py` exposing `mesh_pair` / `next_port` as pytest
fixtures; delete per-file copies. Pre-existing technical debt this
branch widened.

### P13 — Each test spins up a fresh daemon

`bindings/python/tests/test_async_interop.py`,
`test_async_per_module.py`.

Each of ~10+ tests does a full TCP handshake + `start()` + `shutdown()`
cycle, with hardcoded `time.sleep(0.05)` handshake wait. CI flake
source.

Fix: module-scope `_mesh_pair` fixture; per-test register a unique
service name to avoid cross-test interference. Smoke tests can share
a single `Redex` per-class with per-test ORIGIN namespacing.

### P14 — `_parse_status_from_message` silently disables retries on parse miss

`bindings/python/python/net/mesh_rpc.py:1115, 1181`.

`_STATUS_PATTERN = "status\\s*=?\\s*0x..."` tolerates both `status=0x…`
and `status 0x…`. Tolerating multiple forms means the *Rust formatter
is the authoritative spec*. `default_retryable` drives retry policy off
this parse; a formatter typo silently disables retries for
`RpcServerError`.

Rust-side unit test already exists at `mesh_rpc.rs:3259-3361` —
good. Fix: have the Python `_parse_status_from_message` raise instead
of returning `None` for an `RpcServerError` whose message looks like
nrpc but doesn't carry a status. Currently silent fall-through to
`return False`.

### P15 — `Bytes::copy_from_slice(request.as_bytes())` on every send

9 sites in `bindings/python/src/mesh_rpc.rs` (1053, 1165, 1337, 1624,
2075, 2113, 2153, 2603, 2629, 2745, 2888, 2981, 3125).

Pre-existing pattern (sync has it too), so not a regression. Zero-copy
possible via `bytes::Bytes::from_owner(Py<PyBytes>)` if substrate
accepted `impl Into<Bytes>`. Substrate-side change; defer.

### P16 — "Sync equivalent: …" lines repeated 30+ times in docstrings

Every method on `PyAsyncMeshRpc` ends with a `Sync equivalent: …`
line (`mesh_rpc.rs:2592, 2619, 2654, 2691, 2734` and 25+ more).
Mirrored on the typed wrapper (`mesh_rpc.py:1335, 1350, 1359, 1377,
1395, 1415, 1430, 1490, 1548`).

Already in the README's class table (`README.md:111-138`). Drop the
per-method docstring repetitions.

### P17 — `AsyncMeshOsDaemonHandle::{next_control, publish_log, publish_capabilities}` repeats `try_lock + sdk_err` prelude × 3

`bindings/python/src/meshos.rs:1175-1254`.

4-line `try_lock + sdk_err("busy" / "already_shutdown")` prelude
copied 3 times. Minor; a `require_active_handle` helper would tidy but
isn't load-bearing.

---

## False positives noted during the pass

- **`async_bridge.rs` is the right shape.** Consumes
  `pyo3-async-runtimes::tokio::{future_into_py, init_with_runtime}`
  (already in Cargo deps) and adds only the cancel-token glue +
  `CancelGuard` RAII, which has no upstream equivalent. Process-global
  `OnceLock<Runtime>` correctly replaces ad-hoc per-call-site
  runtimes.
- **Tokio runtime is process-global.** `src/async_bridge.rs:37,
  49-65` — single `OnceLock<Runtime>`, multi-thread, plumbed into
  `pyo3_async_runtimes::tokio::init_with_runtime` exactly once from
  `lib.rs:2767`. Not per-call. Correct.
- **GIL across `.await` is clean.** Every `Python::attach(...)` block
  (e.g. `mesh_rpc.rs:614, 621, 651, 705`) closes before the `.await`,
  never spans one.
- **`is_coroutine_function` is registration-time only.** Called at
  the 3 `serve*` registration sites (`mesh_rpc.rs:2465, 2514, 2561`),
  never inside `RpcHandler::call`. Correct shape.
- **Cancel propagation primitive is cheap.** Goes through substrate
  `reserve_cancel_token()` + `mesh.cancel(token)` (the notify-permit
  path), not channel close. `async_bridge.rs:114, 179-185`. Issues
  are P1+P2 (which call sites don't USE this path), not the path
  itself.
- **`AsyncMeshRpc::call*` methods share helpers cleanly.**
  `mesh_rpc.rs:2594-2640` reuse `call_options_from_dict`,
  `rpc_error_to_pyerr`, and `await_with_cancel` with their sync
  siblings — no marshalling duplication.
- **`_net.pyi` hand-mirroring is acceptable.** PyO3 type-stub
  generation tooling is poor; hand-writing is the norm. Cross-refs to
  sync class are good.
- **`__init__.py` re-exports look verbose** but each block is
  feature-gated by `try/from ._net import ...`; metaclass enumeration
  would obscure the feature gating.
- **README content is operator-useful** — migration cookbook,
  mixing-sync-and-async warning, cancellation semantics with code.
  Not narration.

---

## Suggested fix order

1. **P1 + P2** — cancel-propagation correctness. P1 = 3 watch iters
   × ~20 LOC each. P2 = ~62 call sites across 6 files (mechanical
   substitution: `future_into_py(...)` → `await_with_cancel(...)`).
   These are the only real bugs in the branch.
2. **P11** — un-skip the streaming cancel test once P1 lands;
   validates the fix.
3. **P3 + P4** — per-call perf wins. P3 = bridge return-type swap
   (`Vec<u8>` → `bytes::Bytes` wrapper that produces `PyBytes`
   directly). P4 = `parking_lot::Mutex` → `tokio::sync::Mutex` for
   the streaming state.
4. **P5 + P6 + P7** — handler-bridge / outcome-mapping / serve-timeout
   consolidations. ~150 LOC delta total, removes a class of drift
   bugs.
5. **P8 + P9 + P10** — `aclose` / admin-wrapper / decode-wrapper
   consolidations. Boring high-volume LOC reductions.
6. **P12 + P13** — test fixture conftest + module-scope mesh-pair.
   Speeds up CI and removes the 4th copy of pre-existing technical
   debt.
7. **P14 + P16 + P17** — Python retry-parse hardening + docstring
   trim + meshos.rs prelude helper.
8. **P15** — substrate-side zero-copy send. Defer.
