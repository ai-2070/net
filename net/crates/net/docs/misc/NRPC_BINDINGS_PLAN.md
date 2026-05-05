# nRPC bindings plan — Node / Python / Go

Spec for adding the nRPC surface to the three non-Rust bindings. The Rust SDK at `net/crates/net/sdk/src/mesh_rpc.rs` is the reference; the wire format and error model are locked. This doc is the per-language adaptation plan, not a re-design.

## Status

- ✅ Rust SDK — typed `serve_rpc_typed`, `call_typed`, `call_service_typed`, streaming variants, retry/hedge/breaker resilience helpers, metrics.
- ⏳ Node binding — nothing wired today (`grep -rln 'mesh_rpc\|nrpc' bindings/node/` is empty).
- ⏳ Python binding — nothing wired today.
- ⏳ Go binding — nothing wired today.

## Goal & scope

Each binding exposes the **same nRPC surface** as the Rust SDK, idiomatic to the host language. A handler written in any binding interoperates with a caller in any other binding (same wire format, same status codes, same `nrpc:<service>` capability tag).

In scope per language:

- **`serve_rpc(service, handler)`** + raw-bytes variants for users who own their serialization.
- **`call(target, service, payload, opts)`**, **`call_service(...)`**, **`call_streaming(...)`**.
- **Typed wrappers** with codec selection (default JSON; per-language native serialization as an option).
- **Resilience helpers** — `call_with_retry`, `call_with_hedge`, `CircuitBreaker`.
- **Error mapping** — `RpcError` variants → idiomatic exceptions / error returns.
- **`ServeHandle`** — RAII-style unregister, expressed in each language's lifecycle idiom.
- **Metrics** — `rpc_metrics_snapshot()` returning the per-service counter set, plus the Prometheus text formatter.

Out of scope:

