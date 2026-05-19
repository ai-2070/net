//! `Mesh::serve_rpc` / `Mesh::call` glue — the wire-up between
//! `MeshNode`'s pub/sub + per-channel-hash dispatch hook and the
//! `cortex::rpc` server / client folds.
//!
//! See `docs/misc/NRPC_DESIGN.md` for the full architectural framing.
//! In short:
//!
//! - `serve_rpc(service, handler)` registers an inbound dispatcher
//!   for `<service>.requests`'s channel hash. The dispatcher pushes
//!   inbound REQUEST/CANCEL events through the
//!   [`crate::adapter::net::cortex::RpcServerFold`], which spawns
//!   the user handler. The fold's emit closure publishes RESPONSE
//!   events on `<service>.replies.<caller_origin>` via
//!   [`MeshNode::publish`].
//!
//! - `call(target, service, payload, opts)` allocates a `call_id`,
//!   registers a oneshot in the per-Mesh `RpcClientPending`,
//!   subscribes to its own reply channel from `target` (lazy,
//!   cached), publishes the REQUEST envelope on `<service>.requests`,
//!   awaits the oneshot. Drop sends a CANCEL.
//!
//! Phase 1 surface — direct entity-to-entity addressing
//! (`call(target_node_id, ...)`), no service discovery layer yet.
//! Phase 2 will add `call_service(name, ...)` over the existing
//! capability-announcement registry.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::channel::{ChannelHash, ChannelId, ChannelName, ChannelPublisher, PublishConfig};
use super::cortex::{
    build_trace_headers, encode_request_grant, encode_stream_grant, EventMeta,
    RpcAsyncResponseEmitter, RpcCancellationToken, RpcClientFold, RpcClientStreamingHandler,
    RpcContext, RpcDuplexFold, RpcDuplexHandler, RpcHandler, RpcHandlerError, RpcInboundDispatcher,
    RpcInboundEvent, RpcRequestChunkPayload, RpcRequestGrantEmitter, RpcRequestPayload,
    RpcResponseEmitter, RpcResponsePayload, RpcServerFold, RpcServerStreamingFold, RpcStatus,
    RpcStreamingHandler, RpcStreamingRequestFold, StreamItem, TraceContext, DISPATCH_RPC_CANCEL,
    DISPATCH_RPC_REQUEST, DISPATCH_RPC_REQUEST_CHUNK, DISPATCH_RPC_REQUEST_GRANT,
    DISPATCH_RPC_STREAM_GRANT, EVENT_META_SIZE, FLAG_RPC_CLIENT_STREAMING_REQUEST,
    FLAG_RPC_PROPAGATE_TRACE, FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE,
    HEADER_NRPC_REQUEST_WINDOW_INITIAL, HEADER_NRPC_STREAM_WINDOW_INITIAL,
};
use super::mesh_rpc_metrics::{CallMetricsGuard, CallOutcome};
use crate::error::AdapterError;

use super::mesh::MeshNode;
use super::redex::{RedexEntry, RedexEvent, RedexFold};

// ============================================================================
// Public types.
// ============================================================================

/// How `Mesh::call_service` picks a target from the set of nodes
/// advertising the requested service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RoutingPolicy {
    /// Naive round-robin via the per-Mesh `call_id` counter.
    /// Distributes calls evenly across candidates regardless of
    /// load. The default.
    #[default]
    RoundRobin,
    /// Pick a candidate at random per call. Stateless, cheap, and
    /// gives even distribution under independent calls.
    Random,
    /// Consistent-hash to a target by `key`. Same `key` always
    /// hits the same target as long as the candidate set is
    /// stable. Useful for session affinity (route a given
    /// conversation / shard / user to the same backend).
    Sticky {
        /// Caller-supplied identifier — hash maps this to the
        /// target. Use a session id, shard key, or conversation
        /// id depending on the application.
        key: u64,
    },
    /// Pick the candidate with the smallest measured `latency_us`
    /// per the local `ProximityGraph`. Candidates the proximity
    /// graph hasn't observed yet (no entity ↔ node_id mapping or
    /// no pingwave received) sort to the bottom — better to pick
    /// a known-fast node than gamble on an unknown one.
    ///
    /// Falls back deterministically to the first sorted candidate
    /// when no candidates have proximity data, so a freshly-
    /// discovered service still routes consistently.
    LowestLatency,
}

/// Options for [`MeshNode::call`] and [`MeshNode::call_service`].
#[derive(Debug, Clone)]
pub struct CallOptions {
    /// Hard deadline for the call. The future returned by `call`
    /// races a `tokio::time::sleep_until`; whichever fires first
    /// wins. On timeout the caller emits a CANCEL event for
    /// `call_id` so the server can drop the in-flight handler.
    /// `None` means no deadline; the caller waits indefinitely
    /// (or until the future is dropped).
    pub deadline: Option<Instant>,
    /// How `call_service` picks a target. Ignored by `call`
    /// (which takes an explicit `target_node_id`). Default:
    /// `RoundRobin`.
    pub routing_policy: RoutingPolicy,
    /// Skip candidates whose `ProximityGraph` entry reports
    /// `!is_available()` (i.e. `Unhealthy` or `Unknown`).
    /// Default `true`. Candidates with no proximity entry at all
    /// are KEPT — absence of evidence is not evidence of
    /// unhealth, and a freshly-announced service shouldn't be
    /// filtered just because pingwaves haven't propagated yet.
    pub filter_unhealthy: bool,
    /// W3C Trace Context to propagate to the server. When `Some`,
    /// the call sets `FLAG_RPC_PROPAGATE_TRACE` on the request and
    /// emits `traceparent` / `tracestate` headers; the server's
    /// `RpcContext::trace_context` will be populated with the same
    /// values. nRPC is transport-only — application code on both
    /// sides reads / writes this via whatever tracing backend it
    /// has wired up (tracing-opentelemetry, Datadog, etc.).
    pub trace_context: Option<TraceContext>,
    /// Per-call concurrency cap. Future Phase 2 work; v1 ignores
    /// this and the per-Mesh `RpcClientPending` doesn't bound
    /// in-flight count.
    pub max_in_flight_per_target: u32,
    /// **Streaming responses only.** Initial credit window for
    /// per-streaming-response flow control. When `Some(n)`, the
    /// caller emits `nrpc-stream-window-initial: n` on the
    /// REQUEST and the server's pump task awaits one credit per
    /// emitted chunk. The returned [`RpcStream`] auto-grants 1
    /// credit per consumed chunk so the in-flight credit holds
    /// near `n` (or use [`RpcStream::grant`] for batched / custom
    /// cadence). `None` (the default) → unbounded: server pumps
    /// chunks as fast as the publish path can take them
    /// (back-compat / pre-flow-control behavior). Ignored by
    /// non-streaming `call` / `call_service`.
    pub stream_window_initial: Option<u32>,
    /// **Client-streaming / duplex only.** Initial credit window
    /// for per-call request-direction flow control. Mirror of
    /// [`Self::stream_window_initial`] for the upload direction. When
    /// `Some(n)`, the caller emits `nrpc-request-window-initial: n`
    /// on the REQUEST and its `send().await` sink awaits one
    /// credit per pushed chunk; the server refills via
    /// [`DISPATCH_RPC_REQUEST_GRANT`] events. `None` → unbounded:
    /// caller's send sink doesn't block (legacy / fast-path).
    /// Ignored by unary `call` / `call_streaming`.
    ///
    /// Bidi streaming plan (Phase C).
    pub request_window_initial: Option<u32>,
    /// Caller-supplied request headers. Appended to the wire
    /// `RpcRequestPayload::headers` after any auto-generated
    /// headers (trace context, stream-window). Useful for
    /// application-level metadata the server needs at
    /// dispatch-time — e.g., the `net-where` predicate
    /// header (Phase 9b of `CAPABILITY_SYSTEM_SDK_PLAN.md`) that
    /// services consult for predicate-pushdown filtering.
    ///
    /// Each entry is `(name, value_bytes)`. Names use the lowercase
    /// `cyberdeck-*` / `nrpc-*` convention; the substrate doesn't
    /// validate names beyond the `MAX_RPC_HEADER_NAME_LEN` cap
    /// enforced at encode time.
    ///
    /// Default: empty.
    pub request_headers: Vec<(String, Vec<u8>)>,
}

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            deadline: None,
            routing_policy: RoutingPolicy::default(),
            filter_unhealthy: true,
            trace_context: None,
            max_in_flight_per_target: 64,
            stream_window_initial: None,
            request_window_initial: None,
            request_headers: Vec::new(),
        }
    }
}

/// What [`MeshNode::call`] returns on success.
#[derive(Debug, Clone)]
pub struct RpcReply {
    /// Response payload from the server's handler. Caller decodes
    /// according to its application protocol.
    pub body: Bytes,
    /// Headers attached by the server's response.
    pub headers: Vec<(String, Vec<u8>)>,
    /// Wall-clock latency from `call(...)` to RESPONSE arrival.
    pub latency_ns: u64,
}

/// What [`MeshNode::call`] returns on failure.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// No subscription / no route to the target. Either
    /// `target_node_id` is unknown to the local mesh, or the
    /// caller's reply-channel subscription couldn't be set up.
    #[error("no route to target {target:#x}: {reason}")]
    NoRoute {
        /// Target node id the call was directed at.
        target: u64,
        /// Diagnostic — typically the underlying transport error.
        reason: String,
    },
    /// Caller's deadline elapsed before a RESPONSE arrived. The
    /// caller emits a CANCEL on timeout so the server can drop
    /// the in-flight handler; this variant is returned to the
    /// awaiting caller.
    #[error("timeout after {elapsed_ms}ms")]
    Timeout {
        /// Wall-clock milliseconds elapsed before timeout fired.
        elapsed_ms: u64,
    },
    /// Server returned a non-`Ok` status. Body carries the
    /// server's diagnostic (UTF-8) when available.
    #[error("server returned status {status:#06x}: {message}")]
    ServerError {
        /// Wire-level `RpcStatus` value the server returned.
        status: u16,
        /// UTF-8 diagnostic from the response body, when the body
        /// decodes as valid UTF-8; otherwise hex-truncated.
        message: String,
    },
    /// Underlying transport error (publish failure, encryption,
    /// etc.).
    #[error("transport: {0}")]
    Transport(#[from] AdapterError),
    /// Client-local serialization or deserialization failure.
    /// `direction = Encode` means the typed wrapper failed to
    /// encode the request before it ever hit the wire;
    /// `direction = Decode` means the response landed but the
    /// typed wrapper failed to decode it. Either way this is a
    /// caller-fixable bug (wrong codec, schema drift, malformed
    /// `Serialize` impl) — NOT a transient infra failure — so
    /// retry / circuit-breaker predicates skip it by default.
    #[error("codec ({direction:?}): {message}")]
    Codec {
        /// Which side of the call the codec failure happened on.
        direction: CodecDirection,
        /// Decode/encode diagnostic from the underlying serde impl.
        message: String,
    },
}

/// Which side of the call surfaced a [`RpcError::Codec`] failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecDirection {
    /// Encoding the outbound request failed before the call was issued.
    Encode,
    /// Decoding the inbound response failed after the call returned Ok.
    Decode,
}

/// RAII handle returned by [`MeshNode::serve_rpc`]. Dropping it
/// unregisters the inbound dispatcher and removes the service
/// from the local-services registry (so subsequent
/// `announce_capabilities` calls stop emitting the
/// `nrpc:<service>` tag).
///
/// **Bridge task lifecycle.** The bridge task that drains the
/// inbound mpsc into the fold is NOT aborted on Drop. The
/// `register_rpc_inbound` dispatcher closure owns the only
/// `mpsc::Sender` clone, so `unregister_rpc_inbound` (which drops
/// the dispatcher) closes the channel; the bridge's `rx.recv()`
/// then yields `None` and the task exits cleanly after draining
/// any queued events. Aborting would race events that are
/// mid-`fold.lock().apply()` — those events would be killed
/// without their RESPONSE being emitted, so the corresponding
/// callers would just time out.
///
/// Outstanding handler executions (already-spawned tokio tasks)
/// continue to completion regardless.
pub struct ServeHandle {
    /// Channel hash to unregister on Drop.
    channel_hash: ChannelHash,
    /// Service name to remove from `rpc_local_services` on Drop.
    service: String,
    /// The bridge task. Held only so callers can introspect /
    /// detach it; Drop does NOT abort it (see struct doc-comment).
    /// Detaches naturally when the handle is dropped — the bridge
    /// exits on its own once the dispatcher's `mpsc::Sender` is
    /// dropped via `unregister_rpc_inbound`.
    _bridge: JoinHandle<()>,
    /// Hold an Arc back to the mesh so we can unregister on Drop
    /// without the mesh having to track us.
    mesh: Arc<MeshNode>,
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        // Order matters: unregister the dispatcher FIRST so no new
        // events can land in the bridge's mpsc, THEN drop the
        // service-tag entry. The bridge task drains any in-flight
        // events naturally and exits when its `rx.recv()` yields
        // `None` (which happens as soon as the dispatcher closure
        // — the sole `tx` owner — is dropped above).
        self.mesh.unregister_rpc_inbound(self.channel_hash);
        self.mesh.rpc_local_services_arc().remove(&self.service);
    }
}

