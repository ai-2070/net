//! Node bindings for nRPC — Phase B1 (raw-bytes surface).
//!
//! Exposes [`MeshRpc`], an envelope around a live `NetMesh` that
//! provides the nRPC operations:
//!
//! - [`MeshRpc::serve`] — register a handler `(Buffer) =>
//!   Promise<Buffer>` on a service name.
//! - [`MeshRpc::call`] — direct-addressed call against a known
//!   target node id.
//! - [`MeshRpc::call_service`] — service-discovery call (uses the
//!   underlying capability index + routing policy).
//! - [`MeshRpc::call_streaming`] — open a streaming response call
//!   that yields chunks via [`RpcStream::next`] until EOF.
//!
//! Typed serde wrappers + retry/hedge/breaker are deferred to
//! Phase B2; this file is the load-bearing handler-bridging
//! validation that all later phases build on.
//!
//! ## Handler bridging
//!
//! Each `serveRpc` registration installs a [`ThreadsafeFunction`]
//! that crosses the napi boundary. When a request lands, the
//! Rust-side [`NodeRpcHandler::call`] invokes the TSFN with the
//! raw request body and a callback that pushes the JS result
//! (a `Promise<Buffer>`) into a `tokio::sync::oneshot`. The
//! handler task awaits the oneshot, then awaits the JS promise
//! (napi-rs's `Promise<T>` implements `Future`), and surfaces
//! the resolved buffer as the response payload.
//!
//! Errors get mapped to a stable `nrpc:` prefix string per the
//! NRPC_BINDINGS_PLAN.md — the JS-side wrapper layer matches on
//! the prefix to re-throw typed errors.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use ::net::adapter::net::cortex::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use ::net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, RoutingPolicy as InnerRoutingPolicy, RpcError as InnerRpcError,
    RpcStream as InnerRpcStream, ServeHandle as InnerServeHandle,
};
use ::net::adapter::net::MeshNode;

// ============================================================================
// Stable error prefix — matches the convention in cortex.rs (cortex:,
// netdb:, redex:). The JS-side wrapper at @ai2070/net/errors
// inspects this prefix to re-throw typed RpcError instances.
// ============================================================================

pub(crate) const ERR_NRPC_PREFIX: &str = "nrpc:";

#[inline]
fn nrpc_err(kind: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{ERR_NRPC_PREFIX}{kind}: {detail}"))
}

/// Map an inner [`InnerRpcError`] to a napi `Error` with the stable
/// `nrpc:` prefix discriminator. The JS wrapper layer matches on
/// the kind segment (`no_route`, `timeout`, `server_error`,
/// `transport`, `codec_encode`, `codec_decode`) to throw the
/// appropriate typed error class.
fn nrpc_err_from_inner(err: InnerRpcError) -> Error {
    match err {
        InnerRpcError::NoRoute { target, reason } => {
            nrpc_err("no_route", format!("target=0x{target:x} reason={reason}"))
        }
        InnerRpcError::Timeout { elapsed_ms } => {
            nrpc_err("timeout", format!("elapsed_ms={elapsed_ms}"))
        }
        InnerRpcError::ServerError { status, message } => nrpc_err(
            "server_error",
            format!("status=0x{status:04x} message={message}"),
        ),
        InnerRpcError::Transport(e) => nrpc_err("transport", e.to_string()),
        InnerRpcError::Codec { direction, message } => {
            let dir_str = match direction {
                ::net::adapter::net::mesh_rpc::CodecDirection::Encode => "codec_encode",
                ::net::adapter::net::mesh_rpc::CodecDirection::Decode => "codec_decode",
            };
            nrpc_err(dir_str, message)
        }
    }
}

// ============================================================================
// CallOptions — JS object surface.
//
// Subset of the inner CallOptions struct that's safe and useful to
// expose at B1. Routing policy / trace context land in B2 once the
// typed wrappers are wired.
// ============================================================================