- Schema-validated payloads / IDL codegen — deferred to Phase 4 (matches the Rust SDK's stance).
- Server-streaming + bidirectional streaming — only response-streaming is exposed; matches the wire spec.
- Direct construction of `RpcInboundDispatcher` — that's a Rust-only advanced surface.

## Cross-cutting decisions

These apply to all three bindings. Lock them up front so each binding's design is consistent.

### Error mapping

`RpcError` has 5 variants today: `NoRoute`, `Timeout`, `ServerError { status, message }`, `Transport`, `Codec { direction, message }`. Map each to the host language's idiomatic error type with a **stable string discriminator** (matching the existing `cortex:` / `netdb:` / `redex:` prefix convention in `bindings/node/src/cortex.rs`).

Proposed prefix: **`nrpc:`**. Examples:

- `NoRoute` → `"nrpc:no_route: target=0xABCD..., reason=no session..."` 
- `Timeout` → `"nrpc:timeout: elapsed_ms=200"`
- `ServerError { status: 0x4001, message }` → `"nrpc:server_error: status=0x4001, message=..."`
- `Transport(e)` → `"nrpc:transport: ..."`
- `Codec { direction, message }` → `"nrpc:codec_encode: ..."` / `"nrpc:codec_decode: ..."`

Status code constants `NRPC_TYPED_BAD_REQUEST = 0x8000` / `NRPC_TYPED_HANDLER_ERROR = 0x8001` re-exported as host-language constants so callers can pattern-match.

### Async model

Each binding bridges async differently:

- **Node**: napi promotes async Rust functions to JS Promises automatically via `#[napi]` on `async fn`. A handler written as a JS function returning a Promise needs a Tokio→Promise bridge (capture an `Arc<Notify>` + completion channel).
- **Python**: pyo3 uses `#[pyfunction]` + `Runtime::block_on` for sync Python, OR `pyo3-asyncio` for native `async def` integration. We pick **pyo3-asyncio** so handlers can be `async def` (idiomatic).
- **Go**: cgo doesn't directly support async; the existing pattern (see `compute-ffi`) is "FFI calls block; Go side wraps in goroutines." Handlers cross the FFI as C function pointers + caller-supplied context; the FFI side calls back via a thread-safe channel.

### Codec selection

Default codec is **JSON** in every binding (matches Rust SDK). Per-language additions:

- **Node**: also accept `Buffer` directly for users who already have encoded bytes — equivalent to the raw-bytes path.
- **Python**: accept `bytes` directly; optional `pickle` codec gated behind an explicit flag (because pickle is a remote-code-execution surface).
- **Go**: accept `[]byte` directly; optional `encoding/gob` codec for Go-to-Go.

Cross-language calls always use JSON unless both sides explicitly agree on something else out-of-band — same constraint as the Rust SDK already documents.

### Lifecycle (`ServeHandle`)

Each binding expresses the `ServeHandle::Drop` → unregister contract differently:

- **Node**: `ServeHandle` is an `#[napi]` class; the JS-side `serveHandle.close()` method and the Node `Symbol.dispose` (TC39 explicit resource management) both unregister. **No silent finalization** — Node's GC is not deterministic enough for "call site forgot to close" to be safe.
- **Python**: context-manager protocol (`__enter__` / `__exit__`) and an explicit `.close()` method. Hold via `with mesh.serve_rpc(...) as handle:`.
- **Go**: pair `ServeHandle` with a `Close() error` method + a `runtime.SetFinalizer` as a backstop (matches the existing `compute-ffi` handle-model convention).

In every case the docstring states "drop / close stops new dispatch but lets in-flight handlers complete" — same contract as Rust's H8 fix.

### Metrics

`rpc_metrics_snapshot()` returns a per-service counter set. Map to:

- **Node**: `RpcMetricsSnapshot` `#[napi(object)]` with field-typed BigInt counters.
- **Python**: a `@dataclass` (constructed via `pyo3` from the snapshot struct).
- **Go**: a Go struct mirrored from the C-FFI snapshot; field types `uint64` / `int64`.

`prometheus_text()` is a pure string method on the snapshot — trivial to wrap in any language.

## Per-binding plans

### Node

**Module path:** `bindings/node/src/mesh_rpc.rs` (mirrors `cortex.rs`).

**Types exposed:**

```rust
#[napi]
pub struct Mesh { /* wraps net_sdk::mesh::Mesh */ }

#[napi]
impl Mesh {
    #[napi]
    pub async fn serve_rpc_typed(&self, service: String, handler: ThreadsafeFunction<Buffer>) -> Result<ServeHandle>;
    #[napi]
    pub async fn call_typed(&self, target: BigInt, service: String, request: Buffer, opts: CallOptions) -> Result<Buffer>;
    #[napi]
    pub async fn call_service_typed(&self, service: String, request: Buffer, opts: CallOptions) -> Result<Buffer>;
    #[napi]
    pub async fn call_streaming(&self, target: BigInt, service: String, request: Buffer, opts: CallOptions) -> Result<RpcStream>;
    #[napi]
    pub fn rpc_metrics_snapshot(&self) -> RpcMetricsSnapshot;
}

#[napi]
pub struct ServeHandle { /* on Drop: unregister */ }
#[napi]
impl ServeHandle {
    #[napi] pub fn close(&self);
    #[napi(js_name = "[Symbol.dispose]")] pub fn dispose(&self);
}

#[napi]
pub struct RpcStream { /* AsyncIterator over chunks */ }
#[napi]
impl RpcStream {
    #[napi] pub async fn next(&self) -> Result<Option<Buffer>>;
    #[napi] pub fn close(&self);  // emits CANCEL
    #[napi] pub fn grant(&self, n: u32);  // flow-control credit
}
```

**Handler bridging:** the `ThreadsafeFunction<Buffer>` is the napi pattern for "JS function callable from Rust." The Rust side spawns a Tokio task that calls the threadsafe function, awaits its returned Promise (via `tokio::sync::oneshot`), and returns the result.

**Resilience helpers:** mirror the Rust `Mesh::call_with_retry` shape. Take `RetryPolicy` / `HedgePolicy` / `CircuitBreaker` as `#[napi(object)]` config objects.

**Error throws:** `napi::Error::from_reason(format!("nrpc:{kind}: ..."))`. JS-side `@ai2070/net-sdk/errors` adds an `RpcError` class hierarchy that re-throws on the prefix (matches the existing pattern at `bindings/node/src/cortex.rs:47-64`).

**Estimated work:** ~800-1200 LoC binding + ~300 LoC TypeScript wrapper + tests.

### Python

**Module path:** `bindings/python/src/mesh_rpc.rs`.

**Types exposed:**

```rust
#[pyclass]
struct Mesh { /* wraps net_sdk::mesh::Mesh */ }

#[pymethods]
impl Mesh {
    fn serve_rpc_typed(&self, service: String, handler: PyObject) -> PyResult<ServeHandle>;
    fn call_typed<'py>(&self, py: Python<'py>, target: u64, service: String, request: &Bound<'py, PyBytes>, opts: CallOptions) -> PyResult<Bound<'py, PyAny>>; // returns awaitable
    fn call_streaming<'py>(&self, ...) -> PyResult<RpcStream>;
    fn rpc_metrics_snapshot(&self) -> RpcMetricsSnapshot;
}

#[pyclass]
struct ServeHandle { /* implements __enter__/__exit__ + close() */ }

#[pyclass]
struct RpcStream { /* implements __aiter__/__anext__ */ }
```

**Async integration:** use **pyo3-asyncio** so:

- Rust async functions are exposed as Python awaitables (`call_typed` returns an awaitable).
- Python `async def` handlers can be passed to `serve_rpc_typed`; the Rust side calls them via `pyo3_asyncio::tokio::into_future`.

**Handler bridging:** the user's Python `async def` is passed as a `PyObject`. Rust spawns a `tokio::task::spawn` that:

1. Acquires the GIL, calls the function with the decoded `Req` bytes, gets back a coroutine.
2. Releases the GIL, runs `pyo3_asyncio::tokio::into_future(coro).await`.
3. Acquires the GIL again, encodes the result.

**Error raises:** `PyRuntimeError::new_err(format!("nrpc:{kind}: ..."))`. Python wrapper at `python/sdk/errors.py` adds an `RpcError` exception class hierarchy that catches by prefix and re-raises typed.

**Codec note:** Python users naturally want `dict` / `dataclass` round-tripping. The default JSON codec serializes Python dicts via `serde_json` round-tripping (decode bytes → `Value` → bytes for the wire). Native Pythonic codec (using `json` module directly on the Python side) is exposed as `Codec.PythonJson` for users who want to avoid the double-decode.

**Estimated work:** ~600-1000 LoC binding + ~250 LoC Python wrapper + tests.

### Go

**Two-crate model** matches the existing `compute-ffi` shape:

1. **`net/crates/net/src/ffi/mesh_rpc.rs`** — C-ABI exports for the nRPC surface. Stable function signatures, `c_int` return codes, opaque handle pointers.
2. **`net/crates/net/bindings/go/mesh_rpc.go`** — idiomatic Go wrapper around the C ABI.

**C ABI:**

```c
// Lifecycle
int net_rpc_serve(MeshHandle*, const char* service,
                  RpcHandlerFn handler, void* user_data,
                  ServeHandle** out_handle, char** out_err);
int net_rpc_serve_handle_close(ServeHandle*);
void net_rpc_serve_handle_free(ServeHandle*);

// Calls (block on the caller's goroutine; Go side wraps in `go func() { ... }`)
int net_rpc_call(MeshHandle*, uint64_t target, const char* service,
                 const uint8_t* req, size_t req_len,
                 const RpcCallOptions* opts,
                 uint8_t** out_resp, size_t* out_resp_len, char** out_err);

int net_rpc_call_streaming(MeshHandle*, uint64_t target, const char* service,
                           const uint8_t* req, size_t req_len,
                           const RpcCallOptions* opts,
                           RpcStream** out_stream, char** out_err);

int net_rpc_stream_next(RpcStream*,
                        uint8_t** out_chunk, size_t* out_chunk_len,
                        int* out_done, char** out_err);
void net_rpc_stream_close(RpcStream*);
void net_rpc_stream_free(RpcStream*);

// Handler callback type
typedef int (*RpcHandlerFn)(
    const uint8_t* req, size_t req_len,
    uint8_t** out_resp, size_t* out_resp_len,
    char** out_err,
    void* user_data
);
```

**Go-side wrapper** (`bindings/go/net/mesh_rpc.go`):

```go
type Mesh struct { /* wraps C MeshHandle* */ }

func (m *Mesh) ServeRPC(ctx context.Context, service string, handler func(ctx context.Context, req []byte) ([]byte, error)) (*ServeHandle, error)
func (m *Mesh) Call(ctx context.Context, target uint64, service string, req []byte, opts CallOptions) ([]byte, error)
func (m *Mesh) CallStreaming(ctx context.Context, ...) (*RpcStream, error)

type ServeHandle struct { /* opaque + close-once + finalizer */ }
func (h *ServeHandle) Close() error

type RpcStream struct { /* opaque + iterator */ }
func (s *RpcStream) Recv(ctx context.Context) ([]byte, bool, error)  // (chunk, done, err)
func (s *RpcStream) Close() error
```

**Handler bridging:** the trickiest part. Go's `func` can't be passed directly through cgo. The pattern (already used by `compute-ffi`) is:

1. Go registers a handler as a callback via cgo: `C.net_rpc_serve(..., C.RpcHandlerFn(C.go_handler_trampoline), C.uintptr_t(handlerID), ...)`.
2. `go_handler_trampoline` is a `//export`-ed Go function that looks up `handlerID` in a process-wide handler registry (`sync.Map`) and invokes the user's `func`.
3. The Rust side spawns the C callback on a Tokio task that joins via `tokio::task::spawn_blocking` (the C call is sync from Rust's perspective).

**Async integration:** Go handles concurrency via goroutines. Each `Call`/`CallStreaming` blocks the calling goroutine; the user wraps in `go func()` for concurrency. `context.Context` cancellation maps to `RpcStream::close()` / direct CANCEL via a watchdog goroutine that invokes `net_rpc_stream_close` if `ctx.Done()` fires.

**Errors:** the C ABI's `out_err` (`**char`) is a heap-allocated UTF-8 string the Go side wraps in `error` and frees with `net_rpc_free_cstring` (existing convention from `compute-ffi`). Stable `nrpc:` prefix; Go wrapper exposes `RpcError`, `NoRouteError`, `TimeoutError`, etc. as concrete types matching by prefix.

**Estimated work:** ~1500-2000 LoC C-ABI + ~600-800 LoC Go wrapper + tests.

## Phasing

Suggested order — each phase ships independently:

| Phase | Scope |
|-------|-------|
| **B1** | Node — raw `serve_rpc` / `call` / `call_streaming` (Buffer in/out). No typed wrappers. Validates the napi handler-bridging pattern is correct. |
| **B2** | Node — typed wrappers + retry/hedge/breaker. |
| **B3** | Python — raw + pyo3-asyncio integration. |
| **B4** | Python — typed wrappers + resilience helpers. |
| **B5** | Go C-ABI — raw lifecycle + call. |
| **B6** | Go C-ABI — streaming + resilience helpers. |
| **B7** | Cross-binding integration tests — call from any language to any other. |

Each binding's metrics + Prometheus snapshot lands as part of the **last** phase for that language (B2 / B4 / B6) so it has the full counter set to expose.

Phases are language-independent — Node's full pipeline (B1+B2) can finish before Python starts.

## Testing strategy

Per binding:

- **Unit tests** for the FFI/N-API/PyO3 layer using language-native test runners (`cargo test`-with-`#[napi]`, `pytest`, `go test`).
- **Cross-language integration tests** — a Rust test that boots a Mesh, then drives a binding from a subprocess (e.g. `node`/`python`/`go run` invoked via `Command::new`) and asserts the Rust server's metrics reflect the cross-language calls. Lives in `net/crates/net/tests/cross_lang_nrpc/`. Run only when the relevant binding feature is enabled.

Cross-binding test matrix (B7):

| Caller \ Server | Rust | Node | Python | Go |
|---|---|---|---|---|
| **Rust**   | ✅ existing | new | new | new |
| **Node**   | new | new | new | new |
| **Python** | new | new | new | new |
| **Go**     | new | new | new | new |

15 new integration tests (4×4 minus the diagonal, plus self-tests). One canonical "echo + sum" service in each binding makes this tractable — every test is a 4-line round-trip assertion.

## Open design questions

1. **Codec mismatch on the wire** — Rust's `Codec::Json` produces JSON; a Node caller using `Buffer` directly bypasses the codec. If a Rust handler is registered as `serve_rpc_typed::<Req, Resp>` (JSON-decoded) and the Node caller sends raw bytes, the typed handler's decode fails. Document this clearly: typed-with-typed across languages always uses JSON; raw-with-raw is host's responsibility.

2. **Python GIL contention** — a typed handler that takes the GIL on every call is fine for low-throughput services but caps at GIL-bound throughput (~50k QPS on a single Python interpreter). Document and recommend `pickle`-codec or raw-bytes path for hot inner loops.

3. **Go context cancellation propagation** — when `ctx` cancels mid-stream, Rust's `RpcStream::Drop` already fires CANCEL. The Go binding's job is just "watcher goroutine that calls `net_rpc_stream_close` when `ctx.Done()` fires." Pin the watcher goroutine's lifetime to the stream's `Close()` to avoid leaking goroutines past stream lifetime.

4. **Should the C ABI version-stamp itself?** The C ABI is consumed by Go today and may be consumed by future languages. A `net_rpc_abi_version() -> u32` constant lets consumers detect drift. Recommend adding it as part of B5.

## Out of scope (carry-forward from NRPC_DESIGN.md)

- Schema / IDL codegen — `.nrpc` files → typed clients per binding. Phase 4.
- Client-streaming + bidirectional streaming — wire-format extension required.
- Service-mesh sidecar.

## Estimated total LoC

| Binding | C-ABI / FFI | Wrapper code | Tests | Total |
|---|---|---|---|---|
| Node | — (uses napi-rs directly) | ~1500 | ~600 | ~2100 |
| Python | — (uses pyo3 directly) | ~1200 | ~500 | ~1700 |
| Go | ~1800 (Rust C-ABI) | ~700 (Go) | ~600 | ~3100 |
| Cross-lang tests | — | — | ~800 | ~800 |
| **Total** | ~1800 | ~3400 | ~2500 | **~7700** |

Roughly a 2-3 week effort for one engineer per binding pair, parallelizable across all three. The C ABI is the longest pole because it has both Rust + Go ends and zero existing nRPC scaffolding.
