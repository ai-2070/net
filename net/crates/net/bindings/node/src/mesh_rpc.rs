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

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

use ::net::adapter::net::cortex::{
    RequestStream as InnerRequestStream, RpcCallEvent as InnerRpcCallEvent,
    RpcCallStatus as InnerRpcCallStatus, RpcClientStreamingHandler, RpcContext,
    RpcDirection as InnerRpcDirection, RpcDuplexHandler, RpcHandler, RpcHandlerError, RpcObserver,
    RpcResponsePayload, RpcResponseSink as InnerRpcResponseSink, RpcStatus, RpcStreamingContext,
};
use ::net::adapter::net::mesh_rpc::{
    CallOptions as InnerCallOptions, ClientStreamCallRaw as InnerClientStreamCallRaw,
    DuplexCallRaw as InnerDuplexCallRaw, DuplexSink as InnerDuplexSink,
    DuplexStream as InnerDuplexStream, RoutingPolicy as InnerRoutingPolicy,
    RpcError as InnerRpcError, RpcStream as InnerRpcStream, ServeHandle as InnerServeHandle,
};
use ::net::adapter::net::mesh_rpc_metrics::{
    RpcMetricsSnapshot as InnerRpcMetricsSnapshot, ServiceMetrics as InnerServiceMetrics,
};
use ::net::adapter::net::MeshNode;

// ============================================================================
// Stable error prefix — matches the convention in cortex.rs (cortex:,
// netdb:, redex:). The JS-side wrapper at @net-mesh/core/errors
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
        InnerRpcError::CapabilityDenied { target, capability } => nrpc_err(
            "capability_denied",
            format!("target=0x{target:x} capability={capability}"),
        ),
        InnerRpcError::Cancelled => nrpc_err("cancelled", "call cancelled by caller"),
    }
}

// ============================================================================
// Cancellation surface.
//
// AbortSignal-friendly cancel-token pass-through (v3 / C-A1).
//
// JS users mint a `cancelToken: bigint` via
// `MeshRpc.reserveCancelToken()`, pass it on the call's options,
// then call `MeshRpc.cancelCall(token)` from a parallel context
// (e.g. an AbortSignal listener). Both methods delegate to the
// SDK's `MeshNode::reserve_cancel_token` / `MeshNode::cancel`
// primitives (v3 / C-S1) — the napi binding no longer owns a
// local cancel registry. The CallOptions::cancel_token is
// populated into the inner SDK CallOptions and the substrate
// handles abort + CANCEL emission uniformly across every call
// shape (unary, streaming-response, client-streaming, duplex).
// ============================================================================

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
    /// Client-streaming / duplex only: initial credit window for
    /// per-call request-direction flow control. Mirror of
    /// [`stream_window_initial`] for the upload direction. The
    /// SDK's `ClientStreamCallTyped::send` / `DuplexCallTyped::send`
    /// gate on credit when this is set. Server refills via
    /// `REQUEST_GRANT` after each consumed chunk. `None`
    /// (the default) → unbounded.
    pub request_window_initial: Option<u32>,
    /// Caller-side cancel token for AbortSignal integration. Mint
    /// via `MeshRpc.reserveCancelToken()`, pass here, then call
    /// `MeshRpc.cancelCall(token)` from your AbortSignal listener
    /// (or any other cancel trigger) to abort the in-flight call.
    /// On cancel the call rejects with `nrpc:cancelled:` so user
    /// code can match via `classifyError`. Defaults to no cancel
    /// surface (cheaper fast path; no tokio spawn / registry
    /// overhead).
    pub cancel_token: Option<BigInt>,
    /// Caller-supplied request headers, appended to the wire
    /// `RpcRequestPayload.headers` after any auto-generated
    /// headers (trace, stream-window). Used for application-level
    /// metadata the server needs at dispatch-time — most notably
    /// the `net-where` predicate header for Phase 9b
    /// predicate-pushdown filtering.
    ///
    /// JS callers pass `[{ name: "net-where", value: Buffer.from(jsonBytes) }, ...]`.
    /// `undefined` (default) → no extra headers.
    pub request_headers: Option<Vec<RpcRequestHeader>>,
}

/// A single `(name, value)` request-header entry. Names follow the
/// lowercase `cyberdeck-*` / `nrpc-*` convention; the substrate
/// doesn't validate names beyond the `MAX_RPC_HEADER_NAME_LEN` cap.
#[napi(object)]
pub struct RpcRequestHeader {
    /// Header name (e.g. `net-where`).
    pub name: String,
    /// Header value bytes. For text-like headers (predicates,
    /// trace-context), the contents are UTF-8 strings encoded as
    /// `Buffer.from(str)`.
    pub value: Buffer,
}

impl CallOptions {
    /// Convert the user-facing options into the inner SDK
    /// `CallOptions`. With v3 / C-A1 the `cancel_token` rides on
    /// the inner options uniformly across every call shape; the
    /// previous `split` shim that pulled the token out for a
    /// binding-side wrapper is gone now that the SDK owns the
    /// primitive.
    ///
    /// `None` / `Some(0)` cancel-token → the SDK observes the
    /// "no token" sentinel and does no registry work.
    fn into_inner(self) -> InnerCallOptions {
        let mut opts = InnerCallOptions::default();
        if let Some(ms) = self.deadline_ms {
            opts.deadline = Some(Instant::now() + Duration::from_millis(ms as u64));
        }
        opts.stream_window_initial = self.stream_window_initial;
        opts.request_window_initial = self.request_window_initial;
        opts.routing_policy = InnerRoutingPolicy::default();
        if let Some(headers) = self.request_headers {
            opts.request_headers = headers
                .into_iter()
                .map(|h| (h.name, h.value.to_vec()))
                .collect();
        }
        let token = self
            .cancel_token
            .map(crate::common::bigint_u64)
            .transpose()
            .ok()
            .flatten();
        opts.cancel_token = token.filter(|t| *t != 0);
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

/// Stable `nrpc:app_error:` error-message prefix the JS-side
/// `serve` wrapper uses to signal "I want a typed Application
/// status code surfaced to the caller, not a generic Internal."
/// JS handlers throw `new Error("nrpc:app_error:0x8000:<json
/// body>")` and this side maps it to
/// `RpcHandlerError::Application { code, message }`. Mirrors
/// the Python binding's `RpcAppError(code, body)` mechanism.
const JS_APP_ERROR_PREFIX: &str = "nrpc:app_error:";

/// Parse a JS-thrown `nrpc:app_error:0x<code>:<body>` message
/// Convert an owned napi `Buffer` into `bytes::Bytes` without
/// an extra copy. napi-rs 3.x backs `Buffer` with an `Arc<Vec<u8>>`
/// internally; `Bytes::from_owner` takes ownership of the Buffer
/// (preserving the Arc clone) so the resulting `Bytes` borrows the
/// same allocation. Replaces the previous `Bytes::copy_from_slice`
/// pattern that paid a per-chunk memcpy at the JS↔Rust boundary.
fn napi_buffer_to_bytes(buf: Buffer) -> Bytes {
    Bytes::from_owner(buf)
}

/// into the (code, body) pair the SDK expects for
/// `RpcHandlerError::Application`. Returns `None` if the prefix
/// is absent or the format is malformed (caller falls through to
/// the generic Internal mapping). Format chosen to be
/// human-readable + grep-friendly; Python's pyo3 path uses an
/// exception class instead because raising a typed exception is
/// the natural pattern there.
fn parse_js_app_error(message: &str) -> Option<(u16, String)> {
    let rest = message.strip_prefix(JS_APP_ERROR_PREFIX)?;
    let (code_str, body) = rest.split_once(':')?;
    let code_hex = code_str
        .strip_prefix("0x")
        .or(code_str.strip_prefix("0X"))?;
    let code = u16::from_str_radix(code_hex, 16).ok()?;
    Some((code, body.to_string()))
}

type RpcHandlerTsfn = ThreadsafeFunction<Buffer, Promise<Buffer>, Buffer, napi::Status, false>;

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
        let req_buf = Buffer::from(ctx.payload.body.to_vec());
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
        let resp_buf = match promise.await {
            Ok(buf) => buf,
            Err(e) => {
                // Inspect the rejection message — a JS handler that
                // wants to signal a typed application status throws
                // `new Error("nrpc:app_error:0x8000:<body>")`. Map
                // that to RpcHandlerError::Application so the fold
                // emits RpcResponsePayload { status: Application(_),
                // body }; otherwise fall through to the generic
                // Internal mapping. Mirrors the Python binding's
                // RpcAppError pathway.
                let msg = e.to_string();
                if let Some((code, body)) = parse_js_app_error(&msg) {
                    return Err(RpcHandlerError::Application {
                        code,
                        message: body,
                    });
                }
                return Err(RpcHandlerError::Internal(format!(
                    "JS handler promise rejected: {e}"
                )));
            }
        };

        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: resp_buf.to_vec().into(),
        })
    }
}

