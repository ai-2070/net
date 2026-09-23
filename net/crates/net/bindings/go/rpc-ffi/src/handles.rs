//! The shared C-ABI streaming handle types (§4.4 — "types move to a module
//! both `rpc-ffi` and `org-ffi` can name").
//!
//! Both surfaces hand out the SAME opaque handle types: `net_rpc_call_*` and
//! `net_org_call_streaming/_client_stream/_duplex` return
//! [`RpcStreamHandleC`] / [`ClientStreamCallHandleC`] / [`DuplexCallHandleC`],
//! and every consumer drives them through the one `net_rpc_*` operation set
//! (`net_rpc_stream_next`, `net_rpc_client_stream_send`, …). This works
//! because both rlibs are linked into the single `libnet` cdylib
//! (`bindings/go/net-ffi`) — one type, one copy, one set of `static`s. It
//! would NOT work across two cdylibs; that arrangement is exactly what the
//! single-cdylib rule exists to make unrepresentable.
//!
//! The Go wrappers mirror the same sharing: `net_org_call_streaming` returns
//! a `*RpcStream` the public `MeshRpc.CallStreaming` surface also produces,
//! so `streamHandleGuard` and the ctx-cancel watcher are reused verbatim.
//!
//! # Error-wire style ([`ErrWireFn`])
//!
//! The operations that report a midstream failure write ONE message string
//! through `out_err`, and the two surfaces' vocabularies differ on purpose:
//!
//!   - `rpc-ffi` writes the nRPC shape `<kind>: <detail>` (`timeout: …`),
//!     which the Go `parseRpcError` lifts into a typed `*RpcError`.
//!   - `org-ffi` writes the canonical `org:<domain>:<kind>[: <detail>]` wire
//!     (`OrgSdkError::to_wire` — the single source of that vocabulary), which
//!     the Go `parseOrgError` lifts into a typed `*OrgError` with the right
//!     domain. An org stream whose midstream error arrived in the nRPC shape
//!     would classify as `unknown`, silently destroying the rpc-domain
//!     distinction the org error model documents (`include/net_org.h`).
//!
//! So each caller-side handle carries the formatter of the surface that
//! constructed it — set at construction, consulted only on the error path,
//! and impossible to observe on success.

use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::Mutex;

use net::adapter::net::cortex::{RequestStream, RpcResponseSink};
use net::adapter::net::mesh_rpc::{
    ClientStreamCallRaw, DuplexCallRaw, DuplexSink, DuplexStream, RpcError, RpcStream,
};

/// Formats one midstream [`RpcError`] for the `out_err` out-param of the
/// streaming operations. Supplied by the constructing surface: `rpc-ffi`
/// passes its `format_rpc_error` (the nRPC shape), `org-ffi` passes a
/// formatter over `OrgSdkError::to_wire` (the `org:` shape).
pub type ErrWireFn = fn(RpcError) -> String;

/// Opaque caller-side handle for a streaming-response call. The inner SDK
/// stream sits behind an `Arc<Mutex<Option<..>>>` so:
///   - `close()` can `take()` the stream (which fires CANCEL via the SDK's
///     `Drop` impl) and remain idempotent.
///   - `next()` locks, polls, and re-stores `Some(stream)` until the stream
///     terminates.
///
/// Once `close()` runs OR the stream has yielded its terminal item,
/// subsequent `next()` calls return `NET_RPC_ERR_STREAM_DONE`.
pub struct RpcStreamHandleC {
    pub inner: Arc<Mutex<Option<RpcStream>>>,
    /// Mirrors the SDK's `RpcStream::call_id`. Captured at construction so
    /// the diagnostic accessor doesn't need to re-acquire the mutex.
    pub call_id: u64,
    /// `true` once a terminal item (clean end OR error) has been observed.
    /// Latched separately from the `Option` so we don't re-take the inner
    /// stream just to check this state.
    pub done: AtomicBool,
    /// This surface's error-wire vocabulary. See the module doc.
    pub err_wire: ErrWireFn,
}

impl RpcStreamHandleC {
    /// Wrap a freshly opened stream. `call_id` is captured here.
    pub fn new(stream: RpcStream, err_wire: ErrWireFn) -> Self {
        let call_id = stream.call_id();
        Self {
            inner: Arc::new(Mutex::new(Some(stream))),
            call_id,
            done: AtomicBool::new(false),
            err_wire,
        }
    }