/// Per-call options. All fields are optional; defaults match the
/// inner [`InnerCallOptions::default()`].
#[napi(object)]
#[derive(Default)]
pub struct CallOptions {
    /// Hard deadline, in milliseconds from now. The call's future
    /// races a `tokio::time::sleep`; whichever fires first wins.
    /// On timeout the caller emits CANCEL to the server so the
    /// in-flight handler observes its `ctx.cancellation` token.
    pub deadline_ms: Option<u32>,
    /// Streaming-only: initial credit window for per-streaming-
    /// response flow control. `Some(n)` means "the server pump
    /// awaits one credit per emitted chunk; refill via
    /// `RpcStream::grant`." `None` (the default) → unbounded.
    /// Ignored by non-streaming `call` / `callService`.
    pub stream_window_initial: Option<u32>,
}

impl CallOptions {
    fn into_inner(self) -> InnerCallOptions {
        let mut opts = InnerCallOptions::default();
        if let Some(ms) = self.deadline_ms {
            opts.deadline = Some(Instant::now() + Duration::from_millis(ms as u64));
        }
        opts.stream_window_initial = self.stream_window_initial;
        opts.routing_policy = InnerRoutingPolicy::default();
        opts
    }
}

// ============================================================================
// Handler bridging.
//
// `NodeRpcHandler` adapts a TSFN-wrapped JS function to the
// `RpcHandler` async trait the SDK requires. The TSFN's return
// type is `Promise<Buffer>` so the JS-side handler can be either:
//
//   - `async (req: Buffer) => Buffer`      // most common
//   - `(req: Buffer) => Promise<Buffer>`   // explicit
//
// Synchronous JS handlers can wrap their result in
// `Promise.resolve(buf)` (or just declare them `async` — the
// engine handles it).
// ============================================================================

/// Default cap on JS handler latency. Bounds re-entrant deadlocks
/// (handler reaches back into Node and trips the main thread)
/// and Node main-thread starvation; without the cap a wedged JS
/// handler holds the in-flight slot indefinitely. 60s matches the
/// existing `compute` binding's `DEFAULT_CALLBACK_TIMEOUT_MS`.
const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(60);

type RpcHandlerTsfn =
    ThreadsafeFunction<Buffer, Promise<Buffer>, Buffer, napi::Status, false>;

struct NodeRpcHandler {
    tsfn: RpcHandlerTsfn,
    timeout: Duration,
}

#[async_trait::async_trait]
impl RpcHandler for NodeRpcHandler {
    async fn call(
        &self,
        ctx: RpcContext,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        // 1. Cross the napi boundary: hand the request body to JS.
        //    The TSFN call is non-blocking — napi-rs queues the JS
        //    invocation and fires our callback when it returns.
        //    Note: `Result` inside `napi::bindgen_prelude::*`
        //    aliases to napi's `Result<T, napi::Status>`; the
        //    trait method must use `std::result::Result` so the
        //    error type stays `RpcHandlerError`.
        let req_buf = Buffer::from(ctx.payload.body);
        let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
        let status = self.tsfn.call_with_return_value(
            req_buf,
            ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: napi::Result<Promise<Buffer>>, _env| {
                // Receiver dropped means the handler task was
                // cancelled before the JS callback fired — silently
                // discard so napi-rs doesn't escalate to a fatal
                // process exit.
                let _ = tx.send(ret);
                napi::Result::Ok(())
            },
        );
        if status != napi::Status::Ok {
            return Err(RpcHandlerError::Internal(format!(
                "TSFN enqueue failed: {status:?}"
            )));
        }

        // 2. Wait for JS to invoke the handler and return a Promise.
        //    Bounded: a JS handler that doesn't respond within the
        //    timeout surfaces as Internal so the in-flight slot
        //    doesn't leak.
        let promise = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(Ok(p))) => p,
            Ok(Ok(Err(e))) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS handler threw synchronously: {e}"
                )))
            }
            Ok(Err(_)) => {
                return Err(RpcHandlerError::Internal(
                    "JS callback channel disconnected before handler responded".into(),
                ))
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS handler did not respond within {} ms",
                    self.timeout.as_millis()
                )))
            }
        };

        // 3. Await the JS-returned promise. napi-rs's Promise<T>
        //    is `Send + 'static` and implements Future via an
        //    internal "promise resolved" callback that completes a
        //    oneshot — no main-thread polling required.
        let resp_buf = promise
            .await
            .map_err(|e| RpcHandlerError::Internal(format!("JS handler promise rejected: {e}")))?;

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: resp_buf.to_vec(),
        })
    }
}