// ============================================================================
// Observer + metrics POD shapes (S2-A1).
//
// The JS surface mirrors the substrate's `RpcCallEvent` and
// `RpcMetricsSnapshot` types — the only encoding choices are:
//   - u64 fields → `BigInt` (per the binding convention in
//     aggregator.rs)
//   - `RpcCallStatus` tagged union → flattened to
//     `statusKind: "ok"|"error"|"timeout"|"canceled"` plus an
//     optional `statusMessage` (populated only when statusKind ==
//     "error"). napi-rs's `#[napi(object)]` POD layer doesn't
//     support tagged unions natively; this string-discriminant
//     shape is the same pattern the cortex error mapping uses
//     for its `nrpc:<kind>:<msg>` prefix.
//   - `RpcDirection` enum → flattened to `direction:
//     "outbound"|"inbound"` for the same reason.
// ============================================================================

/// Single observed RPC call boundary. Surfaced to the observer
/// callback installed via [`MeshRpc::set_observer`].
#[napi(object)]
pub struct RpcCallEventJs {
    /// 64-bit node id of the calling node. Equal to the local
    /// node id on outbound events.
    pub caller: BigInt,
    /// 64-bit node id of the responding node.
    pub callee: BigInt,
    /// Service / method name.
    pub method: String,
    /// Elapsed time in milliseconds.
    pub latency_ms: u32,
    /// `"ok"` | `"error"` | `"timeout"` | `"canceled"`. The JS
    /// side should match on this discriminant before reading
    /// `statusMessage`.
    pub status_kind: String,
    /// Populated only when `statusKind === "error"`. Carries an
    /// operator-readable diagnostic from the substrate.
    pub status_message: Option<String>,
    /// Wire payload size of the request body (0 when not
    /// available).
    pub request_bytes: u32,
    /// Wire payload size of the response body (0 when not
    /// available).
    pub response_bytes: u32,
    /// `"outbound"` (this node initiated) or `"inbound"` (this
    /// node received). v1 only emits `"outbound"`.
    pub direction: String,
    /// Unix-ms timestamp captured at fire time (best-effort; 0
    /// on a pre-1970 clock).
    pub ts_unix_ms: BigInt,
}

impl From<&InnerRpcCallEvent> for RpcCallEventJs {
    fn from(evt: &InnerRpcCallEvent) -> Self {
        let (status_kind, status_message) = match &evt.status {
            InnerRpcCallStatus::Ok => ("ok", None),
            InnerRpcCallStatus::Error(msg) => ("error", Some(msg.clone())),
            InnerRpcCallStatus::Timeout => ("timeout", None),
            InnerRpcCallStatus::Canceled => ("canceled", None),
        };
        let direction = match evt.direction {
            InnerRpcDirection::Outbound => "outbound",
            InnerRpcDirection::Inbound => "inbound",
        };
        Self {
            caller: BigInt::from(evt.caller),
            callee: BigInt::from(evt.callee),
            method: evt.method.clone(),
            latency_ms: evt.latency_ms,
            status_kind: status_kind.to_string(),
            status_message,
            request_bytes: evt.request_bytes,
            response_bytes: evt.response_bytes,
            direction: direction.to_string(),
            ts_unix_ms: BigInt::from(evt.ts_unix_ms),
        }
    }
}

/// Per-service caller- + server-side nRPC counters at a point
/// in time. Element of [`RpcMetricsSnapshotJs::services`].
#[napi(object)]
pub struct ServiceMetricsJs {
    pub service: String,
    // ---- caller-side ----
    pub calls_total: BigInt,
    pub errors_no_route: BigInt,
    pub errors_timeout: BigInt,
    pub errors_server: BigInt,
    pub errors_transport: BigInt,
    pub in_flight: i64,
    pub latency_sum_ns: BigInt,
    pub latency_count: BigInt,
    /// Cumulative bucket counts; index `i` corresponds to
    /// `DEFAULT_LATENCY_BUCKETS_SECS[i]` in the substrate. Last
    /// entry is the `+Inf` bucket.
    pub latency_buckets: Vec<BigInt>,
    // ---- server-side ----
    pub handler_invocations_total: BigInt,
    pub handler_panics_total: BigInt,
    pub handler_in_flight: i64,
    pub handler_duration_sum_ns: BigInt,
    pub handler_duration_count: BigInt,
    pub handler_duration_buckets: Vec<BigInt>,
    pub streaming_chunks_emitted_total: BigInt,
    pub streaming_chunks_dropped_total: BigInt,
    pub capability_denied_total: BigInt,
}

impl From<&InnerServiceMetrics> for ServiceMetricsJs {
    fn from(m: &InnerServiceMetrics) -> Self {
        Self {
            service: m.service.clone(),
            calls_total: BigInt::from(m.calls_total),
            errors_no_route: BigInt::from(m.errors_no_route),
            errors_timeout: BigInt::from(m.errors_timeout),
            errors_server: BigInt::from(m.errors_server),
            errors_transport: BigInt::from(m.errors_transport),
            in_flight: m.in_flight,
            latency_sum_ns: BigInt::from(m.latency_sum_ns),
            latency_count: BigInt::from(m.latency_count),
            latency_buckets: m
                .latency_buckets
                .iter()
                .copied()
                .map(BigInt::from)
                .collect(),
            handler_invocations_total: BigInt::from(m.handler_invocations_total),
            handler_panics_total: BigInt::from(m.handler_panics_total),
            handler_in_flight: m.handler_in_flight,
            handler_duration_sum_ns: BigInt::from(m.handler_duration_sum_ns),
            handler_duration_count: BigInt::from(m.handler_duration_count),
            handler_duration_buckets: m
                .handler_duration_buckets
                .iter()
                .copied()
                .map(BigInt::from)
                .collect(),
            streaming_chunks_emitted_total: BigInt::from(m.streaming_chunks_emitted_total),
            streaming_chunks_dropped_total: BigInt::from(m.streaming_chunks_dropped_total),
            capability_denied_total: BigInt::from(m.capability_denied_total),
        }
    }
}

/// Snapshot of the per-service nRPC metrics registry. Returned
/// by [`MeshRpc::metrics_snapshot`].
#[napi(object)]
pub struct RpcMetricsSnapshotJs {
    /// One entry per service that has been called at least once
    /// since the mesh was created. Sorted by service name.
    pub services: Vec<ServiceMetricsJs>,
    /// Cumulative count of observer events dropped because the
    /// observer's bounded buffer was full at the time the
    /// substrate dispatch path fired (v3 / O-A1). A non-zero,
    /// climbing value indicates the installed observer can't
    /// keep up with the dispatch rate — the operator should
    /// either lighten the per-event callback work or push events
    /// into their own queue and drain them off a dedicated thread.
    pub observer_dropped_total: BigInt,
}

impl RpcMetricsSnapshotJs {
    /// Build the napi snapshot. Combines the substrate's per-
    /// service registry with the napi-local observer drop
    /// counter (which is a per-process counter; the napi binding
    /// doesn't currently split it by service).
    fn build(snapshot: &InnerRpcMetricsSnapshot) -> Self {
        Self {
            services: snapshot
                .services
                .iter()
                .map(ServiceMetricsJs::from)
                .collect(),
            observer_dropped_total: BigInt::from(
                OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed),
            ),
        }
    }
}

// ----------------------------------------------------------------------
// Observer trampoline — bounded mpsc + dedicated worker (v3 / O-A1).
//
// The substrate calls `RpcObserver::on_call` synchronously from
// the dispatch task. v1 (S2-A1) called the TSFN directly in
// NonBlocking mode from that thread; v3 inserts a bounded mpsc +
// worker task so the substrate dispatch thread pays only one
// atomic counter on overflow instead of paying the TSFN's
// internal Mutex acquire on every event.
//
// Design:
//   - On install: spawn a worker task that drains the receiver
//     and pumps each event to the TSFN.
//   - `on_call`: `try_send` into the channel; overflow increments
//     the process-global `OBSERVER_DROPPED_TOTAL`. No allocation,
//     no lock, no system call on the hot path.
//   - On clear (or mesh drop): drop the sender → worker task
//     exits cleanly when the channel closes.
//
// Locked decision #1 (NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md):
// callbacks should still be cheap, but the substrate dispatch
// path is now defended against a slow consumer — overflow drops
// surface via the snapshot's `observer_dropped_total` counter.
// ----------------------------------------------------------------------

