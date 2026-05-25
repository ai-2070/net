# pyo3 binding internals — async patterns

Patterns + conventions for the side-by-side `Async*` surface
landing per `PYTHON_ASYNC_SDK_SIDE_BY_SIDE.md`. Read this before
adding a new `Async*` class so the new surface stays consistent
with the ones already shipped.

## Pattern: awaitable unary entry-point

Every `Async*` method that performs I/O follows the same shape:

```rust
#[pymethods]
impl PyAsyncMeshRpc {
    fn call<'py>(
        &self,
        py: Python<'py>,
        target_node_id: u64,
        service: String,
        request: &Bound<'py, PyBytes>,
        opts: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req_bytes = Bytes::copy_from_slice(request.as_bytes());
        let service = service.clone();
        let node = self.node.clone();
        let mut inner_opts = call_options_from_dict(opts)?;
        async_bridge::await_with_cancel(py, &self.node, move |token| {
            inner_opts.cancel_token = Some(token);
            async move {
                node.call(target_node_id, &service, req_bytes, inner_opts).await
            }
        })
    }
}
```

Key rules:

1. **Method returns `PyResult<Bound<'py, PyAny>>`** — the
   awaitable, not the resolved value. Python `await`s it.
2. **No `block_on` in the body.** The whole body is
   `await_with_cancel(...)`. If you find yourself reaching for
   `runtime.block_on`, you're in the wrong code path — that's the
   sync `Foo` class, not `AsyncFoo`.
3. **Cancel-token plumbing is non-negotiable.** `await_with_cancel`
   mints the token, populates `inner_opts.cancel_token`, and arms
   the asyncio-cancel → `MeshNode::cancel(token)` bridge. Skipping
   it means `asyncio.wait_for(...)` cancellations silently fail
   to propagate.
4. **Move ownership into the async block.** Clone `String` /
   `Arc<MeshNode>` / `Bytes` BEFORE the closure; never borrow
   from `py` or local stack inside the spawned future.
5. **Errors flow through `RpcError → PyErr`** via the existing
   `rpc_error_to_pyerr` helper. Don't duplicate that mapping.

## Pattern: async iterator (`__aiter__` / `__anext__`)

Server-pushed streams (`AsyncRpcStream`, `AsyncRedexTailIter`,
`AsyncMemoryWatchIter`, `AsyncSnapshotStream`, ...) implement
PEP 525:

```rust
#[pyclass(name = "AsyncFooIter", module = "_net")]
pub struct PyAsyncFooIter {
    inner: Arc<Mutex<Option<InnerStream>>>,
    mesh: Arc<MeshNode>,
}

#[pymethods]
impl PyAsyncFooIter {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        // No cancel-token on per-chunk pulls: the stream itself
        // carries a cancel keep-alive from construction
        // (StreamCancelKeepAlive — see substrate's
        // arm_stream_cancel). Closing the iterator drops the
        // stream which fires CANCEL on the wire.
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = inner.lock();
            let Some(stream) = guard.as_mut() else {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            };
            match stream.next().await {
                Some(item) => Python::attach(|py| Ok(item.into_pyobject(py)?.unbind())),
                None => Err(pyo3::exceptions::PyStopAsyncIteration::new_err(())),
            }
        })
    }
}
```

Key rules:

1. **`__aiter__` returns `slf`** — PEP 525 contract; lets `async
   for x in iter` work.
2. **`__anext__` returns a fresh awaitable per call.** Each call
   re-enters `future_into_py`; the inner stream is the shared
   state behind a mutex.
3. **`StopAsyncIteration` for clean EOF.** Don't return `None` or
   a sentinel from `__anext__`.
4. **No `await_with_cancel` on the per-chunk pull.** The cancel
   keep-alive lives on the stream handle (set at construction
   via `arm_stream_cancel`); per-chunk cancel is handled by
   dropping the iterator, which closes the stream.

## Migration template: sync → async sibling

Adding `AsyncFoo` alongside an existing `Foo`. Walk the existing
sync class top-to-bottom, transforming each method:

### Before (sync)

```rust
#[pymethods]
impl PyFoo {
    fn frobnicate(&self, py: Python<'_>, key: String) -> PyResult<u64> {
        let node = self.node.clone();
        let runtime = self.runtime.clone();
        py.detach(|| {
            runtime.block_on(async move {
                node.frobnicate(&key)
                    .await
                    .map_err(rpc_error_to_pyerr)
            })
        })
    }
}
```

### After (async sibling)

```rust
#[pymethods]
impl PyAsyncFoo {
    fn frobnicate<'py>(
        &self,
        py: Python<'py>,
        key: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let node = self.node.clone();
        async_bridge::await_with_cancel(py, &self.node, move |token| async move {
            // If `node.frobnicate` accepts CallOptions, populate
            // .cancel_token = Some(token) here. If not, the token
            // is harmless (the cancel arm just won't propagate).
            node.frobnicate(&key)
                .await
                .map_err(rpc_error_to_pyerr)
        })
    }
}
```

### Code-review checklist

When the diff lands a new `Async*` method, the reviewer checks:

- [ ] **Class name matches the `Foo` it parallels.** `AsyncMeshRpc`
      mirrors `MeshRpc`, `AsyncRpcStream` mirrors `RpcStream`. No
      `AsyncRpcStreamAdapter` or `MeshRpcAsync`.
- [ ] **Constructor accepts the same arguments as the sync
      sibling.** A `NetMesh` instance can be passed to either; the
      pyo3 layer never forces the caller to choose async at
      construction time.
- [ ] **Every awaitable method calls `await_with_cancel`.** Raw
      `future_into_py` without the cancel-token bridge is a
      regression — the asyncio task cancel path won't propagate.
      Exception: methods that don't go through `MeshNode` (pure
      local lookups) can use `future_into_py` directly.
- [ ] **Streaming classes use `__aiter__` + `__anext__`**, not
      `__iter__` + `__next__`. Both protocols on one class is
      almost never what you want.
- [ ] **Cross-references in docstrings.** The sync `Foo.frobnicate`
      docstring gains a line: "Async equivalent:
      :meth:`AsyncFoo.frobnicate`." The async docstring carries the
      reverse: "Sync equivalent: :meth:`Foo.frobnicate`."
- [ ] **No `block_on` in the `Async*` path.** Grep the diff for
      `block_on` / `py.detach`. If either appears under the
      `Async*` impl, something is wrong.
- [ ] **Same error mapping as the sync sibling.**
      `rpc_error_to_pyerr` / `cortex_error_to_pyerr` / etc. —
      shared helpers, never duplicated.
- [ ] **Module re-export.** `bindings/python/python/net/__init__.py`
      lists `AsyncFoo` in `__all__` alongside `Foo`.