// ============================================================================
// Streaming caller-side: RpcStream.
// ============================================================================

/// An open streaming RPC call. Implements `Stream<Item =
/// Result<Bytes, RpcError>>` — yields chunks as the server emits
/// them, terminates on a clean stream-end frame OR a non-`Ok`
/// status (which is yielded as the last `Err` item before the
/// stream closes).
///
/// Dropping the stream emits a CANCEL to the server (best-effort)
/// and discards the pending entry — any chunks the server emits
/// after the drop are silently discarded by the client fold.
pub struct RpcStream {
    mesh: Arc<MeshNode>,
    target_node_id: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
    inner: tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
    /// Set true once we've yielded the terminal item (or an
    /// error). Subsequent polls return `None`.
    done: bool,
    /// `Some(_)` if this stream uses flow control (caller set
    /// `CallOptions::stream_window_initial`). Auto-grant emits 1
    /// credit per delivered chunk, which keeps the server's
    /// credit at roughly the initial window. `None` → no flow
    /// control; `poll_next` does not emit grants.
    stream_window: Option<u32>,
    /// Observer-fire bookkeeping. Latched on terminal observation
    /// in `poll_next`; fired once from `Drop` so the Deck NRPC
    /// tab + every other `RpcObserver` consumer sees one event
    /// per streaming-response call.
    observer: StreamingObserverState,
}

impl RpcStream {
    /// Server-assigned `call_id`. Useful for trace correlation /
    /// custom logging at the call site.
    pub fn call_id(&self) -> u64 {
        self.call_id
    }

    /// Whether this stream is flow-controlled (caller set
    /// `CallOptions::stream_window_initial`). Useful for tests +
    /// diagnostics; user code typically doesn't need to inspect
    /// this.
    pub fn flow_controlled(&self) -> bool {
        self.stream_window.is_some()
    }

    /// Explicitly grant `amount` more credits to the server's
    /// pump. Spawns a fire-and-forget publish; doesn't await
    /// acknowledgement. **No-op when flow control was not enabled
    /// for this stream** — the server would silently drop the
    /// grant anyway, and emitting wire traffic with no purpose
    /// would just burn bandwidth.
    ///
    /// Auto-grant (1 credit per delivered chunk) covers the
    /// common case; use this for batched cadence (e.g. grant
    /// `window/2` after every `window/2` chunks consumed) when
    /// `auto_grant`-style amortization isn't enough.
    pub fn grant(&self, amount: u32) {
        if !self.flow_controlled() || amount == 0 {
            return;
        }
        spawn_grant_publish(
            Arc::clone(&self.mesh),
            self.target_node_id,
            self.request_channel.clone(),
            self.self_origin,
            self.call_id,
            amount,
        );
    }
}

/// Shared fire-and-forget GRANT-publish helper. Used by
/// [`RpcStream::grant`] (explicit) and the auto-grant in
/// [`RpcStream::poll_next`]. Same direct-unicast publish path as
/// [`spawn_cancel_publish`], just with a different dispatch byte
/// + a 4-byte u32 payload.
fn spawn_grant_publish(
    mesh: Arc<MeshNode>,
    target: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
    amount: u32,
) {
    tokio::spawn(async move {
        let meta = EventMeta::new(DISPATCH_RPC_STREAM_GRANT, 0, self_origin, call_id, 0);
        let request_channel_id = ChannelId::new(request_channel);
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + 4);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&encode_stream_grant(amount));
        let payload = Bytes::from(buf);
        let _ = mesh
            .publish_to_peer(
                target,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await;
    });
}

impl futures::Stream for RpcStream {
    type Item = Result<Bytes, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        match self.inner.poll_recv(cx) {
            std::task::Poll::Ready(Some(StreamItem::Chunk(body))) => {
                // Auto-grant 1 credit per delivered chunk so the
                // server's pump stays at roughly the initial
                // window. No-op when the stream isn't flow-
                // controlled. For batched cadence, callers can
                // skip auto-grant by NOT setting
                // `stream_window_initial` and using `RpcStream::grant`
                // directly with their preferred batching.
                if self.stream_window.is_some() {
                    spawn_grant_publish(
                        Arc::clone(&self.mesh),
                        self.target_node_id,
                        self.request_channel.clone(),
                        self.self_origin,
                        self.call_id,
                        1,
                    );
                }
                self.observer.add_response_bytes(body.len() as u32);
                std::task::Poll::Ready(Some(Ok(body)))
            }
            std::task::Poll::Ready(Some(StreamItem::End)) => {
                self.done = true;
                self.observer.latch_ok();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Some(StreamItem::Error(resp))) => {
                self.done = true;
                let status = resp.status.to_wire();
                let message = String::from_utf8(resp.body).unwrap_or_else(|e| {
                    format!("<{} bytes of non-utf8 body>", e.into_bytes().len())
                });
                self.observer
                    .latch_error(format!("server returned status {status:#06x}: {message}"));
                std::task::Poll::Ready(Some(Err(RpcError::ServerError { status, message })))
            }
            std::task::Poll::Ready(None) => {
                self.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        // Best-effort CANCEL to the server. Spawn a task because
        // Drop can't be async; the publish happens off-thread.
        // Also clear our pending entry so any in-flight chunks
        // are dropped on arrival.
        self.mesh.rpc_client_pending_arc().cancel(self.call_id);
        spawn_cancel_publish(
            Arc::clone(&self.mesh),
            self.target_node_id,
            self.request_channel.clone(),
            self.self_origin,
            self.call_id,
        );
        // Fire the observer with the latched status (Ok / Error /
        // Canceled). Idempotent — only the first fire emits.
        self.observer.fire();
    }
}

// ============================================================================
// Phase C — caller-side client-streaming / duplex primitive.
// ============================================================================

/// Shared REQUEST_CHUNK-publish helper. Builds the wire frame and
/// fires through `publish_to_peer` direct-unicast (same routing
/// pattern as the initial REQUEST — caller knows the target).
async fn publish_request_chunk(
    mesh: &Arc<MeshNode>,
    target: u64,
    request_channel: &ChannelName,
    self_origin: u64,
    chunk: &RpcRequestChunkPayload,
) -> Result<(), RpcError> {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, self_origin, chunk.call_id, 0);
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + chunk.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&chunk.encode());
    let request_channel_id = ChannelId::new(request_channel.clone());
    let request_channel_hash = request_channel_id.hash();
    let stream_id = MeshNode::publish_stream_id(&request_channel_id);
    let payload = Bytes::from(buf);
    mesh.publish_to_peer(
        target,
        request_channel_hash,
        stream_id,
        /* reliable */ true,
        std::slice::from_ref(&payload),
    )
    .await
    .map_err(RpcError::Transport)
}

/// Internal state of a [`ClientStreamCallRaw`]. The state machine
/// is small: open the call (initial REQUEST not yet sent), then
/// send N items (the first becomes the initial REQUEST, subsequent
/// become REQUEST_CHUNKs), then finish (terminal REQUEST_END
/// frame). After finish, no further sends are accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStreamState {
    /// Pending entry registered, reply subscription ensured, but
    /// the initial REQUEST has NOT been published to the wire yet.
    /// First `send` flips this to `Sending`.
    JustOpened,
    /// Initial REQUEST has been published; subsequent sends ride
    /// as REQUEST_CHUNKs.
    Sending,
    /// `finish` has been called; the terminal REQUEST_END frame
    /// (or the initial REQUEST with FLAG_END for the degenerate
    /// zero-send path) has been published. The terminal RESPONSE
    /// has not necessarily arrived yet — that's awaited on the
    /// caller's terminal_rx.
    Finishing,
    /// Terminal RESPONSE has been delivered. Drop is a no-op.
    Done,
}

/// Caller-side handle for a client-streaming (or duplex Phase D)
/// RPC. Push N items via [`ClientStreamCallRaw::send`], then
/// [`ClientStreamCallRaw::finish`] to await the terminal RESPONSE.
///
/// **Lazy initial REQUEST.** The initial REQUEST is published on
/// the FIRST `send()` (or on `finish()` if the caller sends nothing
/// — that's the "zero-item upload" degenerate path that opens and
/// closes the call in one frame). Constructing the handle does
/// NOT yet emit any wire traffic beyond the reply-channel
/// subscription setup.
///
/// **Flow control.** When the caller set
/// [`CallOptions::request_window_initial`] to `Some(n)`, the
/// handle holds an `n`-permit `Semaphore` that gates `send`. The
/// server's [`DISPATCH_RPC_REQUEST_GRANT`] events refill the
/// semaphore. When `None`, `send` doesn't block (caller is on the
/// unbounded-credit fast path).
///
/// **Cancellation.** Dropping the handle BEFORE `finish` returns
/// `Ok` fires a best-effort CANCEL to the server and clears the
/// pending entry. Dropping after a successful `finish` is a no-op
/// (terminal RESPONSE already delivered + entry removed).
///
/// Bidi streaming plan (Phase C).
pub struct ClientStreamCallRaw {
    mesh: Arc<MeshNode>,
    target_node_id: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
    service: String,
    /// Header set queued for the initial REQUEST. Drained on the
    /// first publish (either `send` or `finish`).
    initial_headers: Vec<(String, Vec<u8>)>,
    /// Flag bits queued for the initial REQUEST. Always carries
    /// `FLAG_RPC_CLIENT_STREAMING_REQUEST`; may also carry
    /// `FLAG_RPC_PROPAGATE_TRACE` when the caller supplied a
    /// trace context.
    initial_flags: u16,
    /// `deadline_ns` from `CallOptions::deadline`. Embedded in the
    /// initial REQUEST.
    deadline_ns: u64,
    /// Per-call semaphore for upload credits. `None` when the
    /// caller didn't opt into flow control (`request_window_initial`
    /// was `None` on the `CallOptions`).
    credit_sem: Option<Arc<tokio::sync::Semaphore>>,
    /// Background task that drains REQUEST_GRANT credits from the
    /// pending entry's grant mpsc into `credit_sem`. Aborted on
    /// Drop. `None` when flow control is off.
    grant_pump: Option<JoinHandle<()>>,
    /// Single-shot terminal-RESPONSE receiver. Taken by `finish`;
    /// after that `Drop` doesn't attempt to await again.
    terminal_rx: Option<tokio::sync::oneshot::Receiver<RpcResponsePayload>>,
    /// State machine. See [`ClientStreamState`].
    state: ClientStreamState,
    /// Wall-clock start (for `RpcReply::latency_ns` reporting).
    started: Instant,
    /// Observer-fire bookkeeping. Latched on terminal observation
    /// in `finish`; fired once from `Drop` so the Deck NRPC tab +
    /// every `RpcObserver` consumer sees one event per
    /// client-streaming call.
    observer: StreamingObserverState,
}

impl ClientStreamCallRaw {
    /// Server-assigned `call_id`. Useful for trace correlation /
    /// custom logging.
    pub fn call_id(&self) -> u64 {
        self.call_id
    }

    /// Whether this call is flow-controlled (caller set
    /// `CallOptions::request_window_initial`).
    pub fn flow_controlled(&self) -> bool {
        self.credit_sem.is_some()
    }