/// Bound on the observer event buffer. Big enough that a
/// momentarily-slow observer doesn't lose events under normal
/// load; small enough that an actually-broken observer surfaces
/// drops within seconds rather than minutes. Matches the existing
/// `RpcResponseSink`'s pump-side mpsc bound in the substrate.
const OBSERVER_BUFFER_CAPACITY: usize = 1024;

/// Process-global count of observer events dropped because the
/// bounded buffer was full. Surfaced via
/// [`RpcMetricsSnapshotJs::observer_dropped_total`]. Per-process
/// (not per-mesh / per-service) because observer dispatch is
/// fundamentally per-process; the napi binding's single
/// dispatcher reaches every mesh in the V8 instance.
static OBSERVER_DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

type RpcObserverTsfn = ThreadsafeFunction<RpcCallEventJs, (), RpcCallEventJs, napi::Status, false>;

struct NodeRpcObserver {
    sender: tokio::sync::mpsc::Sender<RpcCallEventJs>,
}

impl NodeRpcObserver {
    /// Install a new observer: build the bounded channel, spawn
    /// the drain worker, return the wrapping observer.
    fn install(tsfn: RpcObserverTsfn) -> Self {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<RpcCallEventJs>(OBSERVER_BUFFER_CAPACITY);
        tokio::spawn(async move {
            while let Some(evt) = receiver.recv().await {
                // NonBlocking on the TSFN side too — if the JS
                // event loop is wedged AND the napi-rs queue is
                // also full, we let napi-rs drop. Our own bounded
                // buffer is the first line of defense; the TSFN
                // overflow path is the last-ditch fallback.
                let _ = tsfn.call(evt, ThreadsafeFunctionCallMode::NonBlocking);
            }
            // Sender dropped → channel closed → worker exits.
        });
        Self { sender }
    }
}

impl RpcObserver for NodeRpcObserver {
    fn on_call(&self, evt: InnerRpcCallEvent) {
        let js_evt = RpcCallEventJs::from(&evt);
        // try_send is non-blocking; full or closed → drop +
        // counter increment. Closed shouldn't happen in normal
        // operation (we hold the sender until set_observer(None)
        // or mesh drop), but it's harmless if it does.
        if self.sender.try_send(js_evt).is_err() {
            OBSERVER_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
        }
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
        let mut guard = self.inner.lock();
        let _ = guard.take();
    }

    /// `true` once `close()` has been called (or after V8 GC
    /// finalized the handle). Useful for tests / diagnostics.
    #[napi]
    pub fn is_closed(&self) -> bool {
        let guard = self.inner.lock();
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
    ///
    /// Note on contention: `next()` holds the lock for the
    /// duration of an in-flight chunk wait, which serializes any
    /// `grant`/`close` issued from a separate JS task against
    /// it. `flow_controlled` is short-circuited via a cached
    /// snapshot below so at least *that* method is lock-free.
    /// Improving `grant` requires SDK plumbing to expose a
    /// control handle independent of the polling future — out of
    /// scope for the binding alone.
    inner: Arc<tokio::sync::Mutex<Option<InnerRpcStream>>>,
    /// Cached at construction time so `flow_controlled()` doesn't
    /// take the mutex (and thus doesn't block on an in-flight
    /// `next()`). The underlying flow-control mode is fixed at
    /// stream creation, so the cache never goes stale.
    flow_controlled_cached: bool,
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
    ///
    /// **Contention note:** this currently serializes against an
    /// in-flight `next()` because the SDK's `RpcStream` doesn't
    /// expose a separable control handle. If you need to grant
    /// while a `next()` is parked, either drain `next()` first or
    /// rely on auto-grant (1 credit per delivered chunk) which
    /// keeps the server's credit at roughly the initial window.
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
    ///
    /// Lock-free: the underlying flow-control mode is fixed at
    /// stream creation, so the value is captured then and read
    /// without taking the inner mutex.
    #[napi]
    pub async fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
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
// ABI 0x0002 — Client-streaming caller-side (Phase B9-1)
//
// Same Arc<TokioMutex<Option<...>>> pattern as `RpcStream`. send /
// finish hold the lock across the await; finish takes the inner
// permanently (consumes the call). close releases without
// emitting REQUEST_END — used when callers want to cancel
// without finishing.
// ============================================================================

/// Open client-streaming RPC call. Push chunks via `send`, then
/// `finish` to await the terminal response. Drop / `close` fires
/// CANCEL via the SDK's `ClientStreamCallRaw::Drop` if `finish`
/// wasn't reached.
#[napi]
pub struct ClientStreamCall {
    inner: Arc<tokio::sync::Mutex<Option<InnerClientStreamCallRaw>>>,
    /// Captured at construction so `callId()` doesn't take the
    /// mutex (which `send` / `finish` may be holding across an
    /// await).
    call_id_cached: u64,
    /// Cached `flow_controlled` flag for the same reason.
    flow_controlled_cached: bool,
    /// Lets `close()` interrupt a pending `send()` that's
    /// awaiting flow-control credit. send `select!`s on this; a
    /// concurrent close fires it, the select picks the close arm,
    /// the call is dropped (CANCEL fires from Drop), and the
    /// pending send returns `stream_closed` instead of hanging
    /// forever on credit that will never arrive.
    close_notify: Arc<tokio::sync::Notify>,
}

#[napi]
impl ClientStreamCall {
    /// Push one body chunk to the server. Encodes as the initial
    /// REQUEST (first call) or as a REQUEST_CHUNK (subsequent).
    /// Awaits one credit when flow control was opted in.
    ///
    /// Concurrent `close()` interrupts the await: `send`
    /// `select!`s on the close-notify, so the upload doesn't
    /// queue behind credit that never arrives.
    #[napi]
    pub async fn send(&self, body: Buffer) -> Result<()> {
        let body = napi_buffer_to_bytes(body);
        // Take the inner out under a brief lock so the long-lived
        // tokio mutex isn't held across the credit await; a
        // racing `close()` can then acquire the lock cleanly to
        // signal cancellation.
        let mut call = {
            let mut guard = self.inner.lock().await;
            guard
                .take()
                .ok_or_else(|| nrpc_err("stream_closed", "client-stream call already closed"))?
        };
        let notify = self.close_notify.clone();
        let result = tokio::select! {
            r = call.send(body) => r,
            _ = notify.notified() => {
                // close() fired. Drop the call (which drops its
                // credit semaphore + fires CANCEL via the SDK's
                // Drop impl) and report stream_closed to JS.
                drop(call);
                return Err(nrpc_err("stream_closed", "send aborted by close()"));
            }
        };
        let mut guard = self.inner.lock().await;
        match result {
            Ok(()) => {
                *guard = Some(call);
                Ok(())
            }
            Err(e) => {
                drop(call);
                Err(nrpc_err_from_inner(e))
            }
        }
    }

    /// Close the upload direction (emit REQUEST_END) and await
    /// the server's terminal response. Consumes the call —
    /// subsequent `send` / `finish` throw `stream_closed`.
    #[napi]
    pub async fn finish(&self) -> Result<Buffer> {
        let mut guard = self.inner.lock().await;
        let call = guard
            .take()
            .ok_or_else(|| nrpc_err("stream_closed", "client-stream call already closed"))?;
        match call.finish().await {
            Ok(reply) => Ok(Buffer::from(reply.body.as_ref())),
            Err(e) => Err(nrpc_err_from_inner(e)),
        }
    }

    /// Server-assigned `call_id` for diagnostics / trace
    /// correlation.
    #[napi]
    pub async fn call_id(&self) -> BigInt {
        BigInt::from(self.call_id_cached)
    }

    /// `true` if the call was opened with a non-`None`
    /// `requestWindowInitial`.
    #[napi]
    pub async fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }

    /// Close without finishing. Fires CANCEL via the SDK's Drop
    /// if the initial REQUEST has already flown. Idempotent.
    ///
    /// Concurrent in-flight `send()` calls awaiting credit are
    /// interrupted via the close-notify — they observe
    /// `stream_closed` instead of hanging.
    #[napi]
    pub async fn close(&self) {
        // Wake any in-flight `send` waiting on credit. notify_one
        // stores a permit so subsequent `notified()` consumes it
        // immediately — fine even if send hasn't started yet.
        self.close_notify.notify_one();
        let _ = self.inner.lock().await.take();
    }
}

// ============================================================================
// ABI 0x0002 — Duplex caller-side (Phase B9-1)
//
// Three classes:
//   - DuplexCall: combined send + receive surface.
//   - DuplexSink + DuplexStream: independent halves after `intoSplit()`.
//
// All three share the same Arc<TokioMutex<Option<...>>> pattern.
// CANCEL semantics are inherited from the SDK's Arc<DuplexInner>:
// fires only when BOTH halves drop without the response stream's
// terminal frame.
// ============================================================================

/// Open duplex RPC call. Combined send + receive surface. Use
/// `intoSplit()` to get independent `DuplexSink` + `DuplexStream`
/// halves for the encoder-task / decoder-task pattern.
#[napi]
pub struct DuplexCall {
    inner: Arc<tokio::sync::Mutex<Option<InnerDuplexCallRaw>>>,
    call_id_cached: u64,
    flow_controlled_cached: bool,
    /// Same role as `ClientStreamCall::close_notify` — lets
    /// `close()` interrupt a pending `send()` blocked on credit.
    close_notify: Arc<tokio::sync::Notify>,
}

#[napi]
impl DuplexCall {
    /// Push one body chunk to the server.
    ///
    /// Concurrent `close()` interrupts the await via the
    /// close-notify (same shape as `ClientStreamCall::send`),
    /// so a stuck flow-control await doesn't pin the call.
    #[napi]
    pub async fn send(&self, body: Buffer) -> Result<()> {
        let body = napi_buffer_to_bytes(body);
        let mut call = {
            let mut guard = self.inner.lock().await;
            guard
                .take()
                .ok_or_else(|| nrpc_err("stream_closed", "duplex call already closed"))?
        };
        let notify = self.close_notify.clone();
        let result = tokio::select! {
            r = call.send(body) => r,
            _ = notify.notified() => {
                drop(call);
                return Err(nrpc_err("stream_closed", "send aborted by close()"));
            }
        };
        let mut guard = self.inner.lock().await;
        match result {
            Ok(()) => {
                *guard = Some(call);
                Ok(())
            }
            Err(e) => {
                drop(call);
                Err(nrpc_err_from_inner(e))
            }
        }
    }

