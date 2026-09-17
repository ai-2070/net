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
    /// Tokio mutex so `__anext__` can hold the guard across
    /// `stream.next().await` — one acquire per pull.
    inner: Arc<TokioMutex<Option<InnerStream>>>,
    /// Set by `close()`; the next pull exits with `StopAsyncIteration`.
    closed: Arc<AtomicBool>,
    mesh: Arc<MeshNode>,
    /// Cancel-token reserved by the call that constructed the
    /// stream — the same token the opening `CallOptions` carries.
    cancel_token: u64,
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
        let closed = self.closed.clone();
        let mesh = self.mesh.clone();
        let token = self.cancel_token;
        // Thread the construction-time token, don't mint a fresh
        // one: a mid-stream `asyncio.wait_for(...).cancel()` must
        // terminate the WHOLE stream (via the substrate's
        // `arm_stream_cancel` watcher), not just this pull.
        crate::async_bridge::await_with_existing_token(py, &mesh, token, async move {
            let mut guard = inner.lock().await;
            if closed.load(Ordering::Acquire) {
                *guard = None;
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            }
            let Some(stream) = guard.as_mut() else {
                return Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()));
            };
            match stream.next().await {
                Some(Ok(item)) => Python::attach(|py| Ok(item.into_pyobject(py)?.unbind())),
                Some(Err(e)) => {
                    *guard = None;
                    Err(rpc_error_to_pyerr(e))
                }
                None => {
                    *guard = None;
                    Err(pyo3::exceptions::PyStopAsyncIteration::new_err(()))
                }
            }
        })
    }
}
```

Key rules:

1. **`__aiter__` returns `slf`** — PEP 525 contract; lets `async
   for x in iter` work.
2. **`__anext__` returns a fresh awaitable per call.** Each call
   re-enters `await_with_existing_token`; the inner stream is the
   shared state behind the mutex.
3. **`StopAsyncIteration` for clean EOF.** Don't return `None` or
   a sentinel from `__anext__`.
4. **Thread the construction-time token — do not call
   `await_with_cancel` on the per-chunk pull.** The `cancel_token`
   was reserved by the call that opened the stream and is stored
   on the handle; passing it to `await_with_existing_token` means
   a task cancel fires `Mesh::cancel(token)`, and the substrate's
   `arm_stream_cancel` watcher terminates the whole stream rather
   than dropping one pull.

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
      `rpc_error_to_pyerr` — the shared converter, never
      duplicated.
- [ ] **Module re-export.** `bindings/python/python/net/__init__.py`
      lists `AsyncFoo` in `__all__` alongside `Foo`.