    /// Push one body chunk to the server. Encodes as the initial
    /// REQUEST (first call) or as a REQUEST_CHUNK (subsequent
    /// calls). When flow control is opted into, awaits one credit
    /// before publishing.
    ///
    /// Returns `Err(RpcError::Codec)` if called after [`Self::finish`].
    pub async fn send(&mut self, body: Bytes) -> Result<(), RpcError> {
        match self.state {
            ClientStreamState::Finishing | ClientStreamState::Done => {
                return Err(RpcError::Codec {
                    direction: CodecDirection::Encode,
                    message: "send() called after finish()".to_string(),
                });
            }
            _ => {}
        }
        // Gate on credit when flow control is opted into.
        if let Some(sem) = self.credit_sem.as_ref() {
            let permit = sem.clone().acquire_owned().await.map_err(|_| {
                RpcError::Transport(AdapterError::Connection("credit semaphore closed".into()))
            })?;
            permit.forget();
        }
        self.observer.add_request_bytes(body.len() as u32);
        match self.state {
            ClientStreamState::JustOpened => {
                // First send → initial REQUEST.
                let req = RpcRequestPayload {
                    service: self.service.clone(),
                    deadline_ns: self.deadline_ns,
                    flags: self.initial_flags,
                    headers: std::mem::take(&mut self.initial_headers),
                    body: body.to_vec(),
                };
                self.publish_initial_request(&req).await?;
                self.state = ClientStreamState::Sending;
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.call_id,
                    flags: 0,
                    headers: vec![],
                    body: body.to_vec(),
                };
                publish_request_chunk(
                    &self.mesh,
                    self.target_node_id,
                    &self.request_channel,
                    self.self_origin,
                    &chunk,
                )
                .await?;
            }
            ClientStreamState::Finishing | ClientStreamState::Done => unreachable!(),
        }
        Ok(())
    }

    /// Close the upload direction and await the server's terminal
    /// RESPONSE. Emits a REQUEST_CHUNK with `FLAG_RPC_REQUEST_END`
    /// (empty body) if the call has already published its initial
    /// REQUEST, or an initial REQUEST with both
    /// `FLAG_RPC_CLIENT_STREAMING_REQUEST` and
    /// `FLAG_RPC_REQUEST_END` set (the degenerate "zero-item
    /// upload" path) if nothing was sent.
    ///
    /// Consumes the handle — Drop after `finish` is a no-op.
    pub async fn finish(mut self) -> Result<RpcReply, RpcError> {
        match self.state {
            ClientStreamState::JustOpened => {
                let req = RpcRequestPayload {
                    service: self.service.clone(),
                    deadline_ns: self.deadline_ns,
                    flags: self.initial_flags | FLAG_RPC_REQUEST_END,
                    headers: std::mem::take(&mut self.initial_headers),
                    body: vec![],
                };
                self.publish_initial_request(&req).await?;
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.call_id,
                    flags: FLAG_RPC_REQUEST_END,
                    headers: vec![],
                    body: vec![],
                };
                publish_request_chunk(
                    &self.mesh,
                    self.target_node_id,
                    &self.request_channel,
                    self.self_origin,
                    &chunk,
                )
                .await?;
            }
            ClientStreamState::Finishing | ClientStreamState::Done => {
                return Err(RpcError::Codec {
                    direction: CodecDirection::Encode,
                    message: "finish() called twice".to_string(),
                });
            }
        }
        self.state = ClientStreamState::Finishing;
        let terminal_rx = self.terminal_rx.take().ok_or_else(|| {
            RpcError::Transport(AdapterError::Connection(
                "terminal receiver already consumed".into(),
            ))
        })?;
        // Honor the deadline if the caller set one.
        let resp = if self.deadline_ns > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let remaining = self.deadline_ns.saturating_sub(now);
            match tokio::time::timeout(std::time::Duration::from_nanos(remaining), terminal_rx)
                .await
            {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => {
                    let msg = "terminal sender dropped before response arrived";
                    self.observer.latch_error(msg);
                    return Err(RpcError::Transport(AdapterError::Connection(msg.into())));
                }
                Err(_elapsed) => {
                    let elapsed_ms = self.started.elapsed().as_millis() as u64;
                    self.observer.latch_timeout();
                    return Err(RpcError::Timeout { elapsed_ms });
                }
            }
        } else {
            match terminal_rx.await {
                Ok(r) => r,
                Err(_) => {
                    let msg = "terminal sender dropped before response arrived";
                    self.observer.latch_error(msg);
                    return Err(RpcError::Transport(AdapterError::Connection(msg.into())));
                }
            }
        };
        self.state = ClientStreamState::Done;
        self.observer.add_response_bytes(resp.body.len() as u32);
        if !resp.status.is_ok() {
            let message = String::from_utf8(resp.body.clone())
                .unwrap_or_else(|e| format!("<{} bytes of non-utf8 body>", e.into_bytes().len()));
            self.observer.latch_error(format!(
                "server returned status {:#06x}: {message}",
                resp.status.to_wire()
            ));
            return Err(RpcError::ServerError {
                status: resp.status.to_wire(),
                message,
            });
        }
        self.observer.latch_ok();
        let latency_ns = self.started.elapsed().as_nanos() as u64;
        Ok(RpcReply {
            body: Bytes::from(resp.body),
            headers: resp.headers,
            latency_ns,
        })
    }

    async fn publish_initial_request(&self, req: &RpcRequestPayload) -> Result<(), RpcError> {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self.self_origin, self.call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + req.encoded_len());
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&req.encode());
        let request_channel_id = ChannelId::new(self.request_channel.clone());
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let payload = Bytes::from(buf);
        self.mesh
            .publish_to_peer(
                self.target_node_id,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await
            .map_err(RpcError::Transport)
    }
}

impl Drop for ClientStreamCallRaw {
    fn drop(&mut self) {
        if let Some(task) = self.grant_pump.take() {
            task.abort();
        }
        // Fire the observer with whatever status was latched
        // (Ok / Error / Timeout / Canceled). Idempotent — only
        // the first call emits.
        self.observer.fire();
        if matches!(self.state, ClientStreamState::Done) {
            // Successful completion — pending entry already gone,
            // no CANCEL needed.
            return;
        }
        self.mesh.rpc_client_pending_arc().cancel(self.call_id);
        // Only fire CANCEL on the wire if the server has actually
        // seen the initial REQUEST. A `JustOpened` Drop means we
        // never published anything; no need to CANCEL a call the
        // server doesn't know about.
        if !matches!(self.state, ClientStreamState::JustOpened) {
            spawn_cancel_publish(
                Arc::clone(&self.mesh),
                self.target_node_id,
                self.request_channel.clone(),
                self.self_origin,
                self.call_id,
            );
        }
    }
}

// ============================================================================
// Phase D — caller-side duplex primitive.
// ============================================================================

/// Shared state between a `DuplexSink` and its sibling
/// `DuplexStream`. Both halves hold an `Arc<DuplexInner>`; when
/// the refcount hits zero (i.e. both halves dropped) the Drop
/// fires CANCEL to the server unless the call was cleanly closed
/// (`clean_close = true`).
struct DuplexInner {
    mesh: Arc<MeshNode>,
    target_node_id: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
    /// Whether the initial REQUEST was successfully published.
    /// `false` means we never reached the wire — no CANCEL needed
    /// (server doesn't know about the call).
    initial_sent: std::sync::atomic::AtomicBool,
    /// Set true when the call closes cleanly — terminal RESPONSE
    /// (or terminal Error) was observed on the response stream.
    /// Suppresses CANCEL-on-drop.
    clean_close: std::sync::atomic::AtomicBool,
    /// Observer-fire bookkeeping. Latched from the various
    /// terminal-observation sites (DuplexCall::next /
    /// DuplexStream::poll_next yielding End or Error); fired
    /// once on Drop. The DuplexCall / DuplexSink / DuplexStream
    /// each share access via the surrounding Arc<DuplexInner>.
    observer: StreamingObserverState,
}

impl Drop for DuplexInner {
    fn drop(&mut self) {
        self.mesh.rpc_client_pending_arc().cancel(self.call_id);
        // Fire the observer with the latched status (Ok / Error /
        // Canceled). Idempotent — only the first call emits.
        self.observer.fire();
        if self.clean_close.load(Ordering::SeqCst) {
            return;
        }
        if !self.initial_sent.load(Ordering::SeqCst) {
            return;
        }
        spawn_cancel_publish(
            Arc::clone(&self.mesh),
            self.target_node_id,
            self.request_channel.clone(),
            self.self_origin,
            self.call_id,
        );
    }
}

/// Send half of a duplex call. Push items via `send`; emit the
/// terminal REQUEST_END frame via `finish_sending`. After
/// `finish_sending` the upload side is closed but the sibling
/// `DuplexStream` continues yielding response chunks until the
/// server's terminal frame arrives.
///
/// Bidi streaming plan (Phase D).
pub struct DuplexSink {
    inner: Arc<DuplexInner>,
    service: String,
    initial_headers: Vec<(String, Vec<u8>)>,
    initial_flags: u16,
    deadline_ns: u64,
    credit_sem: Option<Arc<tokio::sync::Semaphore>>,
    grant_pump: Option<JoinHandle<()>>,
    state: ClientStreamState,
}

impl DuplexSink {
    /// Push one body chunk to the server. Same semantics as
    /// [`ClientStreamCallRaw::send`].
    pub async fn send(&mut self, body: Bytes) -> Result<(), RpcError> {
        match self.state {
            ClientStreamState::Finishing | ClientStreamState::Done => {
                return Err(RpcError::Codec {
                    direction: CodecDirection::Encode,
                    message: "send() called after finish_sending()".to_string(),
                });
            }
            _ => {}
        }
        if let Some(sem) = self.credit_sem.as_ref() {
            let permit = sem.clone().acquire_owned().await.map_err(|_| {
                RpcError::Transport(AdapterError::Connection("credit semaphore closed".into()))
            })?;
            permit.forget();
        }
        self.inner.observer.add_request_bytes(body.len() as u32);
        match self.state {
            ClientStreamState::JustOpened => {
                let req = RpcRequestPayload {
                    service: self.service.clone(),
                    deadline_ns: self.deadline_ns,
                    flags: self.initial_flags,
                    headers: std::mem::take(&mut self.initial_headers),
                    body: body.to_vec(),
                };
                self.publish_initial_request(&req).await?;
                self.inner.initial_sent.store(true, Ordering::SeqCst);
                self.state = ClientStreamState::Sending;
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.inner.call_id,
                    flags: 0,
                    headers: vec![],
                    body: body.to_vec(),
                };
                publish_request_chunk(
                    &self.inner.mesh,
                    self.inner.target_node_id,
                    &self.inner.request_channel,
                    self.inner.self_origin,
                    &chunk,
                )
                .await?;
            }
            ClientStreamState::Finishing | ClientStreamState::Done => unreachable!(),
        }
        Ok(())
    }

    /// Close the upload direction. Emits the terminal REQUEST_END
    /// frame. The response stream continues until the server's
    /// terminal RESPONSE arrives (use the sibling `DuplexStream`).
    pub async fn finish_sending(mut self) -> Result<(), RpcError> {
        match self.state {
            ClientStreamState::JustOpened => {
                let req = RpcRequestPayload {
                    service: self.service.clone(),
                    deadline_ns: self.deadline_ns,
                    flags: self.initial_flags | FLAG_RPC_REQUEST_END,
                    headers: std::mem::take(&mut self.initial_headers),
                    body: vec![],
                };
                self.publish_initial_request(&req).await?;
                self.inner.initial_sent.store(true, Ordering::SeqCst);
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.inner.call_id,
                    flags: FLAG_RPC_REQUEST_END,
                    headers: vec![],
                    body: vec![],
                };
                publish_request_chunk(
                    &self.inner.mesh,
                    self.inner.target_node_id,
                    &self.inner.request_channel,
                    self.inner.self_origin,
                    &chunk,
                )
                .await?;
            }
            ClientStreamState::Finishing | ClientStreamState::Done => {
                return Err(RpcError::Codec {
                    direction: CodecDirection::Encode,
                    message: "finish_sending() called twice".to_string(),
                });
            }
        }
        self.state = ClientStreamState::Finishing;
        Ok(())
    }

    /// Server-assigned `call_id`. Same value on the sibling
    /// `DuplexStream`.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id
    }

    /// Whether this call is flow-controlled on the upload side.
    pub fn flow_controlled(&self) -> bool {
        self.credit_sem.is_some()
    }

    async fn publish_initial_request(&self, req: &RpcRequestPayload) -> Result<(), RpcError> {
        let meta = EventMeta::new(
            DISPATCH_RPC_REQUEST,
            0,
            self.inner.self_origin,
            self.inner.call_id,
            0,
        );
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + req.encoded_len());
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&req.encode());
        let request_channel_id = ChannelId::new(self.inner.request_channel.clone());
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let payload = Bytes::from(buf);
        self.inner
            .mesh
            .publish_to_peer(
                self.inner.target_node_id,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await
            .map_err(RpcError::Transport)
    }
}

impl Drop for DuplexSink {
    fn drop(&mut self) {
        if let Some(task) = self.grant_pump.take() {
            task.abort();
        }
        // The shared DuplexInner's Drop (when refcount hits 0)
        // does the CANCEL — nothing to do here beyond aborting
        // the grant pump.
    }
}

/// Receive half of a duplex call. Implements `futures::Stream`
/// yielding `Result<Bytes, RpcError>` per inbound RESPONSE chunk.
/// EOF on terminal Ok; one final `Err(RpcError::ServerError)` on
/// terminal non-Ok.
///
/// Bidi streaming plan (Phase D).
pub struct DuplexStream {
    inner: Arc<DuplexInner>,
    chunks_rx: tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
    done: bool,
}