    /// Close the upload direction (emit REQUEST_END). Does NOT
    /// close the response stream — the caller drains it via
    /// `next()` until terminal End.
    #[napi(js_name = "finishSending")]
    pub async fn finish_sending(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        let call = guard
            .as_mut()
            .ok_or_else(|| nrpc_err("stream_closed", "duplex call already closed"))?;
        call.finish_sending().await.map_err(nrpc_err_from_inner)
    }

    /// Pull the next inbound response chunk. Returns `null` on
    /// clean EOF. Throws on terminal non-Ok status.
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut guard = self.inner.lock().await;
        let call = guard
            .as_mut()
            .ok_or_else(|| nrpc_err("stream_closed", "duplex call already closed"))?;
        match call.next().await {
            Some(Ok(bytes)) => Ok(Some(Buffer::from(bytes.as_ref()))),
            Some(Err(e)) => Err(nrpc_err_from_inner(e)),
            None => {
                let _ = guard.take();
                Ok(None)
            }
        }
    }

    /// Split the call into independent send + receive halves.
    /// Returns `[sink, stream]` — JS destructures as
    /// `const [sink, stream] = await call.intoSplit()`.
    ///
    /// After `intoSplit` returns, this `DuplexCall` is "done" —
    /// subsequent `send` / `finishSending` / `next` throw.
    /// CANCEL fires only when BOTH split halves drop without
    /// observing the response stream's terminal frame.
    ///
    /// Returned as a tuple because `#[napi(object)]` (the POD
    /// wrapper) requires `FromNapiValue` fields, and `#[napi]`
    /// classes don't implement it. Tuples surface as JS arrays
    /// directly via napi-rs.
    #[napi(js_name = "intoSplit")]
    pub async fn split(&self) -> Result<(DuplexSink, DuplexStream)> {
        let mut guard = self.inner.lock().await;
        let call = guard
            .take()
            .ok_or_else(|| nrpc_err("stream_closed", "duplex call already closed"))?;
        let call_id = call.call_id();
        let flow_controlled = call.flow_controlled();
        let (sink, stream) = call.into_split();
        Ok((
            DuplexSink {
                inner: Arc::new(tokio::sync::Mutex::new(Some(sink))),
                call_id_cached: call_id,
                flow_controlled_cached: flow_controlled,
                close_notify: Arc::new(tokio::sync::Notify::new()),
            },
            DuplexStream {
                inner: Arc::new(tokio::sync::Mutex::new(Some(stream))),
                call_id_cached: call_id,
            },
        ))
    }

    /// Server-assigned `call_id` for diagnostics.
    #[napi]
    pub async fn call_id(&self) -> BigInt {
        BigInt::from(self.call_id_cached)
    }

    /// `true` if the call was opened with a non-`None`
    /// `requestWindowInitial`. Reports the UPLOAD-direction
    /// flow-control state.
    #[napi]
    pub async fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }

    /// Close without observing the response terminator. Fires
    /// CANCEL via the SDK's Drop. Idempotent. Concurrent
    /// in-flight `send()` waiting on credit is interrupted via
    /// the close-notify.
    #[napi]
    pub async fn close(&self) {
        self.close_notify.notify_one();
        let _ = self.inner.lock().await.take();
    }
}

/// Send-half of a `DuplexCall` after `intoSplit`.
#[napi]
pub struct DuplexSink {
    inner: Arc<tokio::sync::Mutex<Option<InnerDuplexSink>>>,
    call_id_cached: u64,
    flow_controlled_cached: bool,
    /// Same role as `ClientStreamCall::close_notify` —
    /// interrupts a pending `send()` blocked on credit.
    close_notify: Arc<tokio::sync::Notify>,
}

#[napi]
impl DuplexSink {
    /// Push one body chunk to the server.
    ///
    /// Concurrent `close()` interrupts the await via the
    /// close-notify (same shape as `DuplexCall::send`).
    #[napi]
    pub async fn send(&self, body: Buffer) -> Result<()> {
        let body = napi_buffer_to_bytes(body);
        let mut sink = {
            let mut guard = self.inner.lock().await;
            guard
                .take()
                .ok_or_else(|| nrpc_err("stream_closed", "duplex sink already closed"))?
        };
        let notify = self.close_notify.clone();
        let result = tokio::select! {
            r = sink.send(body) => r,
            _ = notify.notified() => {
                drop(sink);
                return Err(nrpc_err("stream_closed", "send aborted by close()"));
            }
        };
        let mut guard = self.inner.lock().await;
        match result {
            Ok(()) => {
                *guard = Some(sink);
                Ok(())
            }
            Err(e) => {
                drop(sink);
                Err(nrpc_err_from_inner(e))
            }
        }
    }

    /// Close the upload direction (emit REQUEST_END). Consumes
    /// the sink — subsequent `send` throws.
    #[napi]
    pub async fn finish(&self) -> Result<()> {
        let mut guard = self.inner.lock().await;
        let sink = guard
            .take()
            .ok_or_else(|| nrpc_err("stream_closed", "duplex sink already closed"))?;
        sink.finish_sending().await.map_err(nrpc_err_from_inner)
    }

    /// Server-assigned `call_id`.
    #[napi]
    pub async fn call_id(&self) -> BigInt {
        BigInt::from(self.call_id_cached)
    }

    /// `true` if the call was opened with a non-`None`
    /// `requestWindowInitial`.
    #[napi]
    pub async fn flow_controlled(&self) -> bool {
        self.flow_controlled_cached
    }

    /// Close without emitting REQUEST_END. Idempotent. Concurrent
    /// in-flight `send()` waiting on credit is interrupted via
    /// the close-notify.
    #[napi]
    pub async fn close(&self) {
        self.close_notify.notify_one();
        let _ = self.inner.lock().await.take();
    }
}