    /// Render one midstream error in this handle's wire vocabulary.
    pub fn format_err(&self, e: RpcError) -> String {
        (self.err_wire)(e)
    }
}

/// Opaque caller-side handle for a client-streaming call.
///
/// The inner `ClientStreamCallRaw` is held inside an `Option` behind a
/// `Mutex` so the state machine (`JustOpened` → `Sending` → `Finishing` →
/// `Done`) can be driven across multiple FFI calls without re-entry hazards.
/// `finish` `take()`s the inner value permanently; subsequent `send` /
/// `finish` calls observe `None` and return `NET_RPC_ERR_STREAM_DONE`.
///
/// `call_id` is captured at construction so `net_rpc_client_stream_call_id`
/// doesn't need to lock the mutex. `done` is the same latch as
/// [`RpcStreamHandleC`].
pub struct ClientStreamCallHandleC {
    pub inner: Arc<Mutex<Option<ClientStreamCallRaw>>>,
    pub call_id: u64,
    pub done: AtomicBool,
    /// This surface's error-wire vocabulary. See the module doc.
    pub err_wire: ErrWireFn,
}

impl ClientStreamCallHandleC {
    /// Wrap a freshly opened call. `call_id` is captured here.
    pub fn new(call: ClientStreamCallRaw, err_wire: ErrWireFn) -> Self {
        let call_id = call.call_id();
        Self {
            inner: Arc::new(Mutex::new(Some(call))),
            call_id,
            done: AtomicBool::new(false),
            err_wire,
        }
    }

    /// Render one midstream error in this handle's wire vocabulary.
    pub fn format_err(&self, e: RpcError) -> String {
        (self.err_wire)(e)
    }
}

/// Opaque caller-side handle for a duplex call (combined send + receive).
/// Mirrors [`RpcStreamHandleC`]` shape: the two halves behind mutexes, with a
/// captured `call_id` and a `done` latch.
///
/// **Auto-split.** The combined `DuplexCallRaw` is split into a `DuplexSink`
/// + `DuplexStream` at construction so concurrent send + recv from Go (the
/// primary duplex use case) do NOT contend on the same mutex. Both halves
/// share the underlying `Arc<DuplexInner>`, so CANCEL-on-Drop semantics are
/// preserved: the wire CANCEL fires only after both halves have been dropped
/// without a clean close.
pub struct DuplexCallHandleC {
    pub sink: Arc<Mutex<Option<DuplexSink>>>,
    pub stream: Arc<Mutex<Option<DuplexStream>>>,
    pub call_id: u64,
    pub done: AtomicBool,
    /// This surface's error-wire vocabulary. See the module doc.
    pub err_wire: ErrWireFn,
}

impl DuplexCallHandleC {
    /// Wrap a freshly opened call, auto-splitting it into the two halves.
    pub fn new(call: DuplexCallRaw, err_wire: ErrWireFn) -> Self {
        let call_id = call.call_id();
        let (sink, stream) = call.into_split();
        Self {
            sink: Arc::new(Mutex::new(Some(sink))),
            stream: Arc::new(Mutex::new(Some(stream))),
            call_id,
            done: AtomicBool::new(false),
            err_wire,
        }
    }

    /// Render one midstream error in this handle's wire vocabulary.
    pub fn format_err(&self, e: RpcError) -> String {
        (self.err_wire)(e)
    }
}

/// Opaque caller-side handle for the send-half of a split duplex call.
/// Constructed by `net_rpc_duplex_into_split`. `sink_finish` consumes the
/// inner sink.
pub struct DuplexSinkHandleC {
    pub inner: Arc<Mutex<Option<DuplexSink>>>,
    pub call_id: u64,
    pub done: AtomicBool,
    /// This surface's error-wire vocabulary. See the module doc.
    pub err_wire: ErrWireFn,
}

impl DuplexSinkHandleC {
    /// Wrap one half of a split duplex call. Used by `net_rpc_duplex_into_split`.
    pub fn new(sink: DuplexSink, call_id: u64, err_wire: ErrWireFn) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(sink))),
            call_id,
            done: AtomicBool::new(false),
            err_wire,
        }
    }

    /// Render one midstream error in this handle's wire vocabulary.
    pub fn format_err(&self, e: RpcError) -> String {
        (self.err_wire)(e)
    }
}