impl DuplexStream {
    /// Server-assigned `call_id`. Same value on the sibling
    /// `DuplexSink`.
    pub fn call_id(&self) -> u64 {
        self.inner.call_id
    }
}

impl futures::Stream for DuplexStream {
    type Item = Result<Bytes, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        match self.chunks_rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(StreamItem::Chunk(body))) => {
                self.inner.observer.add_response_bytes(body.len() as u32);
                std::task::Poll::Ready(Some(Ok(body)))
            }
            std::task::Poll::Ready(Some(StreamItem::End)) => {
                self.done = true;
                self.inner.clean_close.store(true, Ordering::SeqCst);
                self.inner.observer.latch_ok();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Some(StreamItem::Error(resp))) => {
                self.done = true;
                self.inner.clean_close.store(true, Ordering::SeqCst);
                let status = resp.status.to_wire();
                let message = String::from_utf8(resp.body).unwrap_or_else(|e| {
                    format!("<{} bytes of non-utf8 body>", e.into_bytes().len())
                });
                self.inner
                    .observer
                    .latch_error(format!("server returned status {status:#06x}: {message}"));
                std::task::Poll::Ready(Some(Err(RpcError::ServerError { status, message })))
            }
            std::task::Poll::Ready(None) => {
                self.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Caller-side handle for a duplex RPC. Combines a `DuplexSink`
/// (upload) and `DuplexStream` (download). For application code
/// that wants to encode requests in one task and decode responses
/// in another, use [`Self::into_split`] to peel off the two halves.
///
/// Bidi streaming plan (Phase D).
pub struct DuplexCallRaw {
    sink: DuplexSink,
    stream: DuplexStream,
}

impl DuplexCallRaw {
    /// Server-assigned `call_id`.
    pub fn call_id(&self) -> u64 {
        self.sink.call_id()
    }

    /// Whether the upload side is flow-controlled.
    pub fn flow_controlled(&self) -> bool {
        self.sink.flow_controlled()
    }

    /// Push one body chunk to the server. Delegates to the inner
    /// `DuplexSink::send`.
    pub async fn send(&mut self, body: Bytes) -> Result<(), RpcError> {
        self.sink.send(body).await
    }

    /// Close the upload direction. Delegates to the inner
    /// `DuplexSink::finish_sending` but keeps the receive side
    /// alive so the caller can keep polling response chunks.
    ///
    /// NOTE: consumes the sink half but not the stream half.
    /// Internally, we replace `self.sink` with a no-op
    /// placeholder so subsequent send() / finish_sending()
    /// surface a clear error (`send() after finish_sending()`).
    pub async fn finish_sending(&mut self) -> Result<(), RpcError> {
        // Take the sink out by swapping in a placeholder whose
        // state is `Done` so subsequent sends error cleanly.
        let placeholder = DuplexSink {
            inner: Arc::clone(&self.sink.inner),
            service: String::new(),
            initial_headers: Vec::new(),
            initial_flags: 0,
            deadline_ns: 0,
            credit_sem: None,
            grant_pump: None,
            state: ClientStreamState::Done,
        };
        let sink = std::mem::replace(&mut self.sink, placeholder);
        sink.finish_sending().await
    }

    /// Pull the next response chunk. `None` on terminal Ok;
    /// `Some(Err)` then `None` on terminal non-Ok. Same shape as
    /// `futures::StreamExt::next`.
    pub async fn next(&mut self) -> Option<Result<Bytes, RpcError>> {
        use futures::StreamExt;
        self.stream.next().await
    }

    /// Split into independent send / receive halves. Both halves
    /// hold an `Arc<DuplexInner>`; CANCEL fires only when BOTH
    /// halves drop without a clean close.
    pub fn into_split(self) -> (DuplexSink, DuplexStream) {
        (self.sink, self.stream)
    }
}

impl futures::Stream for DuplexCallRaw {
    type Item = Result<Bytes, RpcError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.stream).poll_next(cx)
    }
}

// ============================================================================
// Unary call: CANCEL-on-drop guard.
// ============================================================================

/// RAII guard that fires CANCEL to the server if the unary call
/// future is dropped before a response arrives. Without this, a
/// `select!`-loser future (e.g. hedge runner-up) would leave the
/// server-side handler running to completion — wasting CPU on a
/// reply nobody will read.
///
/// The guard is built *after* the REQUEST has been successfully
/// published — if the publish fails, no guard is constructed and
/// no CANCEL is sent. On the success path the call function flips
/// `completed = true` so Drop becomes a no-op (the server already
/// finished and removed its in-flight entry).
struct UnaryCallGuard {
    pending: Arc<super::cortex::RpcClientPending>,
    mesh: Arc<MeshNode>,
    target_node_id: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
    /// True after the call resolved Ok or got a definitive
    /// non-cancellable Err. Drop checks this — `false` fires
    /// CANCEL, `true` is a no-op (still removes the pending
    /// entry).
    completed: bool,
}

impl Drop for UnaryCallGuard {
    fn drop(&mut self) {
        self.pending.cancel(self.call_id);
        if !self.completed {
            spawn_cancel_publish(
                Arc::clone(&self.mesh),
                self.target_node_id,
                self.request_channel.clone(),
                self.self_origin,
                self.call_id,
            );
        }
    }
}

// ============================================================================
// Streaming/duplex observer-fire bookkeeping.
//
// The unary `MeshNode::call` fires `RpcObserver::on_call` at each
// terminal return path (see line ~2306). The streaming /
// client-streaming / duplex paths have multiple terminal points
// (poll_next sees End / Error; finish() returns; Drop without
// terminal observation). To avoid sprinkling `fire_rpc_observer_outbound`
// at every terminal site, each handle holds a
// `StreamingObserverState` that latches the terminal status on
// observation and fires exactly once on Drop. The Deck NRPC tab
// + every consumer of `RpcObserver` get one event per streaming
// / duplex call, same as for unary today.
// ============================================================================

/// Per-call observer-fire bookkeeping shared between the
/// streaming + client-streaming + duplex caller-side handles.
/// Latches terminal status on observation; `fire()` (called from
/// the handle's Drop) emits one `RpcCallEvent` with the latched
/// status (or `Canceled` if nothing latched — i.e. the handle
/// was dropped before observing its terminator).
///
/// Status discriminator:
///   0 = none latched (Drop → Canceled)
///   1 = Ok
///   2 = Error (message in `observer_msg`)
///   3 = Timeout
pub(crate) struct StreamingObserverState {
    mesh: Arc<MeshNode>,
    target_node_id: u64,
    service: String,
    started: Instant,
    request_bytes: AtomicU32,
    response_bytes: AtomicU32,
    observer_status: AtomicU8,
    observer_msg: parking_lot::Mutex<Option<String>>,
    fired: AtomicBool,
}

impl StreamingObserverState {
    pub(crate) fn new(
        mesh: Arc<MeshNode>,
        target_node_id: u64,
        service: impl Into<String>,
        request_bytes: u32,
    ) -> Self {
        Self {
            mesh,
            target_node_id,
            service: service.into(),
            started: Instant::now(),
            request_bytes: AtomicU32::new(request_bytes),
            response_bytes: AtomicU32::new(0),
            observer_status: AtomicU8::new(0),
            observer_msg: parking_lot::Mutex::new(None),
            fired: AtomicBool::new(false),
        }
    }

    pub(crate) fn add_request_bytes(&self, n: u32) {
        self.request_bytes.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn add_response_bytes(&self, n: u32) {
        self.response_bytes.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn latch_ok(&self) {
        self.observer_status.store(1, Ordering::Relaxed);
    }

    pub(crate) fn latch_error(&self, msg: impl Into<String>) {
        *self.observer_msg.lock() = Some(msg.into());
        self.observer_status.store(2, Ordering::Relaxed);
    }

    pub(crate) fn latch_timeout(&self) {
        self.observer_status.store(3, Ordering::Relaxed);
    }

    /// Fire the observer event. Idempotent — only the first call
    /// actually emits; subsequent are no-ops. Called from each
    /// streaming handle's Drop.
    pub(crate) fn fire(&self) {
        if self.fired.swap(true, Ordering::SeqCst) {
            return;
        }
        let status_code = self.observer_status.load(Ordering::Relaxed);
        let status = match status_code {
            1 => crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Ok,
            2 => {
                let msg = self.observer_msg.lock().clone().unwrap_or_default();
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(msg)
            }
            3 => crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Timeout,
            _ => crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Canceled,
        };
        self.mesh.fire_rpc_observer_outbound(
            self.target_node_id,
            &self.service,
            self.started.elapsed().as_millis() as u32,
            status,
            self.request_bytes.load(Ordering::Relaxed),
            self.response_bytes.load(Ordering::Relaxed),
        );
    }
}

/// Per-call cap on in-flight request-direction credits. Tokio's
/// `Semaphore::MAX_PERMITS` is `usize::MAX >> 3`; we cap the
/// caller-side accumulator at this value so a misbehaving server
/// can't make the caller hold an unbounded outstanding window.
/// 1M credits is already orders of magnitude beyond any sane
/// request burst — a caller sitting on 1M unconsumed credits is
/// either misconfigured or under attack.
const REQUEST_GRANT_PER_CALL_CAP: usize = 1_000_000;

/// Add `credits` to a caller-side request-direction credit
/// semaphore, capped so the accumulator never exceeds
/// [`REQUEST_GRANT_PER_CALL_CAP`]. Per-frame cap of `usize::MAX >> 4`
/// remains as a second line of defense against pathological frame
/// values.
fn add_request_grant_credits(sem: &tokio::sync::Semaphore, credits: u32) {
    if credits == 0 {
        return;
    }
    let current = sem.available_permits();
    let remaining = REQUEST_GRANT_PER_CALL_CAP.saturating_sub(current);
    let safe = (credits as usize).min(usize::MAX >> 4).min(remaining);
    if safe > 0 {
        sem.add_permits(safe);
    }
}

/// Build a coalescing REQUEST_GRANT emitter.
///
/// Naive emitters `tokio::spawn` one publish task per consumed
/// chunk, which becomes a spawn-storm + AEAD-storm under bursting.
/// This helper hands back an emitter that pushes `(caller_origin,
/// call_id, credits)` into an unbounded mpsc; a single dedicated
/// drainer task `try_recv`s the queue to drain whatever is
/// immediately available, coalesces credits per call_id, and
/// publishes ONE batched REQUEST_GRANT per call per drain cycle.
///
/// Lifecycle: the drainer task lives as long as any clone of the
/// returned emitter (mpsc sender count > 0). When the fold and all
/// in-flight handlers release the emitter, `rx.recv` returns `None`
/// and the drainer exits naturally.
fn build_request_grant_emitter(
    mesh: Arc<MeshNode>,
    service: String,
    server_origin: u64,
    diag_tag: &'static str,
) -> RpcRequestGrantEmitter {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64, u32)>();
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut summed: std::collections::HashMap<(u64, u64), u32> =
                std::collections::HashMap::new();
            let (caller, call_id, credits) = first;
            summed.insert((caller, call_id), credits);
            // Coalesce anything immediately queued behind the first
            // wake. Bounded by what the substrate has produced so
            // far; doesn't add latency since `try_recv` returns
            // immediately when the queue is empty.
            while let Ok((caller, call_id, credits)) = rx.try_recv() {
                let entry = summed.entry((caller, call_id)).or_insert(0);
                *entry = entry.saturating_add(credits);
            }
            for ((caller, call_id), credits) in summed {
                let reply_channel_name = format!("{service}.replies.{caller:016x}");
                let reply_channel = match ChannelName::new(&reply_channel_name) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            channel = %reply_channel_name,
                            tag = diag_tag,
                            "rpc grant drainer: invalid reply channel name");
                        continue;
                    }
                };
                let meta = EventMeta::new(DISPATCH_RPC_REQUEST_GRANT, 0, server_origin, call_id, 0);
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + 12);
                buf.extend_from_slice(&meta.to_bytes());
                buf.extend_from_slice(&encode_request_grant(call_id, credits));
                let publisher = ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(
                        error = %e,
                        caller_origin = format!("{:#x}", caller),
                        call_id,
                        tag = diag_tag,
                        "rpc grant drainer: REQUEST_GRANT publish failed");
                }
            }
        }
    });
    Arc::new(move |caller_origin, call_id, credits| {
        // Send failure means the drainer has exited (all sender
        // clones dropped, then we somehow cloned a stale one).
        // Treat as a no-op — the call is tearing down anyway.
        let _ = tx.send((caller_origin, call_id, credits));
    })
}