/// Receive-half of a `DuplexCall` after `intoSplit`.
#[napi]
pub struct DuplexStream {
    inner: Arc<tokio::sync::Mutex<Option<InnerDuplexStream>>>,
    call_id_cached: u64,
}

#[napi]
impl DuplexStream {
    /// Pull the next response chunk. Returns `null` on clean EOF.
    /// Throws on terminal non-Ok status.
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut guard = self.inner.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| nrpc_err("stream_closed", "duplex stream already closed"))?;
        use futures::StreamExt;
        match stream.next().await {
            Some(Ok(bytes)) => Ok(Some(Buffer::from(bytes.as_ref()))),
            Some(Err(e)) => Err(nrpc_err_from_inner(e)),
            None => {
                let _ = guard.take();
                Ok(None)
            }
        }
    }

    /// Server-assigned `call_id`.
    #[napi]
    pub async fn call_id(&self) -> BigInt {
        BigInt::from(self.call_id_cached)
    }

    /// Close the stream. Idempotent.
    #[napi]
    pub async fn close(&self) {
        let _ = self.inner.lock().await.take();
    }
}

// ============================================================================
// ABI 0x0002 — Server-side handler primitives (Phase B9-2)
//
// JsRequestStream wraps the SDK's RequestStream and is handed
// to JS handlers as the inbound chunk source. JsResponseSink
// wraps RpcResponseSink for duplex handlers' outbound side.
// Both are napi classes whose JS instances live for the
// duration of the handler callback.
//
// JS handler signatures (idiomatic patterns):
//
//   // Client-streaming
//   await mesh.serveClientStream("svc", async (stream) => {
//     let total = 0;
//     while (true) {
//       const chunk = await stream.next();
//       if (chunk === null) break;
//       total += chunk.length;
//     }
//     return Buffer.from([total]);
//   });
//
//   // Duplex
//   await mesh.serveDuplex("svc", async (stream, sink) => {
//     while (true) {
//       const chunk = await stream.next();
//       if (chunk === null) break;
//       sink.send(Buffer.concat([Buffer.from("echo:"), chunk]));
//     }
//   });
//
// A thin .d.ts/JS shim can add Symbol.asyncIterator on top so
// `for await (const c of stream)` works — pure-JS layer, not
// in scope here.
// ============================================================================

/// Inbound request-stream handle for client-streaming + duplex
/// server handlers. Drain via `await stream.next()` until it
/// returns `null` (REQUEST_END or CANCEL closed the stream).
///
/// Lifetime: bounded by the handler callback. The SDK's
/// underlying `RequestStream` is taken into this wrapper at
/// handler dispatch and dropped when the wrapper is dropped
/// (which happens when JS releases its reference to the instance,
/// typically right after the handler returns).
#[napi]
pub struct JsRequestStream {
    inner: Arc<tokio::sync::Mutex<Option<InnerRequestStream>>>,
    /// Caller's identity hash (peer origin). Surfaced to JS
    /// via the `callerOrigin` getter; 0 on the loopback / no-peer
    /// fast path.
    caller_origin: u64,
    /// Per-call id (mints from the substrate). JS handlers may
    /// thread it into per-call logging / tracing.
    call_id: u64,
    /// Caller's declared deadline as a Unix-nanos absolute
    /// timestamp. `0` means "no deadline declared".
    deadline_ns: u64,
    /// Initial-REQUEST headers, name/value pairs. Names are
    /// lowercase per the substrate convention. Empty when the
    /// REQUEST carried no application headers.
    headers: Arc<Vec<(String, Vec<u8>)>>,
}

#[napi]
impl JsRequestStream {
    /// Caller's peer origin hash. Surfaced as a bigint to JS so
    /// the full u64 round-trips. `0n` on the loopback fast path
    /// where there's no remote peer.
    #[napi(getter)]
    pub fn caller_origin(&self) -> BigInt {
        BigInt::from(self.caller_origin)
    }

    /// The call_id minted by the substrate for this call. Stable
    /// across the call's lifetime; useful for handler-side logging
    /// and trace correlation.
    #[napi(getter)]
    pub fn call_id(&self) -> BigInt {
        BigInt::from(self.call_id)
    }

    /// Caller's declared deadline as a Unix-nanoseconds absolute
    /// timestamp. `0n` means no deadline was declared; otherwise
    /// the handler MAY use it to short-circuit slow processing
    /// past the wire deadline.
    #[napi(getter)]
    pub fn deadline_ns(&self) -> BigInt {
        BigInt::from(self.deadline_ns)
    }

    /// Initial-REQUEST headers carried by the caller. Each entry
    /// is `[name, value]` with `name` lowercase and `value` as a
    /// raw Buffer (per the substrate's bytes contract — values
    /// are not required to be UTF-8). Empty array if the REQUEST
    /// carried no headers.
    #[napi(getter)]
    pub fn headers(&self) -> Vec<(String, Buffer)> {
        self.headers
            .iter()
            .map(|(n, v)| (n.clone(), Buffer::from(v.as_slice())))
            .collect()
    }

    /// Pull the next inbound chunk. Returns `null` on EOF
    /// (REQUEST_END / CANCEL). Multiple `next()` calls in
    /// parallel from the same JS task serialize through the
    /// inner mutex.
    ///
    /// **Ordering under `Promise.all`.** Issuing
    /// `Promise.all([s.next(), s.next(), s.next()])` is legal
    /// but the order in which the three promises resolve is
    /// **nondeterministic** — they race on the inner mutex.
    /// Use sequential `await` (e.g. `while ((b = await s.next()))`)
    /// when chunk order matters. The common case (single
    /// consumer awaiting one chunk at a time) is always in
    /// order.
    #[napi]
    pub async fn next(&self) -> Result<Option<Buffer>> {
        let mut guard = self.inner.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| nrpc_err("stream_closed", "request stream already closed"))?;
        use futures::StreamExt;
        match stream.next().await {
            Some(bytes) => Ok(Some(Buffer::from(bytes.as_ref()))),
            None => {
                let _ = guard.take();
                Ok(None)
            }
        }
    }
}

/// Outbound response sink for duplex server handlers. Emit
/// chunks via `sink.send(buffer)`. Non-blocking (SDK try_send
/// under the hood); drops the chunk on overflow rather than
/// blocking the handler. Same lifetime contract as
/// [`JsRequestStream`].
#[napi]
pub struct JsResponseSink {
    inner: Arc<Mutex<Option<InnerRpcResponseSink>>>,
}

#[napi]
impl JsResponseSink {
    /// Emit one response chunk. Returns `true` on success.
    /// Returns `false` if the sink has been closed (handler
    /// raced with the substrate fold's terminal-frame emission).
    ///
    /// **Flow control.** This call is non-blocking — it `try_send`s
    /// into a bounded 1024-chunk mpsc that feeds the response pump.
    /// The pump itself awaits per-call credit before publishing to
    /// the wire (the `stream_window_initial` opt-in). If the pump
    /// stalls on credit, the mpsc fills, and excess chunks are
    /// dropped (counted via `streaming_chunks_dropped_total`). To
    /// honor flow control, JS handlers should pace their emits via
    /// the protocol's REQUEST_GRANT cadence rather than burst-
    /// pushing past the credit window. This mirrors the Rust SDK's
    /// `ResponseSinkTyped::send` contract — both are non-async.
    #[napi]
    pub fn send(&self, body: Buffer) -> bool {
        let guard = self.inner.lock();
        match guard.as_ref() {
            Some(sink) => {
                sink.send(napi_buffer_to_bytes(body));
                true
            }
            None => false,
        }
    }
}

// ============================================================================
// Server-side TSFN bridges (Phase B9-2).
//
// Two TSFNs — one per shape. Both pass the napi class instances
// as the arg; napi-rs's ToNapiValue impl (generated by #[napi]
// for the class) constructs the JS wrapper on the JS thread.
// ============================================================================

/// TSFN for client-streaming handlers. JS side:
/// `(stream: JsRequestStream) => Promise<Buffer>`.
type ClientStreamingHandlerTsfn =
    ThreadsafeFunction<JsRequestStream, Promise<Buffer>, JsRequestStream, napi::Status, false>;

/// Internal wrapper struct ferried through the duplex TSFN.
/// napi(object) requires its fields to be FromNapiValue, which
/// #[napi] classes don't directly implement — so we register a
/// pair of helper impls below that construct a JS array
/// `[stream, sink]` on the JS thread and the handler
/// destructures `(stream, sink) => ...`.
pub struct DuplexHandlerArgs {
    stream: JsRequestStream,
    sink: JsResponseSink,
}