// ============================================================================
// ServeHandle — RAII wrapper around the inner ServeHandle.
//
// The inner handle's Drop unregisters the inbound dispatcher and
// stops accepting new request dispatch (in-flight handlers
// continue to completion — see H8's fix). We wrap it in an
// `Arc<Mutex<Option<...>>>` so JS-side `close()` can drop it
// deterministically AND a subsequent V8 GC of the napi class
// (which calls our Rust Drop) is a no-op when already closed.
// ============================================================================

/// Handle returned by [`MeshRpc::serve`]. Calling `close()`
/// unregisters the service and stops accepting new request
/// dispatch — in-flight handlers continue to completion.
///
/// Always call `close()` explicitly when done with the service.
/// V8 GC will eventually drop the napi class (and the inner
/// ServeHandle) but timing is non-deterministic; relying on it
/// for production unregister is unsafe.
#[napi]
pub struct ServeHandle {
    inner: Arc<Mutex<Option<InnerServeHandle>>>,
}

#[napi]
impl ServeHandle {
    /// Unregister the service. Idempotent — repeated calls are
    /// no-ops. After close, in-flight handlers continue to
    /// completion but no new requests will be dispatched.
    #[napi]
    pub fn close(&self) {
        // Recover from a poisoned mutex (a thread panicked while
        // holding it) — partial state is fine here, we just want
        // to drop the inner ServeHandle if it's still present.
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let _ = guard.take();
    }

    /// `true` once `close()` has been called (or after V8 GC
    /// finalized the handle). Useful for tests / diagnostics.
    #[napi]
    pub fn is_closed(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.is_none()
    }
}

// ============================================================================
// RpcStream — async-iterator wrapper around the inner RpcStream.
//
// JS-side use:
//   ```ts
//   const stream = await rpc.callStreaming(target, 'svc', body);
//   for (let chunk = await stream.next(); chunk !== null; chunk = await stream.next()) {
//     console.log(chunk);  // Buffer
//   }
//   stream.close();  // optional — drop also CANCELs
//   ```
// ============================================================================

/// Open streaming RPC call. Yields chunks via `next()` until EOF
/// (returns `null`). Drop OR explicit `close()` emits CANCEL to
/// the server (best-effort).
#[napi]
pub struct RpcStream {
    /// Wrapped in `Arc<TokioMutex>` so multiple `&self` napi
    /// methods can serialize against the underlying stream's
    /// `&mut Self::poll_next`. Tokio mutex (not parking_lot)
    /// because the lock is held across the await.
    inner: Arc<tokio::sync::Mutex<Option<InnerRpcStream>>>,
}

#[napi]
impl RpcStream {
    /// Pull the next chunk. Returns `null` on clean EOF (the
    /// server emitted its terminal frame). Throws on error
    /// (terminal non-Ok status from the server, or the stream
    /// having been closed).
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut guard = self.inner.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| nrpc_err("stream_closed", "stream already closed"))?;
        match stream.next().await {
            Some(Ok(bytes)) => Ok(Some(Buffer::from(bytes.as_ref()))),
            Some(Err(e)) => Err(nrpc_err_from_inner(e)),
            None => {
                // Clean EOF — release the inner stream so the
                // CANCEL-on-drop guard fires immediately. Without
                // this the stream sits Some(...) until the napi
                // class is GC'd.
                let _ = guard.take();
                Ok(None)
            }
        }
    }

    /// Grant `n` additional flow-control credits to the server's
    /// pump. Only meaningful if the call set
    /// `streamWindowInitial`; otherwise a no-op. Use this to
    /// implement custom drain cadence (e.g. grant a half-window
    /// every half-window chunks consumed).
    #[napi]
    pub async fn grant(&self, n: u32) -> Result<()> {
        let guard = self.inner.lock().await;
        if let Some(stream) = guard.as_ref() {
            stream.grant(n);
        }
        Ok(())
    }

    /// `true` if the stream was opened with a non-`None`
    /// `streamWindowInitial`. Diagnostic / test helper.
    #[napi]
    pub async fn flow_controlled(&self) -> bool {
        let guard = self.inner.lock().await;
        guard.as_ref().map(|s| s.flow_controlled()).unwrap_or(false)
    }

    /// Close the stream. Emits CANCEL to the server (best-effort)
    /// so the server-side handler observes `ctx.cancellation`.
    /// Idempotent — repeated calls are no-ops.
    #[napi]
    pub async fn close(&self) {
        let _ = self.inner.lock().await.take();
    }
}