/// Opaque caller-side handle for the receive-half of a split duplex call.
/// Constructed by `net_rpc_duplex_into_split`. Drains chunks via
/// `_stream_next` until terminal End / Error.
pub struct DuplexStreamHandleC {
    pub inner: Arc<Mutex<Option<DuplexStream>>>,
    pub call_id: u64,
    pub done: AtomicBool,
    /// This surface's error-wire vocabulary. See the module doc.
    pub err_wire: ErrWireFn,
}

impl DuplexStreamHandleC {
    /// Wrap one half of a split duplex call. Used by `net_rpc_duplex_into_split`.
    pub fn new(stream: DuplexStream, call_id: u64, err_wire: ErrWireFn) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(stream))),
            call_id,
            done: AtomicBool::new(false),
            err_wire,
        }
    }

    /// Render one midstream error in this handle's wire vocabulary.
    pub fn format_err(&self, e: RpcError) -> String {
        (self.err_wire)(e)
    }
}

/// The join-failure stand-in for [`spawn_handler_thread`]: displays like the
/// `JoinError` it replaces. A handler thread that dies without delivering a
/// result (a panic past Go's recover, or an OS-level thread failure) is one
/// class at this boundary.
pub struct HandlerJoinError(());

impl std::fmt::Display for HandlerJoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the dedicated handler thread ended without a result")
    }
}

/// Run one Go handler callback on a DEDICATED OS THREAD — deliberately NOT
/// `tokio::task::spawn_blocking` — and expose exactly the
/// `Result<T, HandlerJoinError>` join shape the handler bridges match on.
///
/// Why not the blocking pool: Go stream handlers RE-ENTER this library from
/// inside the callback (`net_rpc_request_stream_next` /
/// `net_rpc_response_sink_send` — the documented drain pattern), and the
/// blocking entry points drive their futures with `block_on`, which ABORTS
/// the process when called from inside a tokio runtime context (the
/// deliberate fail-loud at every FFI `block_on`). A blocking-pool thread HAS
/// that context entered; a fresh thread does not, so the exact `block_on`
/// that works from the caller's goroutine works here. A unary handler that
/// merely calls back into another call verb needs the same property.
///
/// A handler that outlives the caller's timeout keeps running to completion
/// (as `spawn_blocking` did — neither can be aborted mid-callback); the
/// bridge reports the timeout and the operator's monitoring catches a
/// runaway.
pub fn spawn_handler_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Result<T, HandlerJoinError>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("net-go-handler".into())
        .spawn(move || {
            let _ = tx.send(work());
        });
    async move {
        match spawned {
            Ok(_thread) => rx.await.map_err(|_| HandlerJoinError(())),
            // The OS refused a thread (resource exhaustion) — surface the
            // same join failure rather than half-registering a call.
            Err(_) => Err(HandlerJoinError(())),
        }
    }
}

/// Per-call opaque handle wrapping the SDK's `RequestStream`. The handler
/// pulls request chunks via `net_rpc_request_stream_next` until it sees
/// `STREAM_DONE`.
///
/// Lifetime is bounded by the dispatcher call — Rust constructs the handle,
/// passes it to the Go dispatcher, and frees it after the dispatcher
/// returns. The Go side MUST NOT call any `_free` on this handle.
///
/// Shared by the unary nRPC shape dispatchers and the org shape dispatchers
/// (`net_org_set_*_handler_dispatcher`): the fn types differ only in the
/// leading `*const NetOrgCaller`, never in the handle types.
pub struct RpcRequestStreamHandleC {
    pub inner: Mutex<Option<RequestStream>>,
    pub done: AtomicBool,
}

impl RpcRequestStreamHandleC {
    /// Wrap one per-call request stream for the dispatcher's lifetime.
    pub fn new(requests: RequestStream) -> Self {
        Self {
            inner: Mutex::new(Some(requests)),
            done: AtomicBool::new(false),
        }
    }
}

/// Per-call opaque handle wrapping the SDK's `RpcResponseSink`. Used by
/// streaming and duplex handlers to emit response chunks. Same lifetime
/// contract as [`RpcRequestStreamHandleC`] — Rust owns, Go borrows for the
/// dispatcher call.
pub struct RpcResponseSinkHandleC {
    pub inner: Mutex<Option<RpcResponseSink>>,
}

impl RpcResponseSinkHandleC {
    /// Wrap one per-call response sink for the dispatcher's lifetime.
    pub fn new(responses: RpcResponseSink) -> Self {
        Self {
            inner: Mutex::new(Some(responses)),
        }
    }
}