impl ToNapiValue for DuplexHandlerArgs {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        // Build a JS array [stream, sink]. JS handler destructures
        // via `(args) => { const [stream, sink] = args; ... }`.
        // Manual array construction gives us full control over
        // the per-element ToNapiValue invocation for the napi
        // class instances.
        let env_wrapper = napi::Env::from_raw(env);
        let mut arr = env_wrapper.create_array(2)?;
        let stream_val = unsafe { JsRequestStream::to_napi_value(env, val.stream)? };
        let sink_val = unsafe { JsResponseSink::to_napi_value(env, val.sink)? };
        let stream_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, stream_val)? };
        let sink_unknown =
            unsafe { napi::bindgen_prelude::Unknown::from_napi_value(env, sink_val)? };
        arr.set(0, stream_unknown)?;
        arr.set(1, sink_unknown)?;
        unsafe { napi::bindgen_prelude::Array::to_napi_value(env, arr) }
    }
}

/// TSFN for duplex handlers. JS side:
/// `(args: [JsRequestStream, JsResponseSink]) => Promise<Buffer>`.
/// JS code unpacks via `(args) => { const [stream, sink] = args; ... }`
/// and returns `Buffer.alloc(0)` (or any Buffer — value is
/// ignored; the Promise resolving is the "handler done" signal).
type DuplexHandlerTsfn =
    ThreadsafeFunction<DuplexHandlerArgs, Promise<Buffer>, DuplexHandlerArgs, napi::Status, false>;

/// `RpcClientStreamingHandler` impl bridging to JS via TSFN.
struct NodeClientStreamingRpcHandler {
    tsfn: ClientStreamingHandlerTsfn,
    timeout: Duration,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for NodeClientStreamingRpcHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: InnerRequestStream,
    ) -> std::result::Result<RpcResponsePayload, RpcHandlerError> {
        let stream_handle = JsRequestStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(requests))),
            caller_origin: ctx.caller_origin,
            call_id: ctx.call_id,
            deadline_ns: ctx.deadline_ns,
            headers: Arc::new(ctx.headers),
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
        let status = self.tsfn.call_with_return_value(
            stream_handle,
            ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: napi::Result<Promise<Buffer>>, _env| {
                let _ = tx.send(ret);
                napi::Result::Ok(())
            },
        );
        if status != napi::Status::Ok {
            return Err(RpcHandlerError::Internal(format!(
                "TSFN enqueue failed: {status:?}"
            )));
        }
        // Single deadline spanning both phases: TSFN dispatch
        // (rx.await yields the JS Promise object) AND promise
        // resolution (`promise.await` yields the response buffer).
        // Previously only the first phase was bounded — a hung JS
        // handler that returned a Promise that never resolved
        // would pin the Rust task for the full call lifetime.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let promise = match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(Ok(p))) => p,
            Ok(Ok(Err(e))) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS client-streaming handler threw synchronously: {e}"
                )))
            }
            Ok(Err(_)) => {
                return Err(RpcHandlerError::Internal(
                    "JS client-streaming callback channel disconnected".into(),
                ))
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS client-streaming handler did not dispatch within {} ms",
                    self.timeout.as_millis()
                )))
            }
        };
        let resp_buf = match tokio::time::timeout_at(deadline, promise).await {
            Ok(Ok(buf)) => buf,
            Ok(Err(e)) => {
                let msg = e.to_string();
                if let Some((code, body)) = parse_js_app_error(&msg) {
                    return Err(RpcHandlerError::Application {
                        code,
                        message: body,
                    });
                }
                return Err(RpcHandlerError::Internal(format!(
                    "JS client-streaming handler promise rejected: {e}"
                )));
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS client-streaming handler did not resolve within {} ms",
                    self.timeout.as_millis()
                )))
            }
        };
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: resp_buf.to_vec().into(),
        })
    }
}