/// Shared CANCEL-publish helper: spawn a task that fires a
/// CANCEL event for `call_id` to `target` on the request channel.
/// Both [`RpcStream::Drop`] and [`UnaryCallGuard::Drop`] use it.
fn spawn_cancel_publish(
    mesh: Arc<MeshNode>,
    target: u64,
    request_channel: ChannelName,
    self_origin: u64,
    call_id: u64,
) {
    tokio::spawn(async move {
        let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, self_origin, call_id, 0);
        let request_channel_id = ChannelId::new(request_channel);
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let payload = Bytes::from(meta.to_bytes().to_vec());
        let _ = mesh
            .publish_to_peer(
                target,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await;
    });
}

// ============================================================================
// MeshNode extensions.
// ============================================================================

impl MeshNode {
    /// Register an nRPC handler for `service` on this node.
    ///
    /// Subscribes this node to `<service>.requests` (so the local
    /// `register_rpc_inbound` dispatcher feeds inbound REQUEST
    /// events into the [`RpcServerFold`]) and wires the fold's
    /// RESPONSE-emit callback to publish on
    /// `<service>.replies.<caller_origin>` via the existing
    /// pub/sub path.
    ///
    /// **Local-only registration** (Phase 1). Multi-instance
    /// services that load-balance via `SubscriptionMode::QueueGroup`
    /// require each replica to call `serve_rpc` on its own node;
    /// the mesh-level subscriber roster + `dispatch_recipients`
    /// then routes one-of-N as designed. Each replica's local
    /// `serve_rpc` must use the same service name (which becomes
    /// the queue-group identifier).
    ///
    /// Returns a [`ServeHandle`] whose Drop tears down the
    /// registration. Concurrent registrations for the same service
    /// on one node return `Err(ServeError::AlreadyServing)`.
    pub fn serve_rpc<H: RpcHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
    ) -> Result<ServeHandle, ServeError> {
        let request_channel = ChannelName::new(&format!("{service}.requests"))
            .map_err(|e| ServeError::InvalidServiceName(e.to_string()))?;
        let channel_hash = request_channel.hash();

        // Bridge: a tokio mpsc the inbound dispatcher pushes into.
        // The bridge task drains it and runs each event through
        // the fold. Bounded so a runaway publisher can't OOM the
        // server; over-cap pushes drop the inbound event (which
        // surfaces to the caller as a timeout).
        let (tx, mut rx) = mpsc::channel::<RpcInboundEvent>(1024);

        // Build the emit closure. When the handler completes, the
        // fold calls this with `(caller_origin, call_id, response)`.
        // The closure publishes a RESPONSE event on
        // `<service>.replies.<caller_origin>` via the existing
        // pub/sub path. `tokio::spawn` keeps the closure
        // synchronous (the fold doesn't await).
        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();
        let emit: RpcResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&mesh_for_emit);
            let service = service_for_emit.clone();
            tokio::spawn(async move {
                let reply_channel_name = format!("{service}.replies.{caller_origin:016x}");
                let reply_channel = match ChannelName::new(&reply_channel_name) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, channel = %reply_channel_name,
                            "rpc serve_rpc: invalid reply channel name");
                        return;
                    }
                };
                // Build the RESPONSE event envelope: 24-byte meta
                // + encoded RpcResponsePayload.
                let meta = EventMeta::new(
                    super::cortex::DISPATCH_RPC_RESPONSE,
                    0,
                    server_origin,
                    call_id,
                    0,
                );
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
                buf.extend_from_slice(&meta.to_bytes());
                buf.extend_from_slice(&resp.encode());

                let publisher = ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(error = %e, caller_origin = format!("{:#x}", caller_origin),
                        call_id, "rpc serve_rpc: response publish failed");
                }
            });
        });

        // Build the server fold and wrap it in an Arc<Mutex<...>>
        // so the bridge task can drive it (the trait takes
        // `&mut self`). Attach the per-service metrics handle so
        // the spawned handler tasks bump server-side counters.
        let metrics_handle = self.rpc_metrics_arc().for_service(service);
        let fold = Arc::new(Mutex::new(
            RpcServerFold::new(handler as Arc<dyn RpcHandler>, emit).with_metrics(metrics_handle),
        ));

        // Register the inbound dispatcher. Push into the mpsc;
        // the bridge task does the actual fold work.
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            // Best-effort send — over-cap means the bridge can't
            // keep up; drop and let the caller time out. Logging
            // here would spam.
            let _ = tx.try_send(ev);
        });
        if self
            .register_rpc_inbound(channel_hash, dispatcher)
            .is_some()
        {
            return Err(ServeError::AlreadyServing(service.to_string()));
        }

        // Spawn the bridge task. It reads inbound events, builds
        // synthetic `RedexEvent`s carrying the payload, and feeds
        // them to the fold.
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e, "rpc serve_rpc: fold apply error");
                }
            }
        });

        // Register in the local-services set so the next
        // `announce_capabilities` call merges an `nrpc:<service>`
        // tag onto the announced CapabilitySet, making this node
        // discoverable via `Mesh::find_service_nodes(service)`.
        self.rpc_local_services_arc().insert(service.to_string());

        Ok(ServeHandle {
            channel_hash,
            service: service.to_string(),
            _bridge: bridge,
            mesh: Arc::clone(self),
        })
    }

    /// Streaming variant of [`Self::serve_rpc`]. The handler
    /// receives an [`RpcResponseSink`](super::cortex::RpcResponseSink)
    /// it writes chunks to via `sink.send(body)`; returning
    /// `Ok(())` closes the stream cleanly, `Err(_)` closes with
    /// an error frame.
    ///
    /// Wire-level identical to the unary path apart from the
    /// per-chunk `nrpc-streaming` header markers
    /// (`continue` / `end`). Same auto-registration of
    /// `<service>.requests` + `<service>.replies.` prefix.
    pub fn serve_rpc_streaming<H: RpcStreamingHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
    ) -> Result<ServeHandle, ServeError> {
        let request_channel = ChannelName::new(&format!("{service}.requests"))
            .map_err(|e| ServeError::InvalidServiceName(e.to_string()))?;
        let channel_hash = request_channel.hash();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RpcInboundEvent>(1024);

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();
        // Async emit so the streaming fold's pump can `.await` each
        // publish — guarantees per-call chunk ordering on the wire.
        let emit: RpcAsyncResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&mesh_for_emit);
            let service = service_for_emit.clone();
            Box::pin(async move {
                let reply_channel_name = format!("{service}.replies.{caller_origin:016x}");
                let reply_channel = match ChannelName::new(&reply_channel_name) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, channel = %reply_channel_name,
                                "rpc serve_rpc_streaming: invalid reply channel name");
                        return;
                    }
                };
                let meta = EventMeta::new(
                    super::cortex::DISPATCH_RPC_RESPONSE,
                    0,
                    server_origin,
                    call_id,
                    0,
                );
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
                buf.extend_from_slice(&meta.to_bytes());
                buf.extend_from_slice(&resp.encode());
                let publisher = ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(error = %e,
                            caller_origin = format!("{:#x}", caller_origin),
                            call_id,
                            "rpc serve_rpc_streaming: chunk publish failed");
                }
            })
        });

        // Attach per-service metrics so the spawned handler tasks
        // + pump task bump server-side counters (including the
        // streaming-only `streaming_chunks_emitted_total`).
        let metrics_handle = self.rpc_metrics_arc().for_service(service);
        let fold = Arc::new(Mutex::new(
            RpcServerStreamingFold::new(handler as Arc<dyn RpcStreamingHandler>, emit)
                .with_metrics(metrics_handle),
        ));
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            let _ = tx.try_send(ev);
        });
        if self
            .register_rpc_inbound(channel_hash, dispatcher)
            .is_some()
        {
            return Err(ServeError::AlreadyServing(service.to_string()));
        }
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e, "rpc serve_rpc_streaming: fold apply error");
                }
            }
        });
        self.rpc_local_services_arc().insert(service.to_string());
        Ok(ServeHandle {
            channel_hash,
            service: service.to_string(),
            _bridge: bridge,
            mesh: Arc::clone(self),
        })
    }

    /// Register a client-streaming nRPC handler for `service`.
    /// Mirror of [`Self::serve_rpc_streaming`] but using the
    /// request-side fold ([`RpcStreamingRequestFold`]) — the
    /// handler receives one stream of REQUEST_CHUNK bodies and
    /// emits one terminal RESPONSE.
    ///
    /// Wires two emit callbacks:
    /// - A sync [`RpcResponseEmitter`] for the terminal RESPONSE
    ///   (single emit per call, no ordering concern).
    /// - An [`RpcRequestGrantEmitter`] for upload-direction
    ///   credit grants, which publishes [`DISPATCH_RPC_REQUEST_GRANT`]
    ///   events on the caller's reply channel.
    ///
    /// Bidi streaming plan (Phase C).
    pub fn serve_rpc_client_stream<H: RpcClientStreamingHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
    ) -> Result<ServeHandle, ServeError> {
        let request_channel = ChannelName::new(&format!("{service}.requests"))
            .map_err(|e| ServeError::InvalidServiceName(e.to_string()))?;
        let channel_hash = request_channel.hash();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RpcInboundEvent>(1024);

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();

        // Terminal RESPONSE emitter — sync because there's only
        // one RESPONSE per call (no per-call ordering concern that
        // would require an async-await between chunks).
        let emit_resp_mesh = Arc::clone(&mesh_for_emit);
        let emit_resp_service = service_for_emit.clone();
        let emit_resp: RpcResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&emit_resp_mesh);
            let service = emit_resp_service.clone();
            tokio::spawn(async move {
                let reply_channel_name = format!("{service}.replies.{caller_origin:016x}");
                let reply_channel = match ChannelName::new(&reply_channel_name) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, channel = %reply_channel_name,
                                "rpc serve_rpc_client_stream: invalid reply channel name");
                        return;
                    }
                };
                let meta = EventMeta::new(
                    super::cortex::DISPATCH_RPC_RESPONSE,
                    0,
                    server_origin,
                    call_id,
                    0,
                );
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
                buf.extend_from_slice(&meta.to_bytes());
                buf.extend_from_slice(&resp.encode());
                let publisher = ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(error = %e,
                            caller_origin = format!("{:#x}", caller_origin),
                            call_id,
                            "rpc serve_rpc_client_stream: terminal RESPONSE publish failed");
                }
            });
        });

        // REQUEST_GRANT emitter — coalesces per-chunk credits into
        // a single drainer task that batches by call_id. Avoids the
        // tokio::spawn-per-emit storm under bursting.
        let emit_grant = build_request_grant_emitter(
            Arc::clone(&mesh_for_emit),
            service_for_emit.clone(),
            server_origin,
            "serve_rpc_client_stream",
        );

        let metrics_handle = self.rpc_metrics_arc().for_service(service);
        let fold = Arc::new(Mutex::new(
            RpcStreamingRequestFold::new(handler as Arc<dyn RpcClientStreamingHandler>, emit_resp)
                .with_grant_emitter(emit_grant)
                .with_metrics(metrics_handle),
        ));
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            let _ = tx.try_send(ev);
        });
        if self
            .register_rpc_inbound(channel_hash, dispatcher)
            .is_some()
        {
            return Err(ServeError::AlreadyServing(service.to_string()));
        }
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e,
                        "rpc serve_rpc_client_stream: fold apply error");
                }
            }
        });
        self.rpc_local_services_arc().insert(service.to_string());
        Ok(ServeHandle {
            channel_hash,
            service: service.to_string(),
            _bridge: bridge,
            mesh: Arc::clone(self),
        })
    }

    /// Client-streaming variant of [`Self::call`]. Returns a
    /// [`ClientStreamCallRaw`] handle the caller pushes N items
    /// into via `send`, then `finish` to await the terminal
    /// RESPONSE.
    ///
    /// **Lazy initial REQUEST.** This method does NOT publish a
    /// REQUEST to the wire. It only ensures the caller's reply
    /// subscription is set up and registers the pending entry; the
    /// initial REQUEST is emitted by the first `send` (or by
    /// `finish` for the zero-item degenerate path).
    ///
    /// Sets `FLAG_RPC_CLIENT_STREAMING_REQUEST` on the initial
    /// REQUEST so the server's request-streaming fold knows to
    /// open a request-side stream. Optional `request_window_initial`
    /// header opts into upload-direction flow control.
    ///
    /// Bidi streaming plan (Phase C).
    pub async fn call_client_stream(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        opts: CallOptions,
    ) -> Result<ClientStreamCallRaw, RpcError> {
        // `request_window_initial = Some(0)` would deadlock the
        // caller: every `send` awaits a credit, but the initial
        // REQUEST is lazy (not emitted until the first send), so
        // the server never sees the call and never publishes a
        // GRANT. Reject up front — `None` means "unbounded credit",
        // any positive value opts into flow control.
        if matches!(opts.request_window_initial, Some(0)) {
            return Err(RpcError::Codec {
                direction: CodecDirection::Encode,
                message: "request_window_initial must be None or >= 1; Some(0) deadlocks send"
                    .to_string(),
            });
        }
        let request_channel =
            ChannelName::new(&format!("{service}.requests")).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid service name: {e}"),
            })?;
        let self_origin = self.identity_origin_hash();
        let reply_channel_name = format!("{service}.replies.{self_origin:016x}");
        let reply_channel =
            ChannelName::new(&reply_channel_name).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid reply channel name: {e}"),
            })?;
        let reply_hash = reply_channel.hash();
        self.ensure_reply_subscription(target_node_id, service, reply_channel.clone(), reply_hash)
            .await?;

        let call_id = mint_random_call_id();
        let pending = self.rpc_client_pending();
        let (terminal_rx, mut grant_rx) =
            pending.register_client_streaming(call_id, target_node_id);

        // Build the header set + flags we'll queue for the initial
        // REQUEST (deferred to the first send / finish).
        let mut initial_flags = FLAG_RPC_CLIENT_STREAMING_REQUEST;
        let mut initial_headers: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(tc) = opts.trace_context.as_ref() {
            initial_flags |= FLAG_RPC_PROPAGATE_TRACE;
            initial_headers.extend(build_trace_headers(tc));
        }
        if let Some(window) = opts.request_window_initial {
            initial_headers.push((
                HEADER_NRPC_REQUEST_WINDOW_INITIAL.to_string(),
                window.to_string().into_bytes(),
            ));
        }
        initial_headers.extend(opts.request_headers.iter().cloned());

        // Per-call credit semaphore when flow control is opted in.
        // Initial permits = the caller's declared window. Refilled
        // by REQUEST_GRANT events arriving on the reply channel,
        // pumped through `grant_rx` by the spawned `grant_pump`.
        let credit_sem = opts
            .request_window_initial
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n as usize)));
        let grant_pump = credit_sem.as_ref().map(|sem| {
            let sem = Arc::clone(sem);
            tokio::spawn(async move {
                while let Some(credits) = grant_rx.recv().await {
                    add_request_grant_credits(&sem, credits);
                }
            })
        });

        let deadline_ns = opts.deadline.map(instant_to_unix_nanos).unwrap_or(0);
        let observer = StreamingObserverState::new(Arc::clone(self), target_node_id, service, 0);
        Ok(ClientStreamCallRaw {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel,
            self_origin,
            call_id,
            service: service.to_string(),
            initial_headers,
            initial_flags,
            deadline_ns,
            credit_sem,
            grant_pump,
            terminal_rx: Some(terminal_rx),
            state: ClientStreamState::JustOpened,
            started: Instant::now(),
            observer,
        })
    }

    /// Register a duplex nRPC handler for `service`. Composes
    /// [`Self::serve_rpc_client_stream`] (request-side stream)
    /// with [`Self::serve_rpc_streaming`] (response-side multi-
    /// fire emit) via [`RpcDuplexFold`].
    ///
    /// Wires THREE emit callbacks:
    /// - Async [`RpcAsyncResponseEmitter`] for response chunks +
    ///   the terminal frame (per-call ordering required because
    ///   the response side is multi-fire).
    /// - [`RpcRequestGrantEmitter`] for upload-direction credit
    ///   grants (one per consumed request chunk when flow
    ///   control is opted into).
    ///
    /// Bidi streaming plan (Phase D).
    pub fn serve_rpc_duplex<H: RpcDuplexHandler>(
        self: &Arc<Self>,
        service: &str,
        handler: Arc<H>,
    ) -> Result<ServeHandle, ServeError> {
        let request_channel = ChannelName::new(&format!("{service}.requests"))
            .map_err(|e| ServeError::InvalidServiceName(e.to_string()))?;
        let channel_hash = request_channel.hash();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RpcInboundEvent>(1024);

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();

        // Async response emitter — per-call ordering matters here
        // because the response side is multi-fire (same rationale
        // as serve_rpc_streaming).
        let emit_resp_mesh = Arc::clone(&mesh_for_emit);
        let emit_resp_service = service_for_emit.clone();
        let emit_resp: RpcAsyncResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&emit_resp_mesh);
            let service = emit_resp_service.clone();
            Box::pin(async move {
                let reply_channel_name = format!("{service}.replies.{caller_origin:016x}");
                let reply_channel = match ChannelName::new(&reply_channel_name) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, channel = %reply_channel_name,
                                "rpc serve_rpc_duplex: invalid reply channel name");
                        return;
                    }
                };
                let meta = EventMeta::new(
                    super::cortex::DISPATCH_RPC_RESPONSE,
                    0,
                    server_origin,
                    call_id,
                    0,
                );
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
                buf.extend_from_slice(&meta.to_bytes());
                buf.extend_from_slice(&resp.encode());
                let publisher = ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(error = %e,
                            caller_origin = format!("{:#x}", caller_origin),
                            call_id,
                            "rpc serve_rpc_duplex: chunk publish failed");
                }
            })
        });

        // Request-direction grant emitter — same coalescing
        // drainer shape as serve_rpc_client_stream.
        let emit_grant = build_request_grant_emitter(
            Arc::clone(&mesh_for_emit),
            service_for_emit.clone(),
            server_origin,
            "serve_rpc_duplex",
        );

        let metrics_handle = self.rpc_metrics_arc().for_service(service);
        let fold = Arc::new(Mutex::new(
            RpcDuplexFold::new(handler as Arc<dyn RpcDuplexHandler>, emit_resp)
                .with_grant_emitter(emit_grant)
                .with_metrics(metrics_handle),
        ));
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            let _ = tx.try_send(ev);
        });
        if self
            .register_rpc_inbound(channel_hash, dispatcher)
            .is_some()
        {
            return Err(ServeError::AlreadyServing(service.to_string()));
        }
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e,
                        "rpc serve_rpc_duplex: fold apply error");
                }
            }
        });
        self.rpc_local_services_arc().insert(service.to_string());
        Ok(ServeHandle {
            channel_hash,
            service: service.to_string(),
            _bridge: bridge,
            mesh: Arc::clone(self),
        })
    }

    /// Duplex variant of [`Self::call`]. Returns a
    /// [`DuplexCallRaw`] handle with both upload (`send`,
    /// `finish_sending`) and download (`next`, or impl
    /// `futures::Stream`) surfaces. Use `into_split` to peel off
    /// the two halves for the "encoder task + decoder task"
    /// shape.
    ///
    /// Initial REQUEST sets BOTH `FLAG_RPC_CLIENT_STREAMING_REQUEST`
    /// AND `FLAG_RPC_STREAMING_RESPONSE`. Lazy publish — the
    /// initial REQUEST flies on the first `send` (or on
    /// `finish_sending` for the zero-item degenerate path).
    ///
    /// Bidi streaming plan (Phase D).
    pub async fn call_duplex(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        opts: CallOptions,
    ) -> Result<DuplexCallRaw, RpcError> {
        // Same deadlock guard as `call_client_stream`: Some(0)
        // means "send must await a credit that can never arrive"
        // because the initial REQUEST is lazy.
        if matches!(opts.request_window_initial, Some(0)) {
            return Err(RpcError::Codec {
                direction: CodecDirection::Encode,
                message: "request_window_initial must be None or >= 1; Some(0) deadlocks send"
                    .to_string(),
            });
        }
        let request_channel =
            ChannelName::new(&format!("{service}.requests")).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid service name: {e}"),
            })?;
        let self_origin = self.identity_origin_hash();
        let reply_channel_name = format!("{service}.replies.{self_origin:016x}");
        let reply_channel =
            ChannelName::new(&reply_channel_name).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid reply channel name: {e}"),
            })?;
        let reply_hash = reply_channel.hash();
        self.ensure_reply_subscription(target_node_id, service, reply_channel.clone(), reply_hash)
            .await?;

        let call_id = mint_random_call_id();
        let pending = self.rpc_client_pending();
        let (chunks_rx, mut grant_rx) = pending.register_duplex(call_id, target_node_id);

        let mut initial_flags = FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE;
        let mut initial_headers: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(tc) = opts.trace_context.as_ref() {
            initial_flags |= FLAG_RPC_PROPAGATE_TRACE;
            initial_headers.extend(build_trace_headers(tc));
        }
        if let Some(window) = opts.request_window_initial {
            initial_headers.push((
                HEADER_NRPC_REQUEST_WINDOW_INITIAL.to_string(),
                window.to_string().into_bytes(),
            ));
        }
        if let Some(window) = opts.stream_window_initial {
            initial_headers.push((
                HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
                window.to_string().into_bytes(),
            ));
        }
        initial_headers.extend(opts.request_headers.iter().cloned());

        let credit_sem = opts
            .request_window_initial
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n as usize)));
        let grant_pump = credit_sem.as_ref().map(|sem| {
            let sem = Arc::clone(sem);
            tokio::spawn(async move {
                while let Some(credits) = grant_rx.recv().await {
                    add_request_grant_credits(&sem, credits);
                }
            })
        });

        let deadline_ns = opts.deadline.map(instant_to_unix_nanos).unwrap_or(0);
        let observer = StreamingObserverState::new(Arc::clone(self), target_node_id, service, 0);
        let inner = Arc::new(DuplexInner {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel,
            self_origin,
            call_id,
            initial_sent: std::sync::atomic::AtomicBool::new(false),
            clean_close: std::sync::atomic::AtomicBool::new(false),
            observer,
        });
        let sink = DuplexSink {
            inner: Arc::clone(&inner),
            service: service.to_string(),
            initial_headers,
            initial_flags,
            deadline_ns,
            credit_sem,
            grant_pump,
            state: ClientStreamState::JustOpened,
        };
        let stream = DuplexStream {
            inner,
            chunks_rx,
            done: false,
        };
        Ok(DuplexCallRaw { sink, stream })
    }

    /// Streaming variant of [`Self::call`]. Returns an
    /// [`RpcStream`] that yields chunks (as `Result<Bytes, RpcError>`)
    /// until the server closes the stream.
    ///
    /// Sets `FLAG_RPC_STREAMING_RESPONSE` on the request so the
    /// server's streaming fold knows to expect multi-fire emits.
    /// Same lazy reply-subscription + direct-unicast REQUEST
    /// as the unary `call` path.
    ///
    /// Cancellation: dropping the returned `RpcStream` emits a
    /// CANCEL to the server (best-effort) and discards any
    /// in-flight chunks.
    pub async fn call_streaming(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcStream, RpcError> {
        // `stream_window_initial = Some(0)` would deadlock the
        // RESPONSE direction by default: server's pump awaits one
        // credit per chunk, the caller's auto-grant only fires on
        // consumed chunks, and the first chunk can never be
        // delivered. `None` means "unbounded credit"; any positive
        // value opts into flow control. Reject up front — symmetric
        // with the request-direction guard in `call_client_stream`.
        if matches!(opts.stream_window_initial, Some(0)) {
            return Err(RpcError::Codec {
                direction: CodecDirection::Encode,
                message: "stream_window_initial must be None or >= 1; Some(0) deadlocks the response pump"
                    .to_string(),
            });
        }
        let request_channel =
            ChannelName::new(&format!("{service}.requests")).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid service name: {e}"),
            })?;
        let self_origin = self.identity_origin_hash();
        let reply_channel_name = format!("{service}.replies.{self_origin:016x}");
        let reply_channel =
            ChannelName::new(&reply_channel_name).map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid reply channel name: {e}"),
            })?;
        let reply_hash = reply_channel.hash();
        self.ensure_reply_subscription(target_node_id, service, reply_channel.clone(), reply_hash)
            .await?;

        let call_id = mint_random_call_id();
        let pending = self.rpc_client_pending();
        // S-4 part 2: bind the pending entry to the wire-session
        // peer the request is dispatched to. The fold's deliver
        // gate rejects RESPONSE frames whose from_node doesn't
        // match, so a leaked call_id alone can't spoof a reply.
        let rx = pending.register_streaming(call_id, target_node_id);

        // Build the REQUEST: STREAMING_RESPONSE flag plus optional
        // trace-context headers / propagate-trace flag, same as
        // unary `call`. Plus the optional flow-control header
        // (`nrpc-stream-window-initial`) when the caller opted in
        // via `CallOptions::stream_window_initial`.
        let mut flags = FLAG_RPC_STREAMING_RESPONSE;
        let mut headers = Vec::new();
        if let Some(tc) = opts.trace_context.as_ref() {
            flags |= FLAG_RPC_PROPAGATE_TRACE;
            headers.extend(build_trace_headers(tc));
        }
        if let Some(window) = opts.stream_window_initial {
            headers.push((
                HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
                window.to_string().into_bytes(),
            ));
        }
        // Append caller-supplied request headers (Phase 9b — same
        // semantics as the unary `call` path).
        headers.extend(opts.request_headers.iter().cloned());
        let req = RpcRequestPayload {
            service: service.to_string(),
            deadline_ns: opts.deadline.map(instant_to_unix_nanos).unwrap_or(0),
            flags,
            headers,
            body: payload.to_vec(),
        };
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + req.body.len() + 32);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&req.encode());

        let request_channel_id = ChannelId::new(request_channel.clone());
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let payload_bytes = Bytes::from(buf);
        if let Err(e) = self
            .publish_to_peer(
                target_node_id,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload_bytes),
            )
            .await
        {
            pending.cancel(call_id);
            return Err(RpcError::Transport(e));
        }

        let request_bytes_len = payload_bytes.len() as u32;
        Ok(RpcStream {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel,
            self_origin,
            call_id,
            inner: rx,
            done: false,
            stream_window: opts.stream_window_initial,
            observer: StreamingObserverState::new(
                Arc::clone(self),
                target_node_id,
                service,
                request_bytes_len,
            ),
        })
    }

    /// Find every node currently advertising `service` via the
    /// `nrpc:<service>` capability tag. Returns node IDs in
    /// roster order; the caller picks one (or use [`Self::call_service`]
    /// for the round-robin shortcut).
    ///
    /// Pre-Phase 2: requires the target nodes to have called
    /// `serve_rpc` AND `announce_capabilities` so the
    /// `nrpc:<service>` tag has propagated through capability
    /// announcements. The local node's own services are NOT
    /// automatically included (callers don't typically invoke
    /// themselves via the network — for in-process invocation,
    /// the user has the handler directly).
    pub fn find_service_nodes(&self, service: &str) -> Vec<u64> {
        use crate::adapter::net::behavior::capability::CapabilityFilter;
        let tag = format!("nrpc:{service}");
        let filter = CapabilityFilter::default().require_tag(tag);
        self.capability_index_arc().query(&filter)
    }

    /// Issue an RPC call to `service`, picking one node from
    /// those advertising the `nrpc:<service>` tag in the local
    /// capability index according to `opts.routing_policy`.
    ///
    /// Returns `RpcError::NoRoute` if no nodes advertise the
    /// service (or if `opts.filter_unhealthy` is set and every
    /// candidate is unavailable per the local `ProximityGraph`).
    pub async fn call_service(
        self: &Arc<Self>,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError> {
        let mut candidates = self.find_service_nodes(service);
        if candidates.is_empty() {
            return Err(RpcError::NoRoute {
                target: 0,
                reason: format!(
                    "no nodes advertise `nrpc:{service}` (have any servers \
                     for this service called serve_rpc + announce_capabilities?)"
                ),
            });
        }

        // Health filtering. Skip candidates the proximity graph
        // marks unhealthy (`!is_available()`). Candidates with no
        // proximity entry at all are KEPT — absence of evidence
        // is not evidence of unhealth, and a freshly-announced
        // service shouldn't be filtered just because pingwaves
        // haven't propagated yet.
        //
        // The bridge: each candidate's session-layer `node_id: u64`
        // is mapped to the entity-layer `[u8; 32]` via
        // `MeshNode::entity_id_for_node`. The proximity graph is
        // keyed on the entity id.
        if opts.filter_unhealthy {
            let proximity = self.proximity_graph();
            candidates.retain(|node_id| match self.entity_id_for_node(*node_id) {
                Some(entity_id) => match proximity.get_node(&entity_id) {
                    Some(node) => node.is_available(),
                    None => true, // no proximity data → keep
                },
                None => true, // no entity-id mapping → keep
            });
            if candidates.is_empty() {
                return Err(RpcError::NoRoute {
                    target: 0,
                    reason: format!(
                        "every node advertising `nrpc:{service}` is marked \
                         unhealthy by the local proximity graph",
                    ),
                });
            }
        }

        // Sort once so consistent-hash policies (Sticky) produce
        // a stable ordering across calls regardless of how the
        // capability index returned the candidates, and so the
        // LowestLatency-with-no-proximity-data fallback is
        // deterministic. Cheap — the candidate set is typically
        // small.
        candidates.sort_unstable();

        let target = self.select_target(&candidates, &opts.routing_policy);
        self.call(target, service, payload, opts).await
    }

    /// Select a single target from `candidates` according to
    /// `policy`. Caller has already ensured `candidates` is
    /// non-empty and sorted (so `Sticky` is consistent across
    /// calls).
    fn select_target(&self, candidates: &[u64], policy: &RoutingPolicy) -> u64 {
        match policy {
            RoutingPolicy::RoundRobin => {
                // `fetch_add(1)` on a dedicated cursor — NOT a
                // `load(call_id)` — so two concurrent
                // `call_service` invocations always observe
                // distinct values and pick distinct targets.
                let n = self
                    .rpc_round_robin_cursor_arc()
                    .fetch_add(1, Ordering::Relaxed);
                candidates[(n as usize) % candidates.len()]
            }
            RoutingPolicy::Random => {
                // Lightweight RNG via a fresh fetch_add (same
                // counter, separate per-call value) mixed through
                // xxh3. Sufficient for load distribution;
                // not cryptographically random.
                let n = self
                    .rpc_round_robin_cursor_arc()
                    .fetch_add(1, Ordering::Relaxed);
                let mixed = xxhash_rust::xxh3::xxh3_64(&n.to_le_bytes());
                candidates[(mixed as usize) % candidates.len()]
            }
            RoutingPolicy::Sticky { key } => {
                // Consistent-hash to a position in the (sorted)
                // candidate list. Same key + same candidate set =
                // same target. A change to the candidate set
                // (server failover) reshuffles roughly 1/N of keys.
                let h = xxhash_rust::xxh3::xxh3_64(&key.to_le_bytes());
                candidates[(h as usize) % candidates.len()]
            }
            RoutingPolicy::LowestLatency => {
                // Walk candidates, look up each via the bridge
                // → proximity graph, pick the smallest
                // `latency_us`. Candidates without a proximity
                // entry (no observed pingwave or no entity-id
                // mapping yet) are treated as `u64::MAX` so they
                // sort to the bottom — a known-fast node beats an
                // unknown one.
                //
                // Determinism on tie / no-data: `best_node` starts
                // at `candidates[0]` (the lexicographically first
                // sorted candidate), so all-ties or all-unknown
                // collapse to that consistent fallback.
                let proximity = self.proximity_graph();
                let mut best_node = candidates[0];
                let mut best_latency = u64::MAX;
                for &node_id in candidates {
                    let lat = self
                        .entity_id_for_node(node_id)
                        .and_then(|eid| proximity.get_node(&eid))
                        .map(|n| n.latency_us)
                        .unwrap_or(u64::MAX);
                    if lat < best_latency {
                        best_latency = lat;
                        best_node = node_id;
                    }
                }
                best_node
            }
        }
    }

    /// Issue an RPC call to `target_node_id` for `service`.
    ///
    /// Phase 1 — direct entity-to-entity addressing. The caller
    /// specifies which target to send to; service discovery (the
    /// "find me a healthy instance of X" lookup) is Phase 2.
    ///
    /// Lazily subscribes the local node's `RpcClientFold` to
    /// `<service>.replies.<self_origin>` from `target_node_id` on
    /// the first call to that (target, service) pair. The
    /// subscription is reused across subsequent calls.
    ///
    /// On `opts.deadline` expiring OR the future being dropped,
    /// emits a CANCEL event so the server can drop the in-flight
    /// handler.
    pub async fn call(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError> {
        // `started_total` brackets the entire call for the
        // `RpcObserver` latency field; the substrate-internal
        // `started` further down (set after the subscription
        // setup) drives the existing `RpcReply::latency_ns`
        // accounting so observers and Prometheus metrics
        // measure slightly different spans but stay consistent
        // within their own surface.
        let started_total = Instant::now();
        let request_bytes_len = payload.len() as u32;
        // Per-service route cache: one `DashMap::get(&str)` +
        // `Arc::clone` on the hot path instead of 2 `format!` +
        // 2 `ChannelName::new` + xxhash per call (T1.3 perf audit
        // — `docs/misc/PERF_AUDIT_2026_05_19_NRPC.md`).
        let route = self
            .rpc_route_for_service(service)
            .map_err(|reason| RpcError::NoRoute {
                target: target_node_id,
                reason,
            })?;
        let self_origin = self.identity_origin_hash();

        // Caller-side metrics guard. Bumps `in_flight` immediately;
        // each early-return path calls `metrics_guard.record(...)`
        // with the outcome, and Drop records the latency + bumps
        // the matching counter. A future dropped before any
        // `record(...)` call (e.g. a hedge loser) leaves the guard
        // with `outcome = None` so `in_flight` decrements but no
        // outcome is double-counted.
        let metrics_registry = self.rpc_metrics_arc();
        let mut metrics_guard = CallMetricsGuard::new(metrics_registry.for_service(service));

        // Lazy reply-channel subscription. Once per (target, service).
        // Reply channel + hash come from the cached `RpcRoute`; we
        // only `.clone()` the `ChannelName` (cheap — internally an
        // Arc<str>) instead of building it from scratch.
        if let Err(e) = self
            .ensure_reply_subscription(
                target_node_id,
                service,
                route.reply_channel.clone(),
                route.reply_hash,
            )
            .await
        {
            metrics_guard.record(CallOutcome::NoRoute);
            self.fire_rpc_observer_outbound(
                target_node_id,
                service,
                started_total.elapsed().as_millis() as u32,
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(e.to_string()),
                request_bytes_len,
                0,
            );
            return Err(e);
        }

        // Allocate a fresh call_id. Random u64 from getrandom; a
        // sequential counter would let any session peer that
        // observed one of their own call_ids predict the next-
        // allocated ids and ship spoofed RESPONSE frames on the
        // victim's reply channel. Random u64 collides with
        // probability 2^-64 per call and is unguessable from
        // another peer's perspective.
        let call_id = mint_random_call_id();

        // Register the oneshot before publishing the REQUEST so a
        // very-fast RESPONSE doesn't arrive before we're ready.
        // S-4 part 2: bind the pending entry to target_node_id so
        // the fold's deliver gate rejects spoofed RESPONSE frames
        // arriving from any other session peer.
        let pending = self.rpc_client_pending();
        let rx = pending.register(call_id, target_node_id);

        // Build the REQUEST envelope. If a trace context is set,
        // emit `traceparent` / `tracestate` headers and signal
        // via `FLAG_RPC_PROPAGATE_TRACE` so the server's fold
        // populates `RpcContext::trace_context`.
        let (flags, mut headers) = match opts.trace_context.as_ref() {
            Some(tc) => (FLAG_RPC_PROPAGATE_TRACE, build_trace_headers(tc)),
            None => (0u16, Vec::new()),
        };
        // Append caller-supplied request headers (e.g. the
        // `net-where` predicate header for Phase 9b
        // predicate-pushdown). Auto-generated headers come first
        // so name collisions resolve to caller-overrides via the
        // server-side `predicate_from_rpc_headers` first-match
        // semantics.
        headers.extend(opts.request_headers.iter().cloned());
        let req = RpcRequestPayload {
            service: service.to_string(),
            deadline_ns: opts.deadline.map(instant_to_unix_nanos).unwrap_or(0),
            flags,
            headers,
            body: payload.to_vec(),
        };
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + req.body.len() + 32);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&req.encode());

        // Send the REQUEST directly to `target_node_id` via
        // `publish_to_peer`, bypassing the local subscriber roster
        // lookup. The roster-based `Mesh::publish` would consult
        // `dispatch_recipients(channel)` against the caller's local
        // roster, which has no knowledge of who serves this service
        // (no Subscribe message ever propagated from the server back
        // to the caller — `serve_rpc` is local-only). For Phase 1
        // direct addressing we know the target, so direct-send is
        // the right primitive.
        //
        // The receiver routes via the per-channel-hash dispatcher
        // hook (channel_hash is stamped on the wire by
        // publish_to_peer).
        let started = Instant::now();
        // Request channel hash + stream_id come from the cached
        // route — no `ChannelId::new` clone + xxhash per call.
        let payload_bytes = Bytes::from(buf);
        if let Err(e) = self
            .publish_to_peer(
                target_node_id,
                route.request_channel_hash,
                route.request_stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload_bytes),
            )
            .await
        {
            pending.cancel(call_id);
            // Distinguish "I don't know how to reach this peer"
            // from a generic transport blip: when the publish path
            // surfaces a no-session error, that's NoRoute (the
            // routing layer's job, retry won't help). Other
            // transport errors stay as Transport so retry is
            // applicable.
            let err = if classify_publish_no_session(&e) {
                metrics_guard.record(CallOutcome::NoRoute);
                RpcError::NoRoute {
                    target: target_node_id,
                    reason: e.to_string(),
                }
            } else {
                metrics_guard.record(CallOutcome::Transport);
                RpcError::Transport(e)
            };
            self.fire_rpc_observer_outbound(
                target_node_id,
                service,
                started_total.elapsed().as_millis() as u32,
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(err.to_string()),
                request_bytes_len,
                0,
            );
            return Err(err);
        }

        // From here on, the REQUEST is in flight on the server.
        // Wrap the rest of the call in an RAII guard whose Drop
        // fires CANCEL if `guard.completed` isn't set — covering:
        //  - the call future being dropped mid-flight (e.g. hedge
        //    loser, select!-cancelled future, caller awaiting a
        //    `JoinHandle` that gets cancelled).
        //  - the timeout path (we leave `completed=false` so Drop
        //    handles CANCEL emission; no need for a separate
        //    `send_rpc_cancel` call).
        let mut guard = UnaryCallGuard {
            pending: Arc::clone(&pending),
            mesh: Arc::clone(self),
            target_node_id,
            request_channel: route.request_channel.clone(),
            self_origin,
            call_id,
            completed: false,
        };

        // Race the receiver against the deadline.
        let outcome: Result<Result<RpcResponsePayload, _>, tokio::time::error::Elapsed> =
            match opts.deadline {
                None => Ok(rx.await),
                Some(deadline) => {
                    let timeout_at = deadline.saturating_duration_since(Instant::now());
                    tokio::time::timeout(timeout_at, rx).await
                }
            };

        let resp = match outcome {
            Ok(Ok(resp)) => {
                guard.completed = true;
                resp
            }
            Ok(Err(_recv_err)) => {
                // Sender dropped externally — pending entry is
                // already gone (someone else removed it). Mark
                // completed so Drop doesn't fire a useless CANCEL
                // for a server that's no longer tracking this id.
                guard.completed = true;
                metrics_guard.record(CallOutcome::Transport);
                let err = RpcError::Transport(AdapterError::Connection(
                    "rpc client pending sender dropped (no response will arrive)".into(),
                ));
                self.fire_rpc_observer_outbound(
                    target_node_id,
                    service,
                    started_total.elapsed().as_millis() as u32,
                    crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(
                        err.to_string(),
                    ),
                    request_bytes_len,
                    0,
                );
                return Err(err);
            }
            Err(_elapsed) => {
                // Timeout: leave `completed=false` so Drop emits
                // CANCEL automatically; surface Timeout to caller.
                metrics_guard.record(CallOutcome::Timeout);
                self.fire_rpc_observer_outbound(
                    target_node_id,
                    service,
                    started_total.elapsed().as_millis() as u32,
                    crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Timeout,
                    request_bytes_len,
                    0,
                );
                return Err(RpcError::Timeout {
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
        };

        // Map the wire status onto the public Result type.
        if resp.status.is_ok() {
            metrics_guard.record(CallOutcome::Ok);
            let response_bytes_len = resp.body.len() as u32;
            self.fire_rpc_observer_outbound(
                target_node_id,
                service,
                started_total.elapsed().as_millis() as u32,
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Ok,
                request_bytes_len,
                response_bytes_len,
            );
            Ok(RpcReply {
                body: Bytes::from(resp.body),
                headers: resp.headers,
                latency_ns: started.elapsed().as_nanos() as u64,
            })
        } else {
            metrics_guard.record(CallOutcome::ServerError);
            let status = resp.status.to_wire();
            let response_bytes_len = resp.body.len() as u32;
            let message = String::from_utf8(resp.body)
                .unwrap_or_else(|e| format!("<{} bytes of non-utf8 body>", e.into_bytes().len()));
            self.fire_rpc_observer_outbound(
                target_node_id,
                service,
                started_total.elapsed().as_millis() as u32,
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(message.clone()),
                request_bytes_len,
                response_bytes_len,
            );
            Err(RpcError::ServerError { status, message })
        }
    }

    // ----------------------------------------------------------------
    // Internal helpers.
    // ----------------------------------------------------------------

    /// Lazy-subscribe `reply_channel` from `target_node_id` and
    /// register an inbound dispatcher that drives the per-Mesh
    /// `RpcClientFold`. Idempotent — subsequent calls for the
    /// same (target, service) pair are no-ops.
    ///
    /// **Bounded** at [`MAX_REPLY_SUBSCRIPTIONS`]: a caller talking
    /// to many short-lived (target, service) pairs would otherwise
    /// grow the registry indefinitely. Past the cap we refuse the
    /// new subscription with `NoRoute` rather than evict an
    /// existing one (eviction could rip out a healthy in-flight
    /// reply path).
    ///
    /// **Dispatcher reuse**: the reply-channel name embeds the
    /// CALLER's `self_origin`, NOT the target's, so a single
    /// caller talking to multiple servers for the same service
    /// reuses the same reply channel (same hash). We register the
    /// dispatcher only if the slot is unoccupied; subsequent
    /// (target, service) pairs that hash to the same slot are
    /// allowed to share the existing dispatcher (which routes to
    /// the same per-Mesh `pending` map regardless of target). A
    /// genuine cross-service hash collision is detected at
    /// `serve_rpc` time (the AlreadyServing path) for the server
    /// side; on the caller side here, sharing the dispatcher is
    /// the correct behavior because all RESPONSE events route
    /// through the same `RpcClientPending` keyed by `call_id`.
    async fn ensure_reply_subscription(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        reply_channel: ChannelName,
        reply_hash: ChannelHash,
    ) -> Result<(), RpcError> {
        let registry = self.rpc_reply_subscriptions_arc();
        {
            let entries = registry.lock();
            if entries
                .iter()
                .any(|(t, s)| *t == target_node_id && s == service)
            {
                return Ok(());
            }
            // Cap the registry. New entries past the cap are
            // refused — caller should reuse an existing
            // (target, service) pair or operate on fewer.
            if entries.len() >= MAX_REPLY_SUBSCRIPTIONS {
                return Err(RpcError::NoRoute {
                    target: target_node_id,
                    reason: format!(
                        "reply-subscription registry at cap ({} entries); refusing new \
                         (target={target_node_id:#x}, service={service:?}). Caller should \
                         reuse an existing target+service pair or shrink the active set.",
                        MAX_REPLY_SUBSCRIPTIONS,
                    ),
                });
            }
        }

        // Subscribe to our own reply channel from the target so the
        // target's roster has us as a subscriber when the server's
        // emit closure publishes the RESPONSE.
        self.subscribe_channel(target_node_id, reply_channel.clone())
            .await
            .map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: e.to_string(),
            })?;

        // Register the inbound dispatcher only if the slot is
        // unoccupied. The reply-channel name embeds *self_origin*,
        // not the target, so multiple targets serving the same
        // service share one reply channel + one dispatcher. The
        // existing dispatcher routes to the same per-Mesh
        // `RpcClientPending` keyed by call_id, so reuse is safe.
        if !self.rpc_inbound_dispatcher_registered(reply_hash) {
            let pending = self.rpc_client_pending();
            let fold = Arc::new(Mutex::new(RpcClientFold::new(pending)));
            // S-4 part 2: use `apply_inbound` so the wire-session
            // peer's NodeId (resolved in mesh.rs's dispatch site)
            // flows into the fold's deliver gate. The legacy
            // `RedexFold::apply` shim delivers with from_node=0,
            // which would defeat the binding.
            let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
                fold.lock().apply_inbound(&ev);
            });
            // Race-safe: a concurrent caller might have just
            // registered between our check and our insert. In that
            // case `register_rpc_inbound` returns the prior
            // dispatcher; our new fresh fold is dropped here, and
            // the prior dispatcher (which routes to the same
            // shared `pending`) keeps doing the job. No collision
            // — both folds are functionally equivalent.
            if let Some(prev) = self.register_rpc_inbound(reply_hash, dispatcher) {
                // Roll back: keep the prior dispatcher (it's
                // already wired to the same shared pending map).
                let _ = self.register_rpc_inbound(reply_hash, prev);
            }
        }

        let _ = reply_hash; // captured into the dispatcher above; surfaced for debug
        registry.lock().push((target_node_id, service.to_string()));
        Ok(())
    }
}