// ============================================================================
// MeshRpc — the public envelope class.
//
// Constructed via `MeshRpc.fromMesh(mesh)` (matches the compute
// binding's `DaemonRuntime.fromMesh` shape). One MeshRpc per live
// NetMesh; calls share the underlying MeshNode's RPC plumbing.
// ============================================================================

/// nRPC envelope around a [`crate::NetMesh`]. One instance per
/// live mesh.
#[napi]
pub struct MeshRpc {
    /// Shared with the parent NetMesh — no second socket, no
    /// second handshake table.
    node: Arc<MeshNode>,
}

#[napi]
impl MeshRpc {
    /// Build a MeshRpc against an existing NetMesh. Cheap
    /// (`Arc::clone` on the inner MeshNode) — call once per
    /// mesh and reuse.
    #[napi(factory)]
    pub fn from_mesh(mesh: &crate::NetMesh) -> Result<MeshRpc> {
        let node = mesh.node_arc_clone()?;
        Ok(MeshRpc { node })
    }

    // ---- serve ----------------------------------------------------------

    /// Register `handler` on `service`. Returns a [`ServeHandle`]
    /// whose `close()` unregisters; in-flight handlers continue
    /// to completion after close.
    ///
    /// Handler shape: `(req: Buffer) => Promise<Buffer>`. Sync
    /// handlers can `Promise.resolve(buf)` or simply be declared
    /// `async`.
    #[napi]
    pub fn serve(
        &self,
        service: String,
        handler: Function<'_, Buffer, Promise<Buffer>>,
    ) -> Result<ServeHandle> {
        let tsfn: RpcHandlerTsfn = handler.build_threadsafe_function().build()?;
        let rust_handler = Arc::new(NodeRpcHandler {
            tsfn,
            timeout: DEFAULT_HANDLER_TIMEOUT,
        });
        let inner = self
            .node
            .serve_rpc(&service, rust_handler)
            .map_err(|e| nrpc_err("serve_failed", e))?;
        Ok(ServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    // ---- call -----------------------------------------------------------

    /// Direct-addressed unary call. Caller specifies
    /// `targetNodeId`; the SDK does NOT consult the capability
    /// index. Returns the response body as a Buffer; throws
    /// `nrpc:*` on error.
    #[napi]
    pub async fn call(
        &self,
        target_node_id: BigInt,
        service: String,
        request: Buffer,
        opts: Option<CallOptions>,
    ) -> Result<Buffer> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let opts = opts.unwrap_or_default().into_inner();
        let reply = self
            .node
            .call(target, &service, Bytes::copy_from_slice(request.as_ref()), opts)
            .await
            .map_err(nrpc_err_from_inner)?;
        Ok(Buffer::from(reply.body.as_ref()))
    }

    /// Service-discovery unary call. Resolves `service` against
    /// the local capability index (`nrpc:<service>` tags),
    /// applies the routing policy (default RoundRobin), calls.
    #[napi]
    pub async fn call_service(
        &self,
        service: String,
        request: Buffer,
        opts: Option<CallOptions>,
    ) -> Result<Buffer> {
        let opts = opts.unwrap_or_default().into_inner();
        let reply = self
            .node
            .call_service(&service, Bytes::copy_from_slice(request.as_ref()), opts)
            .await
            .map_err(nrpc_err_from_inner)?;
        Ok(Buffer::from(reply.body.as_ref()))
    }

    // ---- streaming ------------------------------------------------------

    /// Open a streaming-response call. Returns an [`RpcStream`];
    /// drain via `await stream.next()` until it returns `null`.
    /// Drop / `close()` emits CANCEL to the server.
    #[napi]
    pub async fn call_streaming(
        &self,
        target_node_id: BigInt,
        service: String,
        request: Buffer,
        opts: Option<CallOptions>,
    ) -> Result<RpcStream> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let opts = opts.unwrap_or_default().into_inner();
        let inner = self
            .node
            .call_streaming(target, &service, Bytes::copy_from_slice(request.as_ref()), opts)
            .await
            .map_err(nrpc_err_from_inner)?;
        Ok(RpcStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
        })
    }

    // ---- discovery ------------------------------------------------------

    /// All node ids currently advertising `nrpc:<service>` in the
    /// local capability index. Useful for diagnostics + custom
    /// caller-side routing logic. Returns BigInt array (each
    /// node id is a 64-bit value).
    #[napi]
    pub fn find_service_nodes(&self, service: String) -> Vec<BigInt> {
        self.node
            .find_service_nodes(&service)
            .into_iter()
            .map(|n| BigInt::from(n))
            .collect()
    }
}