/// `RpcDuplexHandler` impl bridging to JS via TSFN.
struct NodeDuplexRpcHandler {
    tsfn: DuplexHandlerTsfn,
    timeout: Duration,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for NodeDuplexRpcHandler {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: InnerRequestStream,
        responses: InnerRpcResponseSink,
    ) -> std::result::Result<(), RpcHandlerError> {
        let args = DuplexHandlerArgs {
            stream: JsRequestStream {
                inner: Arc::new(tokio::sync::Mutex::new(Some(requests))),
                caller_origin: ctx.caller_origin,
                call_id: ctx.call_id,
                deadline_ns: ctx.deadline_ns,
                headers: Arc::new(ctx.headers),
            },
            sink: JsResponseSink {
                inner: Arc::new(Mutex::new(Some(responses))),
            },
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<Buffer>>>();
        let status = self.tsfn.call_with_return_value(
            args,
            ThreadsafeFunctionCallMode::NonBlocking,
            move |ret: napi::Result<Promise<Buffer>>, _env| {
                let _ = tx.send(ret);
                napi::Result::Ok(())
            },
        );
        if status != napi::Status::Ok {
            return Err(RpcHandlerError::Internal(format!(
                "TSFN enqueue failed: {status:?}"
            )));
        }
        // Single deadline spans both TSFN dispatch and Promise
        // resolution — see the equivalent comment in the
        // client-streaming bridge.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let promise = match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(Ok(p))) => p,
            Ok(Ok(Err(e))) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS duplex handler threw synchronously: {e}"
                )))
            }
            Ok(Err(_)) => {
                return Err(RpcHandlerError::Internal(
                    "JS duplex callback channel disconnected".into(),
                ))
            }
            Err(_) => {
                return Err(RpcHandlerError::Internal(format!(
                    "JS duplex handler did not dispatch within {} ms",
                    self.timeout.as_millis()
                )))
            }
        };
        match tokio::time::timeout_at(deadline, promise).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => {
                let msg: String = format!("{e}");
                if let Some((code, body)) = parse_js_app_error(&msg) {
                    return Err(RpcHandlerError::Application {
                        code,
                        message: body,
                    });
                }
                Err(RpcHandlerError::Internal(format!(
                    "JS duplex handler promise rejected: {msg}"
                )))
            }
            Err(_) => Err(RpcHandlerError::Internal(format!(
                "JS duplex handler did not resolve within {} ms",
                self.timeout.as_millis()
            ))),
        }
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
    ///
    /// `handlerTimeoutMs` caps the per-call wait for the JS
    /// handler — defaults to 60 000 (60s). A wedged handler past
    /// the cap surfaces to the caller as `RpcStatus::Internal`
    /// "JS handler did not respond within N ms" so the in-flight
    /// slot doesn't leak. Pass 0 to disable the cap (not
    /// recommended — a stuck handler holds a runtime worker
    /// indefinitely).
    #[napi]
    pub fn serve(
        &self,
        service: String,
        handler: Function<'_, Buffer, Promise<Buffer>>,
        handler_timeout_ms: Option<u32>,
    ) -> Result<ServeHandle> {
        let tsfn: RpcHandlerTsfn = handler.build_threadsafe_function().build()?;
        let timeout = match handler_timeout_ms {
            Some(0) => Duration::from_secs(u64::MAX / 1000),
            Some(ms) => Duration::from_millis(ms as u64),
            None => DEFAULT_HANDLER_TIMEOUT,
        };
        let rust_handler = Arc::new(NodeRpcHandler { tsfn, timeout });
        let inner = self
            .node
            .serve_rpc(&service, rust_handler)
            .map_err(|e| nrpc_err("serve_failed", e))?;
        Ok(ServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    // ---- call -----------------------------------------------------------

    /// Reserve a fresh cancel token. Pass on a subsequent call
    /// via `opts.cancelToken`; later, call
    /// [`MeshRpc.cancel_call`] from anywhere to abort the in-
    /// flight call. Tokens are monotonically-increasing,
    /// process-global, never reused — an unused reservation is
    /// harmless (the SDK only allocates a registry entry on the
    /// first paired register or cancel).
    ///
    /// Delegates to the substrate's [`MeshNode::reserve_cancel_token`]
    /// (v3 / C-S1).
    #[napi]
    pub fn reserve_cancel_token(&self) -> BigInt {
        BigInt::from(self.node.reserve_cancel_token())
    }

    /// Abort the in-flight call associated with `token`.
    /// Idempotent — no-op if the token was never used, the call
    /// already finished, or `token == 0`. Triggers CANCEL on the
    /// wire via the substrate's per-call-shape Drop guards; the
    /// awaiting `call` / `callService` rejects with
    /// `nrpc:cancelled:`, streaming entries observe EOF on their
    /// next pull.
    ///
    /// Delegates to the substrate's [`MeshNode::cancel`] (v3 /
    /// C-S1) — the napi binding no longer owns a local cancel
    /// registry.
    #[napi]
    pub fn cancel_call(&self, token: BigInt) -> Result<()> {
        let token = crate::common::bigint_u64(token)?;
        self.node.cancel(token);
        Ok(())
    }

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
        let inner_opts = opts.unwrap_or_default().into_inner();
        let req_bytes = Bytes::copy_from_slice(request.as_ref());
        // cancel_token lives on inner_opts; substrate handles cancel
        // uniformly across shapes. RpcError::Cancelled maps to
        // `nrpc:cancelled:` via the error-kind table.
        self.node
            .call(target, &service, req_bytes, inner_opts)
            .await
            .map(|reply| Buffer::from(reply.body.as_ref()))
            .map_err(nrpc_err_from_inner)
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
        let inner_opts = opts.unwrap_or_default().into_inner();
        let req_bytes = Bytes::copy_from_slice(request.as_ref());
        self.node
            .call_service(&service, req_bytes, inner_opts)
            .await
            .map(|reply| Buffer::from(reply.body.as_ref()))
            .map_err(nrpc_err_from_inner)
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
            .call_streaming(
                target,
                &service,
                Bytes::copy_from_slice(request.as_ref()),
                opts,
            )
            .await
            .map_err(nrpc_err_from_inner)?;
        let flow_controlled_cached = inner.flow_controlled();
        Ok(RpcStream {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
            flow_controlled_cached,
        })
    }

    // ---- ABI 0x0002 client-streaming + duplex callers (B9-1) ----

    /// Open a client-streaming call. Push chunks via
    /// `call.send(buf)`, then `call.finish()` to await the
    /// terminal response. The initial REQUEST is published
    /// lazily on the first `send` (or on `finish` for the
    /// degenerate zero-send path).
    #[napi(js_name = "callClientStream")]
    pub async fn call_client_stream(
        &self,
        target_node_id: BigInt,
        service: String,
        opts: Option<CallOptions>,
    ) -> Result<ClientStreamCall> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let opts = opts.unwrap_or_default().into_inner();
        let inner = self
            .node
            .call_client_stream(target, &service, opts)
            .await
            .map_err(nrpc_err_from_inner)?;
        let call_id_cached = inner.call_id();
        let flow_controlled_cached = inner.flow_controlled();
        Ok(ClientStreamCall {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
            call_id_cached,
            flow_controlled_cached,
            close_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Open a duplex call. Both `requestWindowInitial` (upload
    /// flow control) and `streamWindowInitial` (download flow
    /// control) on `CallOptions` are independently opt-in.
    #[napi(js_name = "callDuplex")]
    pub async fn call_duplex(
        &self,
        target_node_id: BigInt,
        service: String,
        opts: Option<CallOptions>,
    ) -> Result<DuplexCall> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let opts = opts.unwrap_or_default().into_inner();
        let inner = self
            .node
            .call_duplex(target, &service, opts)
            .await
            .map_err(nrpc_err_from_inner)?;
        let call_id_cached = inner.call_id();
        let flow_controlled_cached = inner.flow_controlled();
        Ok(DuplexCall {
            inner: Arc::new(tokio::sync::Mutex::new(Some(inner))),
            call_id_cached,
            flow_controlled_cached,
            close_notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    // ---- ABI 0x0002 server-side serves (B9-2) ----

    /// Register a client-streaming handler. JS signature:
    /// `(stream: JsRequestStream) => Promise<Buffer>`.
    ///
    /// Drain inbound chunks via `await stream.next()` (returns
    /// `null` on EOF). Return the terminal response Buffer.
    /// Throw `new Error("nrpc:app_error:0x<code>:<body>")` to
    /// signal a typed Application status (same convention as
    /// `serve`).
    #[napi(js_name = "serveClientStream")]
    pub fn serve_client_stream(
        &self,
        service: String,
        handler: Function<'_, JsRequestStream, Promise<Buffer>>,
    ) -> Result<ServeHandle> {
        let tsfn: ClientStreamingHandlerTsfn = handler.build_threadsafe_function().build()?;
        let inner_handler = Arc::new(NodeClientStreamingRpcHandler {
            tsfn,
            timeout: DEFAULT_HANDLER_TIMEOUT,
        });
        let inner = self
            .node
            .serve_rpc_client_stream(&service, inner_handler)
            .map_err(|e| nrpc_err("serve_failed", format!("{e}")))?;
        Ok(ServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
        })
    }

    /// Register a duplex handler. JS signature:
    /// `(args: [JsRequestStream, JsResponseSink]) => Promise<void>`.
    /// JS destructures the args:
    ///
    ///   ```js
    ///   mesh.serveDuplex("svc", async ([stream, sink]) => {
    ///     while (true) {
    ///       const chunk = await stream.next();
    ///       if (chunk === null) break;
    ///       sink.send(Buffer.concat([Buffer.from("echo:"), chunk]));
    ///     }
    ///   });
    ///   ```
    #[napi(js_name = "serveDuplex")]
    pub fn serve_duplex(
        &self,
        service: String,
        handler: Function<'_, DuplexHandlerArgs, Promise<Buffer>>,
    ) -> Result<ServeHandle> {
        let tsfn: DuplexHandlerTsfn = handler.build_threadsafe_function().build()?;
        let inner_handler = Arc::new(NodeDuplexRpcHandler {
            tsfn,
            timeout: DEFAULT_HANDLER_TIMEOUT,
        });
        let inner = self
            .node
            .serve_rpc_duplex(&service, inner_handler)
            .map_err(|e| nrpc_err("serve_failed", format!("{e}")))?;
        Ok(ServeHandle {
            inner: Arc::new(Mutex::new(Some(inner))),
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
            .map(BigInt::from)
            .collect()
    }

    // ---- observer + metrics (S2-A1) ------------------------------------

    /// Install (pass a function) or clear (pass `null` /
    /// `undefined`) the caller-side nRPC observer. Replaces any
    /// previously-installed observer.
    ///
    /// The callback fires synchronously from the substrate's
    /// dispatch task on every completed outbound RPC. The TSFN
    /// crosses the napi boundary in `NonBlocking` mode — if the
    /// JS event loop is wedged and the queue fills, events are
    /// **dropped**, not buffered. Callbacks must therefore be
    /// cheap: push into a queue or ring buffer for slow
    /// consumers, do not do work inline.
    ///
    /// v1 emits only `direction === "outbound"` events; the
    /// substrate's server-side hook is a planned follow-up.
    #[napi]
    pub fn set_observer(&self, observer: Option<Function<'_, RpcCallEventJs, ()>>) -> Result<()> {
        match observer {
            Some(f) => {
                let tsfn: RpcObserverTsfn = f.build_threadsafe_function().build()?;
                let obs: Arc<dyn RpcObserver> = Arc::new(NodeRpcObserver::install(tsfn));
                self.node.set_rpc_observer(Some(obs));
            }
            None => {
                self.node.set_rpc_observer(None);
            }
        }
        Ok(())
    }

    /// Snapshot the per-service nRPC metrics registry. Cheap —
    /// one DashMap iteration plus one atomic-load for the
    /// process-global observer-drop counter. Safe to call on
    /// every Prometheus scrape. The returned object is a plain
    /// JS POD (BigInts for u64 fields); read fields directly or
    /// feed into your own exporter.
    #[napi]
    pub fn metrics_snapshot(&self) -> RpcMetricsSnapshotJs {
        RpcMetricsSnapshotJs::build(&self.node.rpc_metrics_snapshot())
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
    /// JS-side wrapper at `@net-mesh/core/errors` matches on the
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
                    format!(
                        "{ERR_NRPC_PREFIX}server_error: status=0x{status:04x} message={message}"
                    )
                }
                InnerRpcError::Transport(e) => format!("{ERR_NRPC_PREFIX}transport: {e}"),
                InnerRpcError::Codec { direction, message } => {
                    let dir = match direction {
                        CodecDirection::Encode => "codec_encode",
                        CodecDirection::Decode => "codec_decode",
                    };
                    format!("{ERR_NRPC_PREFIX}{dir}: {message}")
                }
                InnerRpcError::CapabilityDenied { target, capability } => format!(
                    "{ERR_NRPC_PREFIX}capability_denied: target=0x{target:x} capability={capability}"
                ),
                InnerRpcError::Cancelled => format!("{ERR_NRPC_PREFIX}cancelled: call cancelled"),
            }
        };

        assert!(format_kind(InnerRpcError::NoRoute {
            target: 0xABCD,
            reason: "x".into(),
        })
        .starts_with("nrpc:no_route:"));
        assert!(
            format_kind(InnerRpcError::Timeout { elapsed_ms: 100 }).starts_with("nrpc:timeout:")
        );
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
            request_window_initial: None,
            cancel_token: None,
            request_headers: None,
        };
        let inner = opts.into_inner();
        assert!(inner.deadline.is_some(), "deadline must be Some when set");
        assert_eq!(inner.stream_window_initial, Some(8));
        assert!(
            inner.request_headers.is_empty(),
            "no headers expected when None"
        );

        let empty = CallOptions::default().into_inner();
        assert!(empty.deadline.is_none());
        assert!(empty.stream_window_initial.is_none());
        assert!(empty.request_headers.is_empty());
    }

    /// Phase 9b: `request_headers` plumb through to the substrate's
    /// `InnerCallOptions::request_headers`. The dispatch path (in
    /// substrate) appends these to the `RpcRequestPayload.headers`
    /// vector — pinned by the substrate's mesh_rpc_where end-to-end
    /// test. This unit test pins the binding-side encode contract:
    /// JS `[{ name, value: Buffer }, ...]` → Rust `Vec<(String,
    /// Vec<u8>)>` byte-equal.
    #[test]
    fn call_options_request_headers_plumb_through() {
        let opts = CallOptions {
            deadline_ms: None,
            stream_window_initial: None,
            request_window_initial: None,
            cancel_token: None,
            request_headers: Some(vec![
                RpcRequestHeader {
                    name: "net-where".into(),
                    value: Buffer::from(b"json".as_slice()),
                },
                RpcRequestHeader {
                    name: "cyberdeck-x-tenant".into(),
                    value: Buffer::from(b"acme".as_slice()),
                },
            ]),
        };
        let inner = opts.into_inner();
        assert_eq!(inner.request_headers.len(), 2);
        assert_eq!(inner.request_headers[0].0, "net-where");
        assert_eq!(inner.request_headers[0].1, b"json");
        assert_eq!(inner.request_headers[1].0, "cyberdeck-x-tenant");
        assert_eq!(inner.request_headers[1].1, b"acme");
    }

    /// `parse_js_app_error` parses canonical
    /// `nrpc:app_error:0x<code>:<body>` and surfaces the
    /// (code, body) pair the SDK expects for
    /// RpcHandlerError::Application. Pinned because the JS-side
    /// `appError(code, body)` helper produces this format and a
    /// drift would silently break typed bad-request mapping.
    #[test]
    fn parse_js_app_error_round_trips_canonical_format() {
        // Canonical form: `nrpc:app_error:0x8000:<json body>`.
        let (code, body) = parse_js_app_error(
            "nrpc:app_error:0x8000:{\"error\":\"invalid_request\",\"detail\":\"bad json\"}",
        )
        .expect("canonical form parses");
        assert_eq!(code, 0x8000);
        assert_eq!(
            body,
            "{\"error\":\"invalid_request\",\"detail\":\"bad json\"}"
        );

        // Body containing colons is preserved verbatim — the
        // parser splits only on the first colon AFTER the code.
        let (code, body) =
            parse_js_app_error("nrpc:app_error:0x8001:status: bad").expect("colon-in-body parses");
        assert_eq!(code, 0x8001);
        assert_eq!(body, "status: bad");

        // Uppercase 0X variant tolerated.
        let (code, body) =
            parse_js_app_error("nrpc:app_error:0X4001:detail").expect("uppercase 0X");
        assert_eq!(code, 0x4001);
        assert_eq!(body, "detail");

        // Empty body permitted.
        let (code, body) = parse_js_app_error("nrpc:app_error:0x0001:").expect("empty body");
        assert_eq!(code, 1);
        assert_eq!(body, "");
    }

    /// Anything not matching the canonical shape is rejected;
    /// the caller falls through to the generic Internal mapping
    /// so user-thrown plain errors keep their existing semantics.
    #[test]
    fn parse_js_app_error_rejects_malformed_messages() {
        // No prefix.
        assert!(parse_js_app_error("plain error").is_none());
        // Missing trailing body / colon.
        assert!(parse_js_app_error("nrpc:app_error:0x8000").is_none());
        // Non-hex code.
        assert!(parse_js_app_error("nrpc:app_error:zz:body").is_none());
        // Code overflows u16.
        assert!(parse_js_app_error("nrpc:app_error:0x10000:body").is_none());
        // Empty (just prefix).
        assert!(parse_js_app_error("nrpc:app_error:").is_none());
    }

    /// Regression: Rust-side codec error messages (the format
    /// surfaced by the typed bad-request handler path,
    /// `RpcError::Codec`'s Display, the various encode/decode
    /// failure strings) MUST NOT accidentally match the
    /// `nrpc:app_error:<code>:<body>` shape. If they did, a
    /// codec failure on the JS side could be misrouted to the
    /// Application-error arm with bogus code/body.
    #[test]
    fn parse_js_app_error_does_not_match_codec_diagnostics() {
        // The diagnostic strings emitted by the typed wrappers
        // and substrate codec on various failure paths. Every
        // one MUST return None — otherwise a codec error would
        // surface as an Application error on the JS side.
        let codec_strings = [
            "typed streaming handler: bad request body: invalid type: integer `1`, expected struct",
            "typed sink encode: missing field `numbers`",
            "rpc codec encode: invalid number",
            "rpc codec decode: trailing data",
            "decode failed: invalid utf-8 sequence",
            "typed handler returned Err(String): something went wrong",
            // Looks vaguely similar but no `nrpc:app_error:` prefix.
            "Error: app_error 0x8000",
            "0x8000:body",
            // Has prefix but is short of the colon-code-colon shape.
            "nrpc:app_error",
            // Whitespace-prefixed variants — parser is strict.
            " nrpc:app_error:0x8000:body",
        ];
        for s in codec_strings {
            assert!(
                parse_js_app_error(s).is_none(),
                "codec/diagnostic string MUST NOT match app-error format: {s:?}",
            );
        }
    }

    /// `NodeRpcObserver::on_call` drops events when the bounded
    /// channel fills, incrementing the process-global drop counter
    /// by one per drop. Pinned because the v3 locked decision #1
    /// hinges on this — overflow MUST surface via the snapshot's
    /// `observer_dropped_total` field so a slow JS consumer is
    /// observable from production telemetry rather than a
    /// silently-on-fire dispatch path.
    ///
    /// The test bypasses the TSFN-bearing `install()` by constructing
    /// the observer struct directly with a sender whose receiver is
    /// held but never drained — the channel fills up, every event
    /// past 1024 hits the overflow branch. We hold `_recv` ourselves
    /// so the channel doesn't close (which would also trip the
    /// `is_err()` arm but for the wrong reason).
    #[test]
    fn observer_drops_overflow_events_and_counts_them() {
        use std::sync::atomic::Ordering;

        let baseline = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed);
        let (sender, _recv) =
            tokio::sync::mpsc::channel::<RpcCallEventJs>(OBSERVER_BUFFER_CAPACITY);
        let obs = NodeRpcObserver { sender };
        let make_event = || InnerRpcCallEvent {
            caller: 1,
            callee: 2,
            method: "test.svc.echo".into(),
            latency_ms: 0,
            status: InnerRpcCallStatus::Ok,
            request_bytes: 0,
            response_bytes: 0,
            direction: InnerRpcDirection::Outbound,
            ts_unix_ms: 0,
        };
        const FIRED: u64 = 2000;
        for _ in 0..FIRED {
            obs.on_call(make_event());
        }
        let dropped = OBSERVER_DROPPED_TOTAL.load(Ordering::Relaxed) - baseline;
        let expected = FIRED - OBSERVER_BUFFER_CAPACITY as u64;
        // Allow slack — the counter is process-global so concurrent
        // tests could nudge it; but the per-fire delta from THIS
        // test must be at least the overflow count.
        assert!(
            dropped >= expected,
            "expected ≥ {expected} drops, got {dropped}",
        );
    }
}