/// Hard cap on the number of distinct (target_node_id, service)
/// pairs the caller-side reply-subscription registry will hold.
/// Past the cap, the lazy-subscribe path inside [`MeshNode::call`]
/// refuses new entries with [`RpcError::NoRoute`]. 1024 is
/// generous for any realistic deployment — a caller that needs
/// more should reuse existing reply paths.
pub const MAX_REPLY_SUBSCRIPTIONS: usize = 1024;

/// Mint a random 64-bit call_id from `getrandom` entropy. Used as
/// the correlation token for REQUEST/RESPONSE pairing. The fold
/// keys pending oneshots on this value; any session peer with
/// publish access to the reply channel could ship a forged
/// RESPONSE if it could guess the value. Sequential u64s are
/// predictable from any peer that observes a single allocation;
/// random u64s collide with 2^-64 probability per call and are
/// independent across peers.
///
/// Falls back to a zero call_id on entropy failure — that
/// effectively disables correlation for this call (the oneshot
/// will time out) rather than panic, but in practice
/// `getrandom::fill` failure is a fatal-environment signal
/// (no `/dev/urandom`, broken syscall) and the broader stack
/// won't be functional anyway.
fn mint_random_call_id() -> u64 {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return 0;
    }
    u64::from_le_bytes(buf)
}