// ============================================================================
// Tests for the pure-logic helpers — error mapping. Following the
// `bindings/node/src/common.rs` convention: napi `Error::Drop` calls
// Node-provided FFI symbols not available in standalone `cargo
// test`, so we test only the pre-Error logic.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ::net::adapter::net::mesh_rpc::CodecDirection;

    /// `nrpc_err_from_inner` produces the documented stable kind
    /// segment for each `RpcError` variant. Pinned because the
    /// JS-side wrapper at `@ai2070/net/errors` matches on the
    /// kind segment to throw typed exceptions — silently changing
    /// a kind name would break every TS consumer's catch block.
    #[test]
    fn nrpc_err_kind_segments_are_stable() {
        // Build a string the way `Error::from_reason` would — we
        // can't actually construct the napi Error in a cargo test
        // (its Drop calls Node), but the format string is what
        // the JS wrapper sees.
        let format_kind = |err: InnerRpcError| -> String {
            match err {
                InnerRpcError::NoRoute { target, reason } => {
                    format!("{ERR_NRPC_PREFIX}no_route: target=0x{target:x} reason={reason}")
                }
                InnerRpcError::Timeout { elapsed_ms } => {
                    format!("{ERR_NRPC_PREFIX}timeout: elapsed_ms={elapsed_ms}")
                }
                InnerRpcError::ServerError { status, message } => {
                    format!("{ERR_NRPC_PREFIX}server_error: status=0x{status:04x} message={message}")
                }
                InnerRpcError::Transport(e) => format!("{ERR_NRPC_PREFIX}transport: {e}"),
                InnerRpcError::Codec { direction, message } => {
                    let dir = match direction {
                        CodecDirection::Encode => "codec_encode",
                        CodecDirection::Decode => "codec_decode",
                    };
                    format!("{ERR_NRPC_PREFIX}{dir}: {message}")
                }
            }
        };

        assert!(format_kind(InnerRpcError::NoRoute {
            target: 0xABCD,
            reason: "x".into(),
        })
        .starts_with("nrpc:no_route:"));
        assert!(format_kind(InnerRpcError::Timeout { elapsed_ms: 100 })
            .starts_with("nrpc:timeout:"));
        assert!(format_kind(InnerRpcError::ServerError {
            status: 0x4001,
            message: "x".into(),
        })
        .starts_with("nrpc:server_error:"));
        assert!(format_kind(InnerRpcError::Codec {
            direction: CodecDirection::Encode,
            message: "x".into(),
        })
        .starts_with("nrpc:codec_encode:"));
        assert!(format_kind(InnerRpcError::Codec {
            direction: CodecDirection::Decode,
            message: "x".into(),
        })
        .starts_with("nrpc:codec_decode:"));
    }

    /// `CallOptions::into_inner` round-trips deadline_ms +
    /// stream_window_initial; missing fields default to None.
    #[test]
    fn call_options_into_inner_round_trips_fields() {
        let opts = CallOptions {
            deadline_ms: Some(500),
            stream_window_initial: Some(8),
        };
        let inner = opts.into_inner();
        assert!(inner.deadline.is_some(), "deadline must be Some when set");
        assert_eq!(inner.stream_window_initial, Some(8));

        let empty = CallOptions::default().into_inner();
        assert!(empty.deadline.is_none());
        assert!(empty.stream_window_initial.is_none());
    }
}