// ============================================================================
// Internal: tiny shims so the `serve_rpc` / `call` impls stay
// readable. The underlying state lives on `MeshNode`; these just
// rename the accessor methods locally.
// ============================================================================

impl MeshNode {
    fn rpc_client_pending(&self) -> Arc<super::cortex::RpcClientPending> {
        self.rpc_client_pending_arc()
    }
    fn identity_origin_hash(&self) -> u64 {
        self.public_key_origin_hash()
    }
}

// `proximity_graph()` is already a public accessor on MeshNode
// (see the existing `pub fn proximity_graph(&self) -> &Arc<...>`).
// `select_target` uses it directly; no shim needed.

// ============================================================================
// Errors.
// ============================================================================

/// Errors returned by [`MeshNode::serve_rpc`].
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The service name fails channel-name validation.
    #[error("invalid service name: {0}")]
    InvalidServiceName(String),
    /// A handler for this service is already registered on this
    /// node. Drop the prior `ServeHandle` to free the slot.
    #[error("already serving service `{0}` on this node")]
    AlreadyServing(String),
}

// ============================================================================
// Helpers.
// ============================================================================

/// Detect the "no session to the target node id" sub-case of
/// [`AdapterError::Connection`]. The publish path can surface
/// this through one of two messages depending on which inner
/// helper landed it:
///
///   - `"publish: no session for subscriber {hash}"` — emitted
///     by `mesh.rs::publish_to_peer` when the subscriber-roster
///     path can't find an active session.
///   - `"no session to publisher {hash}"` — emitted by the lower
///     mesh.rs send path when there's no active session to the
///     target's publisher record at all.
///
/// Both mean "I can't reach this peer". When we observe either,
/// we surface as [`RpcError::NoRoute`] rather than `Transport`
/// because retrying the same target without a session is
/// pointless and the right behavior for a routing helper is to
/// try a different target.
fn classify_publish_no_session(err: &AdapterError) -> bool {
    match err {
        AdapterError::Connection(msg) => {
            msg.contains("no session for subscriber") || msg.contains("no session to publisher")
        }
        _ => false,
    }
}

fn instant_to_unix_nanos(instant: Instant) -> u64 {
    // `Instant` is monotonic and not wall-clock — convert via the
    // delta from now plus current SystemTime. The result drifts
    // marginally with wall-clock skew but is good enough for
    // server-side deadline-already-passed short-circuits (which are
    // the only consumer of `deadline_ns`).
    let now_instant = Instant::now();
    let now_wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    if instant >= now_instant {
        let delta = instant.duration_since(now_instant);
        now_wall.saturating_add(delta.as_nanos() as u64)
    } else {
        let delta = now_instant.duration_since(instant);
        now_wall.saturating_sub(delta.as_nanos() as u64)
    }
}

#[allow(dead_code)]
fn _ensure_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ServeHandle>();
    assert_send_sync::<RpcCancellationToken>();
    assert_send_sync::<RpcContext>();
    assert_send_sync::<RpcHandlerError>();
    assert_send_sync::<RpcStatus>();
    assert_send_sync::<RpcReply>();
    assert_send_sync::<CallOptions>();
}
