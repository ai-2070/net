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
    build_trace_headers, encode_request_grant, encode_rpc_route, encode_stream_grant,
    peek_request_service, EventMeta, RpcAsyncResponseEmitter, RpcCancellationToken, RpcClientFold,
    RpcClientStreamingHandler, RpcContext, RpcDuplexFold, RpcDuplexHandler, RpcHandler,
    RpcHandlerError, RpcInboundDispatcher, RpcInboundEvent, RpcRequestChunkPayload,
    RpcRequestGrantEmitter, RpcRequestPayload, RpcResponseEmitter, RpcResponsePayload,
    RpcServerFold, RpcServerStreamingFold, RpcStatus, RpcStreamingHandler, RpcStreamingRequestFold,
    StreamItem, TraceContext, DISPATCH_RPC_CANCEL, DISPATCH_RPC_REQUEST,
    DISPATCH_RPC_REQUEST_CHUNK, DISPATCH_RPC_REQUEST_GRANT, DISPATCH_RPC_STREAM_GRANT,
    EVENT_META_SIZE, FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_PROPAGATE_TRACE,
    FLAG_RPC_REQUEST_END, FLAG_RPC_STREAMING_RESPONSE, HEADER_NRPC_REQUEST_WINDOW_INITIAL,
    HEADER_NRPC_STREAM_WINDOW_INITIAL, RPC_ROUTE_V1_SIZE,
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
    /// Caller-side cancel token. Mint via
    /// [`MeshNode::reserve_cancel_token`]; pair with
    /// [`MeshNode::cancel`] from any thread to abort the in-flight
    /// call. `None` (or `Some(0)` — the "no token" sentinel) → no
    /// cancel slot is reserved and the call has no external abort
    /// path beyond Drop-on-future-cancellation.
    ///
    /// Honored uniformly by every call shape: `call`, `call_service`,
    /// `call_streaming`, `call_client_stream`, `call_duplex`. The
    /// substrate registers the token in a per-mesh cancel registry
    /// at call construction and removes it on resolution (success,
    /// error, or Drop). A cancel that fires mid-flight surfaces to
    /// the caller as [`RpcError::Cancelled`] and emits CANCEL on
    /// the wire via the existing per-call-shape guards (UnaryCallGuard,
    /// ClientStreamCallRaw::Drop, DuplexCallRaw::Drop).
    ///
    /// Cancel-before-register is race-safe: a cancel that arrives
    /// in the gap between `reserve_cancel_token` and the call's
    /// internal register step latches a pre-cancel flag on the
    /// registry's orphan entry; the subsequent register observes
    /// it and the call short-circuits to [`RpcError::Cancelled`]
    /// without ever publishing the REQUEST.
    pub cancel_token: Option<u64>,
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
            cancel_token: None,
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
        /// Reply headers from the error response — the wire has always
        /// carried them (same frame field as success replies); the
        /// caller used to discard them here. Empty when the server
        /// attached none. The message stays the human diagnostic;
        /// headers are the structured sidecar channel (e.g. a
        /// `net-failure-schematic` verdict).
        headers: Vec<(String, Vec<u8>)>,
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
    /// v0.4 capability-auth gate denied the call. Either the
    /// target's latest `CapabilityAnnouncement` does not list
    /// the requested `nrpc:<service>` tag, or it lists the tag
    /// with allow-lists the caller does not match. See
    /// `docs/plans/CAPABILITY_AUTH_PLAN.md` §3 for the model.
    ///
    /// Raised by the caller-side gate inside
    /// [`MeshNode::call_service`] BEFORE the request hits the
    /// wire, and surfaced by the caller on receipt of a
    /// `RpcStatus::CapabilityDenied` response (the callee-side
    /// defense-in-depth path).
    #[error("capability denied: target {target:#x} does not authorize nrpc:{capability}")]
    CapabilityDenied {
        /// Target node id the gate denied.
        target: u64,
        /// Service / capability tag (without the `nrpc:` prefix)
        /// the gate denied.
        capability: String,
    },
    /// Caller-side cancellation fired via
    /// [`MeshNode::cancel`] with the call's `cancel_token`.
    /// Triggers a Drop-on-cancel CANCEL frame on the wire so the
    /// server's in-flight handler observes the cancel; the
    /// awaiting caller returns this variant. NOT retried by the
    /// default retry policy — cancellation is caller-driven and
    /// re-issuing the call defeats the point.
    #[error("call cancelled by caller")]
    Cancelled,
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
    /// OA2-E0.1: the registration id this handle owns. Drop passes
    /// it to `unregister_rpc_inbound`, which removes the dispatcher
    /// ONLY if the id still matches — so a stale handle whose
    /// registration was already torn down and replaced cannot evict
    /// the newer registration.
    registration_id: u64,
    /// Service name to remove from `rpc_local_services` on Drop.
    service: String,
    /// The bridge task. Held only so callers can introspect /
    /// detach it; Drop does NOT abort it (see struct doc-comment).
    /// Detaches naturally when the handle is dropped — the bridge
    /// exits on its own once the dispatcher's `mpsc::Sender` is
    /// dropped via `unregister_rpc_inbound`.
    _bridge: JoinHandle<()>,
    /// The per-service response drainer task (unary `serve_rpc` only;
    /// `None` for the streaming/duplex variants, which still spawn per
    /// emit). Like `_bridge`, held only to detach — it exits on its own
    /// once the emit closure (the sole `Sender` owner, dropped when the
    /// bridge task ends and the fold drops) is gone. See §8a.
    _response_drain: Option<JoinHandle<()>>,
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
        self.mesh
            .unregister_rpc_inbound(self.channel_hash, self.registration_id);
        // OA2-E0.2 P1: token-owned retirement. Remove the service tag
        // ONLY if it still belongs to THIS registration. A handle
        // preempted between the dispatcher-unregister above and here,
        // while a replacement `serve_rpc` re-registered the freed slot
        // and reinstalled the tag under a new id, must NOT evict the
        // replacement's live tag by name.
        self.mesh
            .rpc_local_services_arc()
            .remove_if(&self.service, self.registration_id);
    }
}

/// OA2-E0.2 P0 — cross-service confused-deputy guard for the serve
/// bridges.
///
/// The route discriminator (E0.2) already selected THIS service's
/// dispatcher by canonical channel hash. But the initial REQUEST
/// payload carries its own self-declared `service` field, and the
/// server folds route to their handler without re-checking it. A
/// frame physically delivered to `admin.requests` whose payload
/// names `echo` would otherwise run the admin handler under an
/// `echo` request — a cross-service confused deputy.
///
/// Returns `true` when `frame` is an initial `DISPATCH_RPC_REQUEST`
/// whose payload names a service OTHER than `expected` and must be
/// dropped BEFORE the capability gate or any fold/handler state.
/// Returns `false` for:
///   * control frames (CANCEL / CHUNK / GRANT) — they carry no
///     service and inherit the route-selected active call;
///   * a REQUEST whose payload names exactly `expected`;
///   * a REQUEST whose service field is unreadable — the fold's own
///     full decode then rejects it (`UnknownVersion`), so no handler
///     runs and the caller still gets a diagnostic (rather than a
///     silent drop here that mimics a timeout);
///   * a frame too short to even carry the `EventMeta` — the fold
///     drops those too.
fn is_cross_service_request(frame: &[u8], expected: &str) -> bool {
    let Some(meta) = (if frame.len() >= EVENT_META_SIZE {
        EventMeta::from_bytes(&frame[..EVENT_META_SIZE])
    } else {
        None
    }) else {
        return false;
    };
    if meta.dispatch != DISPATCH_RPC_REQUEST {
        return false;
    }
    match peek_request_service(frame) {
        Some(svc) => svc != expected,
        None => false,
    }
}

/// OA2-E1 (Kyra E1 audit) — cache the direct-send response
/// destination for a call ONLY when it is trustworthy.
///
/// The origin→node cache is an optimization the response emit closure
/// consults to skip the subscriber-roster fan-out. Populating it from
/// EVERY inbound frame BEFORE the equality/capability gate let a
/// forged frame poison it: an attacker whose frame claims a victim's
/// `origin_hash` (but arrives on the attacker's own session) could
/// overwrite `victim_origin → attacker_node` and redirect the
/// victim's in-flight response. This closes that:
///
/// - only the initial `DISPATCH_RPC_REQUEST` establishes routing
///   (CANCEL / CHUNK / GRANT never rewrite it);
/// - the wire-claimed `origin_hash` is cached ONLY when it equals the
///   AEAD-authenticated last-hop peer's OWN origin (`from_node`'s
///   TOFU-pinned entity) — a forged or relayed origin is refused, so
///   the cache only ever maps a real caller to its own node;
/// - callers reach this ONLY on the accept path (after the
///   service-equality check and the capability/admission gate), so a
///   denied frame never mutates routing.
///
/// An unauthenticated / loopback / relayed call simply is not cached;
/// the response falls back to the signed subscriber roster, preserving
/// public behavior without the direct-send shortcut.
fn cache_authenticated_response_destination(
    mesh: &MeshNode,
    cache: &RpcOriginNodeCache,
    inbound: &RpcInboundEvent,
) {
    let dispatch = if inbound.payload.len() >= EVENT_META_SIZE {
        EventMeta::from_bytes(&inbound.payload[..EVENT_META_SIZE]).map(|m| m.dispatch)
    } else {
        None
    };
    // Direct-session binding (E0.3): the wire-claimed `origin_hash` is
    // trusted only when it matches the AEAD-authenticated `from_node`
    // peer's OWN origin. A malicious node that stamps a victim's origin
    // on the wire header is refused here.
    let authenticated_peer_origin = mesh
        .peer_entity_id(inbound.from_node)
        .map(|e| e.origin_hash());
    if response_route_is_trustworthy(
        inbound.from_node,
        dispatch,
        inbound.origin_hash,
        authenticated_peer_origin,
    ) {
        cache.insert(inbound.origin_hash, inbound.from_node);
    }
}

/// The pure decision behind [`cache_authenticated_response_destination`]
/// (Kyra E1 audit) — factored out so it is deterministically testable
/// without a live session. A response destination is trustworthy iff:
///
/// - `from_node` is a real session (never the `0` loopback sentinel);
/// - the frame is an initial `DISPATCH_RPC_REQUEST` (control frames
///   carry no new routing and must not rewrite it);
/// - the wire-claimed `claimed_origin` equals the AEAD-authenticated
///   peer's OWN origin (`authenticated_peer_origin`) — a forged or
///   relayed origin, or an unpinned peer (`None`), is refused.
fn response_route_is_trustworthy(
    from_node: u64,
    dispatch: Option<u8>,
    claimed_origin: u64,
    authenticated_peer_origin: Option<u64>,
) -> bool {
    from_node != 0
        && dispatch == Some(DISPATCH_RPC_REQUEST)
        && authenticated_peer_origin == Some(claimed_origin)
}

/// A response ready to publish, handed from a (synchronous) `serve_rpc`
/// emit closure to the per-service response drainer task. Replaces the
/// pre-§8a `tokio::spawn`-per-response: the emit closure builds the wire
/// payload (cheap, sync) and `try_send`s this job; one drain task does the
/// `.await` publish. The reply `ChannelName` is `Arc<str>` and `payload` is
/// `Bytes`, so the hand-off is a couple of moves — no copy, no per-response
/// task allocation/scheduling.
struct RpcResponseJob {
    caller_origin: u64,
    call_id: u64,
    target_hint: Option<u64>,
    reply_channel: ChannelName,
    /// PERF_AUDIT §3.10 — cached
    /// `ChannelId::new(reply_channel).hash()`, populated by the
    /// emit closure's `reply_channel_cache` lookup. Pre-fix the
    /// drainer re-ran xxh3 over the channel name per response;
    /// the same `OriginKeyedLru` now caches the triple so a
    /// cache hit is one Arc bump + two `u64` copies.
    reply_channel_hash: ChannelHash,
    /// PERF_AUDIT §3.10 — cached
    /// `MeshNode::publish_stream_id(&reply_channel_id)`.
    reply_stream_id: u64,
    payload: Bytes,
}

/// Cached triple `(ChannelName, ChannelHash, stream_id)` for the
/// per-caller reply channel. Stored in the per-`serve_rpc`
/// `OriginKeyedLru` so each subsequent response to the same
/// caller is one Arc bump on the name + two `u64` copies — no
/// xxh3, no `publish_stream_id`.
///
/// Per PERF_AUDIT §3.10.
#[derive(Clone)]
struct CachedReplyChannel {
    name: ChannelName,
    hash: ChannelHash,
    stream_id: u64,
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
    /// Cached `ChannelId::new(request_channel).hash()`. Pre-fix
    /// `spawn_grant_publish` re-ran xxh3 over the channel name on
    /// every auto/explicit grant — per PERF_AUDIT §3.10 the value
    /// is invariant for the stream's lifetime, so we cache it once
    /// at construction.
    request_channel_hash: ChannelHash,
    /// Cached `MeshNode::publish_stream_id(&request_channel_id)`.
    /// Same reasoning as `request_channel_hash`.
    request_stream_id: u64,
    self_origin: u64,
    call_id: u64,
    inner: tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
    /// Set true once we've yielded the terminal item (or an
    /// error). Subsequent polls return `None`.
    done: bool,
    /// `Some(_)` if this stream uses flow control (caller set
    /// `CallOptions::stream_window_initial`). Auto-grant
    /// accumulates 1 credit per delivered chunk and fires one
    /// batched `spawn_grant_publish` once the accumulator reaches
    /// `window / 2` (or 1 for tiny windows). Keeps the server's
    /// pump fed at roughly the configured rate without the per-
    /// chunk spawn-storm + AEAD-storm the pre-fix path produced.
    /// `None` → no flow control; `poll_next` does not emit grants.
    /// Per PERF_AUDIT_2026_06_10_FULL_CRATE.md §3.3.
    stream_window: Option<u32>,
    /// Auto-grant accumulator: chunks delivered since the last
    /// emitted grant. Flushed at the `window / 2` threshold (see
    /// the doc on [`Self::stream_window`]).
    grant_pending: u32,
    /// Observer-fire bookkeeping. Latched on terminal observation
    /// in `poll_next`; fired once from `Drop` so the Deck NRPC
    /// tab + every other `RpcObserver` consumer sees one event
    /// per streaming-response call.
    observer: StreamingObserverState,
    /// v3 cancel-watcher keep-alive (C-S1). Dropping this field
    /// (on stream Drop) resolves the matching watcher task's
    /// oneshot receiver with `Err`, telling the watcher to exit
    /// cleanly + release the registry entry. When the call was
    /// opened without `cancel_token`, this is a placeholder sender
    /// with no watcher behind it — drop has no observable effect.
    _cancel_keep_alive: StreamCancelKeepAlive,
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
            self.request_channel_hash,
            self.request_stream_id,
            self.self_origin,
            self.call_id,
            amount,
        );
    }
}

/// PERF_AUDIT §3.3 — auto-grant coalescing decision for
/// [`RpcStream::poll_next`]. Accumulates one credit (the chunk
/// that was just delivered to the consumer) into `pending` and
/// returns `Some(amount)` when the accumulator reaches the flush
/// threshold of `window / 2` (clamped to ≥ 1 so a window of 1
/// degenerates to the pre-fix per-chunk cadence).
///
/// Liveness invariant (why no flush-on-drop / timer backstop is
/// needed): the credits left pending never exceed
/// `threshold - 1 < window`. The server starts with `window`
/// credits and `credits = window - (sent - delivered) - pending`,
/// so whenever the consumer has polled everything that was sent
/// (the only state in which it could block waiting on the server),
/// `credits = window - pending >= window - threshold + 1 >= 1` —
/// the server can always make progress. A consumer that stops
/// polling stalls the pump by design (that's flow control), and
/// the chunks already buffered in the stream's mpsc are enough to
/// carry `pending` across the threshold as soon as it resumes.
fn accumulate_auto_grant(pending: &mut u32, window: u32) -> Option<u32> {
    *pending = pending.saturating_add(1);
    let threshold = (window / 2).max(1);
    if *pending >= threshold {
        let amount = *pending;
        *pending = 0;
        Some(amount)
    } else {
        None
    }
}

/// Shared fire-and-forget GRANT-publish helper. Used by
/// [`RpcStream::grant`] (explicit) and the auto-grant in
/// [`RpcStream::poll_next`]. Same direct-unicast publish path as
/// [`spawn_cancel_publish`], just with a different dispatch byte
/// + a 4-byte u32 payload.
///
/// PERF_AUDIT §3.10 — takes `request_channel_hash` and
/// `request_stream_id` as pre-computed inputs (cached on
/// `RpcStream`) so the per-chunk grant path doesn't re-run
/// `ChannelId::new` + xxh3 on every call.
fn spawn_grant_publish(
    mesh: Arc<MeshNode>,
    target: u64,
    request_channel_hash: ChannelHash,
    request_stream_id: u64,
    self_origin: u64,
    call_id: u64,
    amount: u32,
) {
    tokio::spawn(async move {
        let meta = EventMeta::new(DISPATCH_RPC_STREAM_GRANT, 0, self_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + 4);
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, request_channel_hash);
        buf.extend_from_slice(&encode_stream_grant(amount));
        let payload = Bytes::from(buf);
        let _ = mesh
            .publish_to_peer(
                target,
                request_channel_hash,
                request_stream_id,
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
                // Auto-grant: accumulate 1 credit per delivered
                // chunk and fire a batched `spawn_grant_publish`
                // only when the accumulator reaches `window / 2`
                // (or 1 for tiny windows). Per PERF_AUDIT §3.3 —
                // pre-fix this spawned one task + one reliable
                // AEAD packet per chunk, a spawn-storm + AEAD-
                // storm under bursting; the server side already
                // fixed the identical shape via
                // `build_request_grant_emitter` (§3.3 audit text).
                // Callers needing finer cadence still have
                // `RpcStream::grant` for explicit batches.
                if let Some(window) = self.stream_window {
                    let mut pending = self.grant_pending;
                    if let Some(amount) = accumulate_auto_grant(&mut pending, window) {
                        spawn_grant_publish(
                            Arc::clone(&self.mesh),
                            self.target_node_id,
                            self.request_channel_hash,
                            self.request_stream_id,
                            self.self_origin,
                            self.call_id,
                            amount,
                        );
                    }
                    self.grant_pending = pending;
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
                let message = String::from_utf8(resp.body.to_vec()).unwrap_or_else(|e| {
                    format!("<{} bytes of non-utf8 body>", e.into_bytes().len())
                });
                self.observer
                    .latch_error(format!("server returned status {status:#06x}: {message}"));
                std::task::Poll::Ready(Some(Err(RpcError::ServerError {
                    status,
                    message,
                    headers: resp.headers,
                })))
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
/// PERF_AUDIT §3.10 — accepts pre-computed
/// `request_channel_hash` and `request_stream_id` (cached on
/// `ClientStreamCallRaw`) so the per-chunk client-stream send path
/// doesn't re-run `ChannelId::new` + xxh3 on every chunk.
async fn publish_request_chunk(
    mesh: &Arc<MeshNode>,
    target: u64,
    request_channel_hash: ChannelHash,
    request_stream_id: u64,
    self_origin: u64,
    chunk: &RpcRequestChunkPayload,
) -> Result<(), RpcError> {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, self_origin, chunk.call_id, 0);
    let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + chunk.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut buf, request_channel_hash);
    chunk.encode_into(&mut buf);
    let payload = Bytes::from(buf);
    mesh.publish_to_peer(
        target,
        request_channel_hash,
        request_stream_id,
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
    /// PERF_AUDIT §3.10 — cached `ChannelId::new(request_channel).hash()`
    /// so per-chunk REQUEST_CHUNK publishes don't re-run xxh3.
    request_channel_hash: ChannelHash,
    /// PERF_AUDIT §3.10 — cached
    /// `MeshNode::publish_stream_id(&request_channel_id)`.
    request_stream_id: u64,
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
    /// v3 cancel-watcher keep-alive (C-S1). Dropping this field
    /// (on call Drop) tells the watcher task to exit cleanly and
    /// release the registry entry. See
    /// [`spawn_stream_cancel_watcher`] for the lifecycle.
    _cancel_keep_alive: StreamCancelKeepAlive,
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
                    body: body.clone(),
                };
                self.publish_initial_request(&req).await?;
                self.state = ClientStreamState::Sending;
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.call_id,
                    flags: 0,
                    headers: vec![],
                    body: body.clone(),
                };
                publish_request_chunk(
                    &self.mesh,
                    self.target_node_id,
                    self.request_channel_hash,
                    self.request_stream_id,
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
                    body: Bytes::new(),
                };
                self.publish_initial_request(&req).await?;
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.call_id,
                    flags: FLAG_RPC_REQUEST_END,
                    headers: vec![],
                    body: Bytes::new(),
                };
                publish_request_chunk(
                    &self.mesh,
                    self.target_node_id,
                    self.request_channel_hash,
                    self.request_stream_id,
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
            // String::from_utf8 takes `Vec<u8>`. `Bytes::to_vec()`
            // matches the prior `resp.body.clone()` semantics (full
            // copy of the body for the error-formatting path);
            // bulk-throughput improvement lives on the decode side,
            // not here.
            let message = String::from_utf8(resp.body.to_vec())
                .unwrap_or_else(|e| format!("<{} bytes of non-utf8 body>", e.into_bytes().len()));
            self.observer.latch_error(format!(
                "server returned status {:#06x}: {message}",
                resp.status.to_wire()
            ));
            return Err(RpcError::ServerError {
                status: resp.status.to_wire(),
                message,
                headers: resp.headers,
            });
        }
        self.observer.latch_ok();
        let latency_ns = self.started.elapsed().as_nanos() as u64;
        Ok(RpcReply {
            body: resp.body,
            headers: resp.headers,
            latency_ns,
        })
    }

    async fn publish_initial_request(&self, req: &RpcRequestPayload) -> Result<(), RpcError> {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self.self_origin, self.call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, self.request_channel_hash);
        req.encode_into(&mut buf);
        let payload = Bytes::from(buf);
        // PERF_AUDIT §3.10 — use the cached hash + stream_id from
        // construction; no per-publish `ChannelId::new` + xxh3.
        self.mesh
            .publish_to_peer(
                self.target_node_id,
                self.request_channel_hash,
                self.request_stream_id,
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
    /// PERF_AUDIT §3.10 — cached channel-id hash + stream id so
    /// per-chunk publishes from the upload side don't re-run
    /// `ChannelId::new` + xxh3.
    request_channel_hash: ChannelHash,
    request_stream_id: u64,
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
    /// v3 cancel-watcher keep-alive (C-S1). Lives on
    /// `Arc<DuplexInner>` so it survives `into_split` — both
    /// halves of the duplex hold the same Arc, so the watcher
    /// task exits only when BOTH halves drop (matching the
    /// Drop-fires-CANCEL semantics above). Wrapped in `Option`
    /// for `mem::take`-style construction patterns; populated
    /// once at `call_duplex` time and never cleared.
    _cancel_keep_alive: Option<StreamCancelKeepAlive>,
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
                    body: body.clone(),
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
                    body: body.clone(),
                };
                publish_request_chunk(
                    &self.inner.mesh,
                    self.inner.target_node_id,
                    self.inner.request_channel_hash,
                    self.inner.request_stream_id,
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
                    body: Bytes::new(),
                };
                self.publish_initial_request(&req).await?;
                self.inner.initial_sent.store(true, Ordering::SeqCst);
            }
            ClientStreamState::Sending => {
                let chunk = RpcRequestChunkPayload {
                    call_id: self.inner.call_id,
                    flags: FLAG_RPC_REQUEST_END,
                    headers: vec![],
                    body: Bytes::new(),
                };
                publish_request_chunk(
                    &self.inner.mesh,
                    self.inner.target_node_id,
                    self.inner.request_channel_hash,
                    self.inner.request_stream_id,
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
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.encoded_len());
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, self.inner.request_channel_hash);
        req.encode_into(&mut buf);
        let payload = Bytes::from(buf);
        // PERF_AUDIT §3.10 — cached hash + stream_id from the
        // inner `ClientStreamCallRaw`.
        self.inner
            .mesh
            .publish_to_peer(
                self.inner.target_node_id,
                self.inner.request_channel_hash,
                self.inner.request_stream_id,
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
                let message = String::from_utf8(resp.body.to_vec()).unwrap_or_else(|e| {
                    format!("<{} bytes of non-utf8 body>", e.into_bytes().len())
                });
                self.inner
                    .observer
                    .latch_error(format!("server returned status {status:#06x}: {message}"));
                std::task::Poll::Ready(Some(Err(RpcError::ServerError {
                    status,
                    message,
                    headers: resp.headers,
                })))
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
                let reply_channel_hash = reply_channel.hash();
                let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + 12);
                buf.extend_from_slice(&meta.to_bytes());
                encode_rpc_route(&mut buf, reply_channel_hash);
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

/// Per-service map from a caller's `origin_hash` (wire field) to the
/// AEAD-verified `from_node` of the session that delivered their
/// inbound REQUEST. Populated by the serve_rpc bridge tasks at
/// REQUEST-receipt time; consulted by [`publish_response_to_caller`]
/// to skip the roster fan-out on the response leg.
///
/// Lives per `serve_rpc*` registration rather than mesh-wide because
/// the source-of-truth `MeshNode::origin_hash_to_node` is only safe
/// to populate from *signed* capability announcements — populating
/// it from unsigned wire `origin_hash` fields would let any session
/// peer pre-claim arbitrary origins. This map is bridge-local and
/// only used by the matching service's response emit, so a malicious
/// peer can at most misdirect responses for THEIR own request — they
/// already could.
///
/// **Bounded** ([`OriginKeyedLru`]): the key is the wire-claimed
/// `origin_hash` and the bridge inserts it *before* the capability gate,
/// so an unbounded map would let one authed peer spray distinct origins and
/// amplify server memory. The LRU caps the footprint; eviction costs only a
/// response-path cache miss (roster fallback), never correctness.
type RpcOriginNodeCache = Arc<OriginKeyedLru<u64>>;

/// Capacity bound for the per-`serve_rpc` caller-keyed caches
/// ([`RpcOriginNodeCache`] and the §8b reply-channel cache). Sized for the
/// legitimate active-caller working set of a single service; well past it the
/// LRU evicts cold origins rather than growing without limit under a
/// crafted-origin flood. Each entry is tiny (a `u64` and, for the reply
/// cache, an `Arc<str>` channel name), so the whole bound is a few hundred KB
/// per service.
const RPC_CALLER_CACHE_CAP: usize = 4096;

/// Non-zero form of [`RPC_CALLER_CACHE_CAP`], validated at compile time so
/// `OriginKeyedLru::new` carries no runtime `unwrap`/`expect`. A zero cap
/// would fail the build here rather than panic at startup.
const RPC_CALLER_CACHE_CAP_NZ: std::num::NonZeroUsize =
    match std::num::NonZeroUsize::new(RPC_CALLER_CACHE_CAP) {
        Some(n) => n,
        None => panic!("RPC_CALLER_CACHE_CAP must be non-zero"),
    };

/// Thread-safe, bounded LRU keyed by the wire-claimed caller `origin_hash`.
///
/// Backs both [`RpcOriginNodeCache`] and the §8b reply-channel cache. Wraps
/// `lru::LruCache` (which needs `&mut` even to read, to bump the entry to
/// most-recently-used) in a `parking_lot::Mutex`. The per-response lock is
/// uncontended in the common case — one fold drives a given service — and is
/// far cheaper than the `format!` + `ChannelName` allocation / roster fan-out
/// the caches exist to avoid. Eviction is always safe: a miss just recomputes
/// the value (channel name) or falls back to the roster lookup.
struct OriginKeyedLru<V>(Mutex<lru::LruCache<u64, V>>);

impl<V: Clone> OriginKeyedLru<V> {
    fn new() -> Self {
        Self(Mutex::new(lru::LruCache::new(RPC_CALLER_CACHE_CAP_NZ)))
    }

    /// Look up `origin`, promoting it to most-recently-used on a hit.
    fn get(&self, origin: u64) -> Option<V> {
        self.0.lock().get(&origin).cloned()
    }

    /// Insert / refresh `origin`, evicting the least-recently-used entry
    /// when at capacity.
    fn insert(&self, origin: u64, value: V) {
        self.0.lock().put(origin, value);
    }
}

/// Direct-send a built RESPONSE (or streaming chunk) packet to the
/// caller's reply channel, bypassing the roster fan-out path
/// [`MeshNode::publish`] uses.
///
/// **Fast path:** when the bridge has cached the caller's
/// `from_node` (i.e. the server processed an inbound REQUEST from
/// this caller via an AEAD-authenticated session), or when the
/// caller's capability announcement has reached us, the response
/// rides `publish_to_peer` — one DashMap lookup instead of roster
/// lookup + ACL check + subnet filter + per-recipient `Vec<Bytes>`
/// allocation.
///
/// **Fallback:** when neither lookup resolves — pathological cases
/// like a test harness where the caller never announces and the
/// bridge cache is empty — fall back to [`MeshNode::publish`] via
/// the roster, matching the pre-T1.2 behavior verbatim.
/// PERF_AUDIT §3.10 — accepts pre-computed
/// `reply_channel_hash` and `reply_stream_id` so the per-response
/// path doesn't re-run `ChannelId::new` + xxh3 + `publish_stream_id`
/// on every send. The emit closure's `OriginKeyedLru<CachedReplyChannel>`
/// caches the triple per caller_origin.
async fn publish_response_to_caller(
    mesh: &MeshNode,
    caller_origin: u64,
    target_hint: Option<u64>,
    reply_channel: &ChannelName,
    reply_channel_hash: ChannelHash,
    reply_stream_id: u64,
    payload: Bytes,
) -> Result<(), AdapterError> {
    // OA2-E0.2: every server→caller frame (RESPONSE / DEADLINE /
    // REQUEST_GRANT / STREAM_GRANT built for the reply channel)
    // funnels through here, so insert the RpcRouteV1 discriminator —
    // the reply channel's canonical hash — once, centrally. The
    // caller's mesh ingress selects exactly this dispatcher.
    let payload = super::cortex::insert_rpc_route(payload, reply_channel_hash);
    let resolved = target_hint.or_else(|| mesh.get_node_by_origin_hash(caller_origin));
    if let Some(target_node_id) = resolved {
        return mesh
            .publish_to_peer(
                target_node_id,
                reply_channel_hash,
                reply_stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await;
    }
    // Fallback: roster fan-out. Reached when the caller's origin is
    // unknown to both the bridge cache AND the global reverse index.
    let publisher = ChannelPublisher::new(reply_channel.clone(), PublishConfig::default());
    mesh.publish(&publisher, payload).await.map(|_| ())
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
        // OA2-E0.2: CANCEL is meta-only (no frame payload) — the
        // RpcRouteV1 discriminator still rides so ingress selects the
        // exact request dispatcher, never a bucket-colliding sibling.
        let mut buf = meta.to_bytes().to_vec();
        encode_rpc_route(&mut buf, request_channel_hash);
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

/// Type alias for the keep-alive sender that streaming-call handles
/// store. Its purpose is *only* to signal "stream done" when the
/// handle drops: the cancel-watcher task `select!`s on the matching
/// receiver, and dropping the sender (which happens on handle Drop)
/// resolves the receiver with an `Err` so the watcher exits cleanly.
///
/// `()` payload because the signal IS the resolution; no data is
/// transmitted.
type StreamCancelKeepAlive = tokio::sync::oneshot::Sender<()>;

/// Spawn a cancel-watcher task for a streaming call (call_streaming,
/// call_client_stream, call_duplex). The watcher races
/// `cancel_notify.notified()` against the keep-alive oneshot — first
/// to fire wins. On cancel, the watcher drops the pending-streaming
/// entry (which closes the receiver's mpsc, letting the stream's
/// poll_next observe EOF), then releases the registry entry. On
/// handle Drop, the keep-alive sender drops, the oneshot resolves
/// `Err`, and the watcher exits via the done arm with a registry
/// release.
///
/// When `cancel_token == 0` (the "no token" sentinel), this is a
/// no-op: the returned sender is a placeholder whose drop has no
/// observable effect, and no task is spawned. Lets the streaming
/// call shapes always store a keep-alive on the returned handle
/// without branching on whether a token was set.
fn spawn_stream_cancel_watcher(
    cancel_notify: Arc<tokio::sync::Notify>,
    cancel_token: u64,
    cancel_registry: Arc<crate::adapter::net::cancel_registry::CancelRegistry>,
    pending: Arc<crate::adapter::net::cortex::RpcClientPending>,
    call_id: u64,
) -> StreamCancelKeepAlive {
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    if cancel_token == 0 {
        // No-op fast path. The returned sender is held by the
        // handle but never paired with a watcher, so its eventual
        // drop has no effect. Avoids spawning a task per
        // cancel-less stream.
        return done_tx;
    }
    tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = cancel_notify.notified() => {
                // Cancel fired. Drop the pending-stream entry so
                // the receiver's mpsc closes (causing the stream's
                // poll_next to observe EOF via Ready(None)). The
                // handle's Drop will then fire CANCEL on the wire
                // via its existing per-shape Drop impl.
                pending.cancel(call_id);
                cancel_registry.release(cancel_token);
            }
            _ = done_rx => {
                // Stream completed normally — sender dropped on
                // handle Drop, recv returns Err. Just release the
                // registry entry; no CANCEL emission needed (the
                // handle's Drop handles that path itself if it
                // wasn't a clean close).
                cancel_registry.release(cancel_token);
            }
        }
    });
    done_tx
}

/// One-call helper that registers a cancel-notify against the
/// caller's `opts.cancel_token` and spawns the stream cancel
/// watcher. Used by every streaming call shape (`call_streaming`,
/// `call_client_stream`, `call_duplex`) to keep their bodies free
/// of the three-step token/notify/spawn boilerplate.
///
/// When `opts.cancel_token` is `None` (or `Some(0)`), this is the
/// same no-op fast path as [`spawn_stream_cancel_watcher`].
fn arm_stream_cancel(
    mesh: &Arc<MeshNode>,
    opts: &CallOptions,
    pending: &Arc<crate::adapter::net::cortex::RpcClientPending>,
    call_id: u64,
) -> StreamCancelKeepAlive {
    let cancel_token = opts.cancel_token.unwrap_or(0);
    let cancel_notify = mesh.cancel_registry().register_notify(cancel_token);
    spawn_stream_cancel_watcher(
        cancel_notify,
        cancel_token,
        Arc::clone(mesh.cancel_registry()),
        Arc::clone(pending),
        call_id,
    )
}

/// Side-effects + return value for the unary `call`'s cancel
/// branch. Releases the registry entry, records the Transport
/// outcome on the metrics guard, fires the Canceled observer
/// event, and returns `RpcError::Cancelled`. Both the
/// no-deadline and with-deadline `select!` arms invoke this so a
/// shape change to the cancel outcome (extra metric, new field
/// on the observer event) lands in exactly one place.
fn fire_unary_cancel_outcome(
    mesh: &Arc<MeshNode>,
    metrics_guard: &mut crate::adapter::net::mesh_rpc_metrics::CallMetricsGuard,
    cancel_token: u64,
    target_node_id: u64,
    service: &str,
    started_total: Instant,
    request_bytes_len: u32,
) -> RpcError {
    mesh.cancel_registry().release(cancel_token);
    metrics_guard.record(crate::adapter::net::mesh_rpc_metrics::CallOutcome::Transport);
    mesh.fire_rpc_observer_outbound(
        target_node_id,
        service,
        started_total.elapsed().as_millis() as u32,
        crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Canceled,
        request_bytes_len,
        0,
    );
    RpcError::Cancelled
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

        // T1.2 cache: maps each caller's wire `origin_hash` to the
        // AEAD-verified `from_node` of the session that delivered
        // its REQUEST. Populated by the bridge below; consumed by
        // the emit closure so [`publish_response_to_caller`] can
        // skip the roster fan-out on the response leg.
        let origin_node_cache: RpcOriginNodeCache = Arc::new(OriginKeyedLru::new());

        // Build the emit closure. When the handler completes, the
        // fold calls this (synchronously) with `(caller_origin, call_id,
        // response)`. §8a: instead of `tokio::spawn`ing a task per response,
        // the closure builds the wire payload (cheap, no await) and hands a
        // job to a single per-service response drainer task (below), which
        // does the `.await` publish. A `tokio::spawn` per response cost
        // ~1–2 µs of scheduling on a wake-bound path; a channel send is a
        // fraction of that, and the drainer amortizes the wakeup.
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();
        let origin_node_cache_for_emit = Arc::clone(&origin_node_cache);
        // §8b reply-channel cache: the reply channel name is
        // `<service>.replies.<caller_origin:016x>` — deterministic from
        // `(service, caller_origin)`, and `service` is fixed for this
        // `serve_rpc`, so it varies only by `caller_origin`. `ChannelName` is
        // `Arc<str>`, so a cache hit is an Arc bump; this removes the per-
        // response `format!` String + `ChannelName::new` (`Arc<str>`) allocation
        // (and the per-call `service.clone()`) the emit closure used to pay on
        // every response. Keyed by the wire-claimed `caller_origin` and so
        // bounded the same way as `origin_node_cache` above — an
        // `OriginKeyedLru`, not an unbounded map, so a crafted-origin flood
        // can't amplify server memory (a miss just rebuilds the name).
        // PERF_AUDIT §3.10 — cache the triple (name, hash, stream_id)
        // per caller_origin so the per-response drainer doesn't
        // recompute xxh3 + publish_stream_id on every send.
        let reply_channel_cache: Arc<OriginKeyedLru<CachedReplyChannel>> =
            Arc::new(OriginKeyedLru::new());
        // §8a response drainer channel. Bounded like the inbound channel; a
        // full channel means the drainer can't keep up, so we drop (the
        // caller times out) rather than block the fold.
        let (resp_tx, mut resp_rx) = mpsc::channel::<RpcResponseJob>(1024);
        let emit: RpcResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let target_hint = origin_node_cache_for_emit.get(caller_origin);
            // Resolve the reply channel from cache (Arc bump on hit; one
            // `format!` + `ChannelName::new` the first time we see a caller).
            let cached = match reply_channel_cache.get(caller_origin) {
                Some(c) => c,
                None => {
                    let name = format!("{service_for_emit}.replies.{caller_origin:016x}");
                    match ChannelName::new(&name) {
                        Ok(channel_name) => {
                            // Compute hash + stream_id ONCE per caller_origin
                            // and stash them alongside the name.
                            let channel_id = ChannelId::new(channel_name.clone());
                            let triple = CachedReplyChannel {
                                hash: channel_id.hash(),
                                stream_id: MeshNode::publish_stream_id(&channel_id),
                                name: channel_name,
                            };
                            reply_channel_cache.insert(caller_origin, triple.clone());
                            triple
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, channel = %name,
                                "rpc serve_rpc: invalid reply channel name");
                            return;
                        }
                    }
                }
            };
            // Build the RESPONSE event envelope (24-byte meta + encoded
            // payload) synchronously — pure CPU, no await — then hand it to
            // the drainer.
            let meta = EventMeta::new(
                super::cortex::DISPATCH_RPC_RESPONSE,
                0,
                server_origin,
                call_id,
                0,
            );
            let mut buf = Vec::with_capacity(EVENT_META_SIZE + 64);
            buf.extend_from_slice(&meta.to_bytes());
            resp.encode_into(&mut buf);
            if resp_tx
                .try_send(RpcResponseJob {
                    caller_origin,
                    call_id,
                    target_hint,
                    reply_channel: cached.name,
                    reply_channel_hash: cached.hash,
                    reply_stream_id: cached.stream_id,
                    payload: Bytes::from(buf),
                })
                .is_err()
            {
                tracing::debug!(
                    caller_origin = format!("{:#x}", caller_origin),
                    call_id,
                    "rpc serve_rpc: response drainer at capacity; dropping response"
                );
            }
        });

        // Build the server fold and wrap it in an Arc<Mutex<...>>
        // so the bridge task can drive it (the trait takes
        // `&mut self`). Attach the per-service metrics handle so
        // the spawned handler tasks bump server-side counters.
        let metrics_handle = self.rpc_metrics_arc().for_service(service);
        // Keep a clone of the emit closure for the callee-side
        // capability-auth defense-in-depth path in the bridge
        // below — the fold owns its own clone, this one only
        // emits the `CapabilityDenied` rejection before the fold
        // sees the event.
        let emit_for_bridge = Arc::clone(&emit);
        // Clone the per-service metrics handle so the bridge can
        // bump `capability_denied_total` on gate rejection. The
        // fold's own clone (passed via `with_metrics`) handles the
        // handler-side counters; this one covers the path BEFORE
        // the handler runs, which the fold-side metrics never see.
        let metrics_for_bridge = Arc::clone(&metrics_handle);
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
        // Register the service in `rpc_local_services` and refresh
        // the self-indexed announcement BEFORE installing the
        // dispatcher so the callee-side gate (in the bridge below)
        // sees a self-announcement carrying `nrpc:<service>` the
        // moment the first inbound event lands. Without this, the
        // gate was either silently permissive (no self-ann) or
        // silently denying (self-ann from a prior
        // `announce_capabilities` that pre-dated this service's
        // registration). See `docs/misc/CODE_REVIEW_2026_05_19_CAPABILITY_AUTH.md`
        // H1 + H2.
        //
        // OA2-E0.1: register FIRST (vacant-only). A duplicate
        // `serve_rpc` now fails WITHOUT touching the incumbent and
        // WITHOUT leaving a service tag behind. The tag + self-index
        // still land before the bridge task below runs the gate, so
        // the H1/H2 visibility guarantee holds.
        let Some(registration_id) = self.register_rpc_inbound(channel_hash, dispatcher) else {
            return Err(ServeError::AlreadyServing(service.to_string()));
        };
        self.rpc_local_services_arc()
            .insert(service.to_string(), registration_id);
        self.index_self_with_local_services();

        // Spawn the bridge task. It reads inbound events, runs
        // the v0.4 capability-auth callee-side gate (defense in
        // depth — the caller-side gate inside `call_service`
        // covers the well-behaved client path), and on accept
        // feeds them to the fold.
        let mesh_for_bridge = Arc::clone(self);
        let service_for_bridge = service.to_string();
        let origin_node_cache_for_bridge = Arc::clone(&origin_node_cache);
        let bridge = tokio::spawn(async move {
            let tag = format!("nrpc:{}", service_for_bridge);
            use crate::adapter::net::behavior::fold::capability_bridge;
            while let Some(inbound) = rx.recv().await {
                // OA2-E0.2 P0: captured-service equality. An initial
                // REQUEST routed to THIS dispatcher whose payload
                // names a different service is a cross-service
                // confused deputy — drop it before the capability
                // gate or any fold/handler state. Control frames
                // inherit the route-selected active call and are not
                // re-checked (see `is_cross_service_request`).
                //
                // KC7 (Kyra E1 audit): the origin→node response-route
                // cache is populated LATER — after this equality check
                // AND the capability gate, and only for an
                // authenticated origin — so a denied/mismatched/forged
                // frame can never poison a legitimate call's response
                // routing (see `cache_authenticated_response_destination`).
                if is_cross_service_request(&inbound.payload, &service_for_bridge) {
                    continue;
                }
                // Defense-in-depth check. Skip only when the wire
                // session resolved no NodeId (`from_node == 0` is
                // the loopback / test sentinel per
                // `RpcInboundEvent::from_node` — production wire
                // delivery drops events that fail NodeId
                // resolution rather than passing 0). The cold-
                // start "no self-ann" skip the original
                // implementation carried was a permissive hole;
                // `index_self_with_local_services` above
                // guarantees a self-ann exists before the
                // dispatcher is wired, so denying when the gate
                // says no is now the safe failure mode.
                let self_node = mesh_for_bridge.node_id();
                let from_node = inbound.from_node;
                if from_node != 0
                    && !capability_bridge::may_execute(
                        mesh_for_bridge.capability_fold(),
                        self_node,
                        &tag,
                        from_node,
                    )
                {
                    // Decode the EventMeta so we can address the
                    // caller's reply channel (keyed on
                    // `caller_origin`) and tag the response with
                    // the correct `call_id`. A garbled meta means
                    // the request would have been rejected by the
                    // fold's own decode path too; drop silently
                    // to match the existing skip-on-malformed
                    // behavior there.
                    let Some(meta) = (if inbound.payload.len() >= EVENT_META_SIZE {
                        EventMeta::from_bytes(&inbound.payload[..EVENT_META_SIZE])
                    } else {
                        None
                    }) else {
                        continue;
                    };
                    let resp = super::cortex::RpcResponsePayload {
                        status: RpcStatus::CapabilityDenied,
                        headers: vec![],
                        body: Bytes::from(format!(
                            "callee-side capability-auth gate denied nrpc:{}",
                            service_for_bridge
                        )),
                    };
                    // Server-side metrics: bump `capability_denied_total`
                    // on the per-service counter. The fold-side
                    // metrics never see this path (the handler isn't
                    // invoked), so without this bump a noisy
                    // unauthorized caller is invisible to operators
                    // watching `nrpc_handler_invocations_total` —
                    // the dashboard sees "0 requests" while the
                    // caller sees `CapabilityDenied`.
                    metrics_for_bridge
                        .capability_denied_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    (emit_for_bridge)(meta.origin_hash, meta.seq_or_ts, resp);
                    continue;
                }
                // KC7: the frame passed equality + the capability gate.
                // NOW record its response destination — and only if the
                // claimed origin is the authenticated session peer's own.
                cache_authenticated_response_destination(
                    &mesh_for_bridge,
                    &origin_node_cache_for_bridge,
                    &inbound,
                );
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e, "rpc serve_rpc: fold apply error");
                }
            }
        });

        // §8a response drainer. Drains `resp_rx` and does the `.await`
        // publish that the emit closure used to `tokio::spawn` per response.
        // Exits on its own when `resp_tx` (held only by the emit closure,
        // which the fold owns) is dropped — i.e. when the bridge task ends
        // and the fold drops, the same teardown that stops `_bridge`.
        let response_drain_mesh = Arc::clone(self);
        let response_drain = tokio::spawn(async move {
            while let Some(job) = resp_rx.recv().await {
                if let Err(e) = publish_response_to_caller(
                    &response_drain_mesh,
                    job.caller_origin,
                    job.target_hint,
                    &job.reply_channel,
                    job.reply_channel_hash,
                    job.reply_stream_id,
                    job.payload,
                )
                .await
                {
                    tracing::warn!(
                        error = %e,
                        caller_origin = format!("{:#x}", job.caller_origin),
                        call_id = job.call_id,
                        "rpc serve_rpc: response publish failed"
                    );
                }
            }
        });

        // Spawn an async re-announce so peers also learn about
        // the new service without the operator having to call
        // `announce_capabilities` manually. The local self-index
        // already happened above; this is purely for peer
        // visibility (the broadcast path also re-runs the
        // self-index, which is a cheap version bump).
        let mesh_for_announce = Arc::clone(self);
        let service_for_log = service.to_string();
        tokio::spawn(async move {
            let baseline = mesh_for_announce.user_caps_snapshot();
            if let Err(e) = mesh_for_announce.announce_capabilities(baseline).await {
                tracing::warn!(
                    error = %e,
                    service = %service_for_log,
                    "serve_rpc: auto re-announce failed",
                );
            }
        });

        Ok(ServeHandle {
            channel_hash,
            registration_id,
            service: service.to_string(),
            _bridge: bridge,
            _response_drain: Some(response_drain),
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

        // T1.2 cache: bridge populates from inbound.from_node, emit
        // closure consults to skip roster fan-out. See the unary
        // serve_rpc above for the full rationale.
        let origin_node_cache: RpcOriginNodeCache = Arc::new(OriginKeyedLru::new());

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();
        let origin_node_cache_for_emit = Arc::clone(&origin_node_cache);
        // Async emit so the streaming fold's pump can `.await` each
        // publish — guarantees per-call chunk ordering on the wire.
        let emit: RpcAsyncResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&mesh_for_emit);
            let service = service_for_emit.clone();
            let target_hint = origin_node_cache_for_emit.get(caller_origin);
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
                resp.encode_into(&mut buf);
                // PERF_AUDIT §3.10: compute hash + stream_id at the
                // call site. These legacy streaming paths don't yet
                // cache the triple via `OriginKeyedLru<CachedReplyChannel>`;
                // wiring them up is a follow-up — for now the
                // compute happens here per response, same as the
                // pre-fix in-function shape.
                let reply_channel_id = ChannelId::new(reply_channel.clone());
                let reply_channel_hash = reply_channel_id.hash();
                let reply_stream_id = MeshNode::publish_stream_id(&reply_channel_id);
                if let Err(e) = publish_response_to_caller(
                    &mesh,
                    caller_origin,
                    target_hint,
                    &reply_channel,
                    reply_channel_hash,
                    reply_stream_id,
                    Bytes::from(buf),
                )
                .await
                {
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
        // Keep clones of the emit closure + metrics handle for the
        // callee-side capability-auth gate in the bridge below —
        // same defense-in-depth shape as the unary `serve_rpc`
        // bridge. The fold owns its own clones; these only serve
        // the deny path, which runs BEFORE the fold (and therefore
        // the handler) ever sees the event.
        let emit_for_bridge = Arc::clone(&emit);
        let metrics_for_bridge = Arc::clone(&metrics_handle);
        let fold = Arc::new(Mutex::new(
            RpcServerStreamingFold::new(handler as Arc<dyn RpcStreamingHandler>, emit)
                .with_metrics(metrics_handle),
        ));
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            let _ = tx.try_send(ev);
        });
        // Register the service + refresh the self-indexed
        // announcement BEFORE installing the dispatcher, exactly as
        // the unary path does: the callee-side gate reads the local
        // fold, so a self-announcement carrying `nrpc:<service>`
        // must exist the moment the first inbound event lands —
        // otherwise the gate would deny legitimate callers of a
        // just-registered service (see the unary `serve_rpc`
        // comment + `CODE_REVIEW_2026_05_19_CAPABILITY_AUTH.md`
        // H1 / H2).
        //
        // OA2-E0.1: register FIRST (vacant-only); a duplicate leaves
        // no service tag behind. The tag still lands before the
        // bridge task runs the gate.
        let Some(registration_id) = self.register_rpc_inbound(channel_hash, dispatcher) else {
            return Err(ServeError::AlreadyServing(service.to_string()));
        };
        self.rpc_local_services_arc()
            .insert(service.to_string(), registration_id);
        self.index_self_with_local_services();
        let origin_node_cache_for_bridge = Arc::clone(&origin_node_cache);
        let mesh_for_bridge = Arc::clone(self);
        let service_for_bridge = service.to_string();
        let bridge = tokio::spawn(async move {
            let tag = format!("nrpc:{}", service_for_bridge);
            use crate::adapter::net::behavior::fold::capability_bridge;
            while let Some(inbound) = rx.recv().await {
                // OA2-E0.2 P0: captured-service equality — see the
                // unary bridge. A cross-service initial REQUEST is
                // dropped before the capability gate or fold state.
                // KC7: the response-route cache is populated only after
                // the gate below, and only for an authenticated origin.
                if is_cross_service_request(&inbound.payload, &service_for_bridge) {
                    continue;
                }
                // Callee-side capability-auth gate — the streaming
                // mirror of the unary bridge's defense-in-depth
                // check (the caller-side gate inside
                // `call_service_streaming` covers the well-behaved
                // client path). Skip only when the wire session
                // resolved no NodeId (`from_node == 0` is the
                // loopback / test sentinel per
                // `RpcInboundEvent::from_node`).
                let self_node = mesh_for_bridge.node_id();
                let from_node = inbound.from_node;
                if from_node != 0
                    && !capability_bridge::may_execute(
                        mesh_for_bridge.capability_fold(),
                        self_node,
                        &tag,
                        from_node,
                    )
                {
                    // Decode the EventMeta so we can address the
                    // caller's reply channel (keyed on
                    // `caller_origin`) and tag the response with
                    // the correct `call_id`. A garbled meta would
                    // have been rejected by the fold's own decode
                    // path too; drop silently to match that
                    // skip-on-malformed behavior.
                    let Some(meta) = (if inbound.payload.len() >= EVENT_META_SIZE {
                        EventMeta::from_bytes(&inbound.payload[..EVENT_META_SIZE])
                    } else {
                        None
                    }) else {
                        continue;
                    };
                    // Terminal frame: a non-`Ok` status closes the
                    // caller's stream regardless of streaming
                    // headers (`classify_streaming_chunk`), so this
                    // single emit both denies and terminates.
                    let resp = super::cortex::RpcResponsePayload {
                        status: RpcStatus::CapabilityDenied,
                        headers: vec![],
                        body: Bytes::from(format!(
                            "callee-side capability-auth gate denied nrpc:{}",
                            service_for_bridge
                        )),
                    };
                    // Same operator-visibility bump as the unary
                    // deny path: the handler never runs, so the
                    // fold-side metrics can't count this.
                    metrics_for_bridge
                        .capability_denied_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    (emit_for_bridge)(meta.origin_hash, meta.seq_or_ts, resp).await;
                    continue;
                }
                // KC7: cache the response destination only past the gate
                // and only for an authenticated direct-session origin.
                cache_authenticated_response_destination(
                    &mesh_for_bridge,
                    &origin_node_cache_for_bridge,
                    &inbound,
                );
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e, "rpc serve_rpc_streaming: fold apply error");
                }
            }
        });
        Ok(ServeHandle {
            channel_hash,
            registration_id,
            service: service.to_string(),
            _bridge: bridge,
            // Streaming/duplex variants still spawn per emit (§8a covers the
            // unary hot path); no drainer.
            _response_drain: None,
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

        // T1.2 cache — see serve_rpc above for full rationale.
        let origin_node_cache: RpcOriginNodeCache = Arc::new(OriginKeyedLru::new());

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();

        // Terminal RESPONSE emitter — sync because there's only
        // one RESPONSE per call (no per-call ordering concern that
        // would require an async-await between chunks).
        let emit_resp_mesh = Arc::clone(&mesh_for_emit);
        let emit_resp_service = service_for_emit.clone();
        let origin_node_cache_for_emit = Arc::clone(&origin_node_cache);
        let emit_resp: RpcResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&emit_resp_mesh);
            let service = emit_resp_service.clone();
            let target_hint = origin_node_cache_for_emit.get(caller_origin);
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
                resp.encode_into(&mut buf);
                // PERF_AUDIT §3.10: compute hash + stream_id at the
                // call site. These legacy streaming paths don't yet
                // cache the triple via `OriginKeyedLru<CachedReplyChannel>`;
                // wiring them up is a follow-up — for now the
                // compute happens here per response, same as the
                // pre-fix in-function shape.
                let reply_channel_id = ChannelId::new(reply_channel.clone());
                let reply_channel_hash = reply_channel_id.hash();
                let reply_stream_id = MeshNode::publish_stream_id(&reply_channel_id);
                if let Err(e) = publish_response_to_caller(
                    &mesh,
                    caller_origin,
                    target_hint,
                    &reply_channel,
                    reply_channel_hash,
                    reply_stream_id,
                    Bytes::from(buf),
                )
                .await
                {
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
        // OA2-E0.1: vacant-only register; a duplicate fails without
        // disturbing the incumbent registration.
        let Some(registration_id) = self.register_rpc_inbound(channel_hash, dispatcher) else {
            return Err(ServeError::AlreadyServing(service.to_string()));
        };
        // OA2-E0 (Kyra E0 review): publish the token-owned service
        // registration AND refresh the self-index BEFORE the bridge is
        // exposed, so no inbound event can be processed before the
        // local registration/discovery state exists (the unary and
        // response-streaming paths already do this). The dispatcher
        // above only buffers into the mpsc; the bridge that drains it
        // is spawned LAST.
        self.rpc_local_services_arc()
            .insert(service.to_string(), registration_id);
        self.index_self_with_local_services();
        let origin_node_cache_for_bridge = Arc::clone(&origin_node_cache);
        let service_for_bridge = service.to_string();
        let mesh_for_bridge = Arc::clone(self);
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                // OA2-E0.2 P0: captured-service equality — a
                // cross-service initial REQUEST is dropped before any
                // fold state. Control frames inherit the
                // route-selected active call (see the unary bridge).
                if is_cross_service_request(&inbound.payload, &service_for_bridge) {
                    continue;
                }
                // KC7: record the response destination only after the
                // equality check and only for an authenticated
                // direct-session origin — never from a forged/denied
                // frame (see `cache_authenticated_response_destination`).
                cache_authenticated_response_destination(
                    &mesh_for_bridge,
                    &origin_node_cache_for_bridge,
                    &inbound,
                );
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e,
                        "rpc serve_rpc_client_stream: fold apply error");
                }
            }
        });
        Ok(ServeHandle {
            channel_hash,
            registration_id,
            service: service.to_string(),
            _bridge: bridge,
            // Streaming/duplex variants still spawn per emit (§8a covers the
            // unary hot path); no drainer.
            _response_drain: None,
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
        // T1.3: per-service route cache (see PERF_AUDIT
        // 2026-05-19). One DashMap::get + Arc::clone instead of
        // 2 format! + 2 ChannelName::new + xxhash per call.
        let route = self.rpc_route_or_no_route(target_node_id, service)?;
        let self_origin = self.identity_origin_hash();
        self.ensure_reply_subscription(
            target_node_id,
            service,
            route.reply_channel.clone(),
            route.reply_hash,
        )
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
        let cancel_keep_alive = arm_stream_cancel(self, &opts, &pending, call_id);
        Ok(ClientStreamCallRaw {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel: route.request_channel.clone(),
            request_channel_hash: route.request_channel_hash,
            request_stream_id: route.request_stream_id,
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
            _cancel_keep_alive: cancel_keep_alive,
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

        // T1.2 cache — see serve_rpc above for full rationale.
        let origin_node_cache: RpcOriginNodeCache = Arc::new(OriginKeyedLru::new());

        let mesh_for_emit = Arc::clone(self);
        let service_for_emit = service.to_string();
        let server_origin = self.identity_origin_hash();

        // Async response emitter — per-call ordering matters here
        // because the response side is multi-fire (same rationale
        // as serve_rpc_streaming).
        let emit_resp_mesh = Arc::clone(&mesh_for_emit);
        let emit_resp_service = service_for_emit.clone();
        let origin_node_cache_for_emit = Arc::clone(&origin_node_cache);
        let emit_resp: RpcAsyncResponseEmitter = Arc::new(move |caller_origin, call_id, resp| {
            let mesh = Arc::clone(&emit_resp_mesh);
            let service = emit_resp_service.clone();
            let target_hint = origin_node_cache_for_emit.get(caller_origin);
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
                resp.encode_into(&mut buf);
                // PERF_AUDIT §3.10: compute hash + stream_id at the
                // call site. These legacy streaming paths don't yet
                // cache the triple via `OriginKeyedLru<CachedReplyChannel>`;
                // wiring them up is a follow-up — for now the
                // compute happens here per response, same as the
                // pre-fix in-function shape.
                let reply_channel_id = ChannelId::new(reply_channel.clone());
                let reply_channel_hash = reply_channel_id.hash();
                let reply_stream_id = MeshNode::publish_stream_id(&reply_channel_id);
                if let Err(e) = publish_response_to_caller(
                    &mesh,
                    caller_origin,
                    target_hint,
                    &reply_channel,
                    reply_channel_hash,
                    reply_stream_id,
                    Bytes::from(buf),
                )
                .await
                {
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
        // OA2-E0.1: vacant-only register; a duplicate fails without
        // disturbing the incumbent registration.
        let Some(registration_id) = self.register_rpc_inbound(channel_hash, dispatcher) else {
            return Err(ServeError::AlreadyServing(service.to_string()));
        };
        // OA2-E0 (Kyra E0 review): publish + self-index BEFORE the
        // bridge is exposed (see serve_rpc_client_stream). The
        // dispatcher only buffers; the bridge drains it LAST.
        self.rpc_local_services_arc()
            .insert(service.to_string(), registration_id);
        self.index_self_with_local_services();
        let origin_node_cache_for_bridge = Arc::clone(&origin_node_cache);
        let service_for_bridge = service.to_string();
        let mesh_for_bridge = Arc::clone(self);
        let bridge = tokio::spawn(async move {
            while let Some(inbound) = rx.recv().await {
                // OA2-E0.2 P0: captured-service equality — a
                // cross-service initial REQUEST is dropped before any
                // fold state. Control frames inherit the
                // route-selected active call (see the unary bridge).
                if is_cross_service_request(&inbound.payload, &service_for_bridge) {
                    continue;
                }
                // KC7: record the response destination only after the
                // equality check and only for an authenticated
                // direct-session origin — never from a forged/denied
                // frame (see `cache_authenticated_response_destination`).
                cache_authenticated_response_destination(
                    &mesh_for_bridge,
                    &origin_node_cache_for_bridge,
                    &inbound,
                );
                let payload = inbound.payload;
                let entry = RedexEntry::new_heap(0, 0, payload.len() as u32, 0, 0);
                let ev = RedexEvent { entry, payload };
                if let Err(e) = fold.lock().apply(&ev, &mut ()) {
                    tracing::warn!(error = %e,
                        "rpc serve_rpc_duplex: fold apply error");
                }
            }
        });
        Ok(ServeHandle {
            channel_hash,
            registration_id,
            service: service.to_string(),
            _bridge: bridge,
            // Streaming/duplex variants still spawn per emit (§8a covers the
            // unary hot path); no drainer.
            _response_drain: None,
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
        // T1.3: per-service route cache (see PERF_AUDIT
        // 2026-05-19). One DashMap::get + Arc::clone instead of
        // 2 format! + 2 ChannelName::new + xxhash per call.
        let route = self.rpc_route_or_no_route(target_node_id, service)?;
        let self_origin = self.identity_origin_hash();
        self.ensure_reply_subscription(
            target_node_id,
            service,
            route.reply_channel.clone(),
            route.reply_hash,
        )
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
        // Cancel keep-alive lives on the shared Arc<DuplexInner>
        // so it survives into_split — the watcher exits only when
        // BOTH the sink AND stream halves drop, matching the
        // existing CANCEL-on-drop semantics.
        let cancel_keep_alive = arm_stream_cancel(self, &opts, &pending, call_id);
        let inner = Arc::new(DuplexInner {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel: route.request_channel.clone(),
            request_channel_hash: route.request_channel_hash,
            request_stream_id: route.request_stream_id,
            self_origin,
            call_id,
            initial_sent: std::sync::atomic::AtomicBool::new(false),
            clean_close: std::sync::atomic::AtomicBool::new(false),
            observer,
            _cancel_keep_alive: Some(cancel_keep_alive),
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
        // T1.3: per-service route cache. One DashMap::get + Arc::clone
        // on the hot path instead of 2 format! + 2 ChannelName::new +
        // xxhash per call.
        let route = self.rpc_route_or_no_route(target_node_id, service)?;
        let self_origin = self.identity_origin_hash();
        self.ensure_reply_subscription(
            target_node_id,
            service,
            route.reply_channel.clone(),
            route.reply_hash,
        )
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
            body: payload.clone(),
        };
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.body.len() + 32);
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, route.request_channel_hash);
        req.encode_into(&mut buf);

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
            return Err(RpcError::Transport(e));
        }

        let request_bytes_len = payload_bytes.len() as u32;
        // Cancel keep-alive lives on the returned RpcStream so the
        // watcher exits cleanly when the stream drops without cancel.
        let cancel_keep_alive = arm_stream_cancel(self, &opts, &pending, call_id);
        Ok(RpcStream {
            mesh: Arc::clone(self),
            target_node_id,
            request_channel: route.request_channel.clone(),
            // PERF_AUDIT §3.10 — cache the channel hash + stream
            // id from `route` so per-chunk grants in `poll_next`
            // don't re-run `ChannelId::new` + xxh3.
            request_channel_hash: route.request_channel_hash,
            request_stream_id: route.request_stream_id,
            self_origin,
            call_id,
            inner: rx,
            done: false,
            stream_window: opts.stream_window_initial,
            grant_pending: 0,
            _cancel_keep_alive: cancel_keep_alive,
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
        use crate::adapter::net::behavior::fold::capability_bridge;
        let tag = format!("nrpc:{service}");
        let filter = CapabilityFilter::default().require_tag(tag);
        capability_bridge::find_nodes_matching(self.capability_fold(), &filter)
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

        // v0.4 capability-auth caller-side gate. Filter the
        // candidate set BEFORE target selection so the routing
        // policy never picks a peer the caller can't actually
        // reach. Pre-fix `select_target` could pick a denied
        // candidate even when authorized peers existed in the
        // set, and the resulting `CapabilityDenied` masked the
        // fact that the call would have succeeded against B or
        // C. Each candidate's own announcement lists
        // `nrpc:<service>` (otherwise it wouldn't be a
        // `find_service_nodes` candidate), so the gate's
        // `has_tag` arm short-circuits in the common case; the
        // new work is the allow-list scan. Permissive
        // announcements (all three lists empty) admit any
        // caller — the byte-identity wire-compat tests pin that
        // an unmodified peer's announcement stays unrestricted.
        // See `docs/plans/CAPABILITY_AUTH_PLAN.md` §3.
        let tag = format!("nrpc:{service}");
        use crate::adapter::net::behavior::fold::capability_bridge;
        let self_id = self.node_id();
        let any_candidate = candidates[0];
        let fold = self.capability_fold();
        // PERF_AUDIT §4.2 — batch the per-candidate gate so the
        // fold read lock is taken once and the caller's subnet +
        // groups are parsed once, not N times.
        let verdicts = capability_bridge::may_execute_batch(fold, &candidates, &tag, self_id);
        let mut iter = verdicts.into_iter();
        candidates.retain(|_| iter.next().unwrap_or(false));
        if candidates.is_empty() {
            return Err(RpcError::CapabilityDenied {
                // No authorized target; surface one of the
                // originally-advertised candidates so the caller
                // can correlate the denial with a real peer. The
                // semantic is "no peer advertising `nrpc:<service>`
                // authorizes this caller" — `any_candidate` is a
                // representative, not necessarily the strictest.
                target: any_candidate,
                capability: service.to_string(),
            });
        }

        let target = self.select_target(&candidates, &opts.routing_policy);
        self.call(target, service, payload, opts).await
    }

    /// Capability-routed server-streaming call. Same routing as
    /// [`call_service`] — capability-fold lookup, health filter,
    /// routing-policy sort, capability-auth gate, target selection —
    /// but the terminal step is [`call_streaming`] instead of
    /// [`call`]. Returns the substrate's `RpcStream` so callers can
    /// drive an `async for chunk in stream:` loop.
    ///
    /// Use cases: an agent invoking a long-running tool that emits
    /// progress + a terminal result, a fan-out subscriber that wants
    /// streaming chunks from whatever node currently advertises the
    /// service, any consumer that today reaches for
    /// `find_service_nodes` → manual target selection → `call_streaming`
    /// and ends up re-implementing the cap-auth gate `call_service`
    /// already enforces.
    ///
    /// Honors `CallOptions::cancel_token` (v3) and
    /// `CallOptions::deadline` exactly like `call_streaming`.
    ///
    /// [`call_service`]: Self::call_service
    /// [`call_streaming`]: Self::call_streaming
    /// [`call`]: Self::call
    pub async fn call_service_streaming(
        self: &Arc<Self>,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcStream, RpcError> {
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

        // Health filter — mirrors `call_service`. Candidates with no
        // proximity entry are kept (absence of evidence ≠ evidence of
        // unhealth); only candidates the proximity graph marks
        // explicitly unavailable get dropped.
        if opts.filter_unhealthy {
            let proximity = self.proximity_graph();
            candidates.retain(|node_id| match self.entity_id_for_node(*node_id) {
                Some(entity_id) => match proximity.get_node(&entity_id) {
                    Some(node) => node.is_available(),
                    None => true,
                },
                None => true,
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

        // Deterministic ordering so Sticky / LowestLatency-fallback
        // pick stably across calls — mirrors `call_service`.
        candidates.sort_unstable();

        // v0.4 capability-auth caller-side gate. Same as `call_service`:
        // filter the candidate set BEFORE target selection so the
        // routing policy never picks a peer the caller can't reach.
        let tag = format!("nrpc:{service}");
        use crate::adapter::net::behavior::fold::capability_bridge;
        let self_id = self.node_id();
        let any_candidate = candidates[0];
        let fold = self.capability_fold();
        // PERF_AUDIT §4.2 — batch the per-candidate gate. See the
        // mirror site at `:3093`.
        let verdicts = capability_bridge::may_execute_batch(fold, &candidates, &tag, self_id);
        let mut iter = verdicts.into_iter();
        candidates.retain(|_| iter.next().unwrap_or(false));
        if candidates.is_empty() {
            return Err(RpcError::CapabilityDenied {
                target: any_candidate,
                capability: service.to_string(),
            });
        }

        let target = self.select_target(&candidates, &opts.routing_policy);
        self.call_streaming(target, service, payload, opts).await
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
        mut opts: CallOptions,
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
        let route = self.rpc_route_or_no_route(target_node_id, service)?;
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
        // PERF_AUDIT §3.11 — `opts` is owned by this function and
        // its `request_headers` are unused after this point;
        // `Vec::append(&mut other)` drains `other` into `headers`
        // with zero allocation, vs the pre-fix
        // `.iter().cloned()` which deep-cloned each
        // `(String, Vec<u8>)` pair into a fresh entry.
        headers.append(&mut opts.request_headers);
        let req = RpcRequestPayload {
            service: service.to_string(),
            deadline_ns: opts.deadline.map(instant_to_unix_nanos).unwrap_or(0),
            flags,
            headers,
            body: payload.clone(),
        };
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, self_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + RPC_ROUTE_V1_SIZE + req.body.len() + 32);
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, route.request_channel_hash);
        req.encode_into(&mut buf);

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
        //  - the cancel_token path (same: leave completed=false,
        //    Drop emits CANCEL).
        let mut guard = UnaryCallGuard {
            pending: Arc::clone(&pending),
            mesh: Arc::clone(self),
            target_node_id,
            request_channel: route.request_channel.clone(),
            self_origin,
            call_id,
            completed: false,
        };

        // Substrate cancel-token plumbing (v3 / C-S1). When the
        // caller set `opts.cancel_token`, register a Notify against
        // the per-mesh cancel_registry. The select! arm below
        // observes the cancel signal and short-circuits to
        // RpcError::Cancelled, leaving guard.completed = false so
        // Drop fires CANCEL on the wire. Release the registry
        // entry once the call resolves so the registry doesn't
        // grow unboundedly.
        let cancel_token = opts.cancel_token.unwrap_or(0);
        let cancel_notify = self.cancel_registry().register_notify(cancel_token);

        // Race the receiver against the deadline AND the cancel
        // signal. Each branch lifts to the same outcome shape
        // (Result<Result<RpcResponsePayload, _>, Elapsed>) so the
        // existing post-match logic stays unchanged for the ok /
        // timeout paths; the cancel arm returns early via
        // fire_unary_cancel_outcome — leaves guard.completed=false
        // so Drop emits CANCEL on the wire.
        let outcome: Result<Result<RpcResponsePayload, _>, tokio::time::error::Elapsed> =
            match opts.deadline {
                None => {
                    tokio::select! {
                        biased;
                        _ = cancel_notify.notified() => {
                            return Err(fire_unary_cancel_outcome(
                                self,
                                &mut metrics_guard,
                                cancel_token,
                                target_node_id,
                                service,
                                started_total,
                                request_bytes_len,
                            ));
                        }
                        r = rx => Ok(r),
                    }
                }
                Some(deadline) => {
                    let timeout_at = deadline.saturating_duration_since(Instant::now());
                    tokio::select! {
                        biased;
                        _ = cancel_notify.notified() => {
                            return Err(fire_unary_cancel_outcome(
                                self,
                                &mut metrics_guard,
                                cancel_token,
                                target_node_id,
                                service,
                                started_total,
                                request_bytes_len,
                            ));
                        }
                        r = tokio::time::timeout(timeout_at, rx) => r,
                    }
                }
            };

        // Whichever non-cancel path won, release the registry
        // entry. Idempotent if the cancel arm already released.
        self.cancel_registry().release(cancel_token);

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
                body: resp.body,
                headers: resp.headers,
                latency_ns: started.elapsed().as_nanos() as u64,
            })
        } else {
            metrics_guard.record(CallOutcome::ServerError);
            let status = resp.status.to_wire();
            let response_bytes_len = resp.body.len() as u32;
            let message = String::from_utf8(resp.body.to_vec())
                .unwrap_or_else(|e| format!("<{} bytes of non-utf8 body>", e.into_bytes().len()));
            self.fire_rpc_observer_outbound(
                target_node_id,
                service,
                started_total.elapsed().as_millis() as u32,
                crate::adapter::net::cortex::rpc_observer::RpcCallStatus::Error(message.clone()),
                request_bytes_len,
                response_bytes_len,
            );
            // v0.4 capability-auth: callee-side defense-in-depth
            // surfaces as a wire `CapabilityDenied` status. Map it
            // back to the typed `RpcError::CapabilityDenied` so
            // application code sees the same variant regardless of
            // which side of the gate fired.
            if matches!(resp.status, RpcStatus::CapabilityDenied) {
                return Err(RpcError::CapabilityDenied {
                    target: target_node_id,
                    capability: service.to_string(),
                });
            }
            Err(RpcError::ServerError {
                status,
                message,
                headers: resp.headers,
            })
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
        // PERF_AUDIT §3.5 — DashMap keyed by
        // `(target, xxh3_64(service))`. Pre-fix this was a global
        // `Mutex<Vec<(u64, String)>>` that every concurrent RPC
        // caller took on every call to scan the Vec with a String
        // compare per entry — all callers serialized on it. Now the
        // hot path is one shard-local read with a single String
        // compare against the slot's stored service name (xxh3 is
        // not collision-free; see `reply_subscription_covers`).
        let service_hash = xxhash_rust::xxh3::xxh3_64(service.as_bytes());
        if reply_subscription_covers(&registry, target_node_id, service_hash, service) {
            return Ok(());
        }
        // Cap the registry. `len()` on DashMap is approximate under
        // concurrent churn (it sums shard counts under shard reads,
        // not a global lock), which is exactly the semantics we
        // want here — the cap is a soft guard against a runaway
        // caller, not a precise invariant. Past the cap, new
        // entries are refused.
        if registry.len() >= MAX_REPLY_SUBSCRIPTIONS {
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
            // Race-safe via VACANT-ONLY registration (OA2-E0.1): a
            // concurrent caller might have registered between our
            // `registered` check and here. If so, `register_rpc_inbound`
            // returns `None` (occupied) and leaves the incumbent
            // dispatcher UNTOUCHED — our fresh fold is simply dropped.
            // The incumbent routes to the same shared `pending` map, so
            // reuse is correct; no restore dance is needed. We don't
            // retain the returned id — the reply dispatcher is a
            // long-lived caller-side registration with no ServeHandle.
            let _ = self.register_rpc_inbound(reply_hash, dispatcher);
        }

        let _ = reply_hash; // captured into the dispatcher above; surfaced for debug
                            // `insert` is idempotent — a concurrent caller that beat us
                            // to it just overwrote the slot with the identical value.
                            // On a genuine xxh3 collision between two service names on
                            // the same target, the slot flips to whichever service
                            // subscribed last and the other re-subscribes on its next
                            // call (idempotent, correct, merely un-cached). Cap drift
                            // past MAX_REPLY_SUBSCRIPTIONS during a concurrent insert
                            // race is bounded by the number of concurrent callers,
                            // which operators tune separately.
        registry.insert((target_node_id, service_hash), Arc::from(service));
        Ok(())
    }
}

/// PERF_AUDIT §3.5 — hot-path membership check for the
/// reply-subscription registry. Returns `true` only when the slot
/// for `(target, xxh3(service))` exists AND the stored service
/// name matches exactly. xxh3_64 is neither collision-free nor
/// cryptographic; a hash-only hit that skipped the subscribe for
/// a *different* service would silently drop that service's
/// replies — the reply channel name embeds the service, so being
/// in the target's roster for the colliding service's channel
/// does nothing for this one. Verifying the stored name turns a
/// collision into a per-call re-subscribe (idempotent, harmless)
/// instead of a correctness bug.
fn reply_subscription_covers(
    registry: &dashmap::DashMap<(u64, u64), Arc<str>>,
    target_node_id: u64,
    service_hash: u64,
    service: &str,
) -> bool {
    registry
        .get(&(target_node_id, service_hash))
        .is_some_and(|entry| entry.value().as_ref() == service)
}

/// Hard cap on the number of distinct (target_node_id, service)
/// pairs the caller-side reply-subscription registry will hold.
/// Past the cap, the lazy-subscribe path inside [`MeshNode::call`]
/// refuses new entries with [`RpcError::NoRoute`]. 1024 is
/// generous for any realistic deployment — a caller that needs
/// more should reuse existing reply paths.
pub const MAX_REPLY_SUBSCRIPTIONS: usize = 1024;

/// Mint a random 64-bit call_id. Used as the correlation token
/// for REQUEST/RESPONSE pairing. The fold keys pending oneshots on
/// this value; any session peer with publish access to the reply
/// channel could ship a forged RESPONSE if it could guess the
/// value. Sequential u64s are predictable from any peer that
/// observes a single allocation; random u64s collide with 2^-64
/// probability per call and are unpredictable to observing peers.
///
/// **PERF_AUDIT §3.8** — pre-fix this called `getrandom::fill` for
/// 8 bytes per RPC — one OS entropy syscall per call
/// (BCryptGenRandom on Windows, ~200-400 ns; somewhat cheaper on
/// Linux). Now each thread refills a small pool of raw OS entropy
/// ([`CALL_ID_ENTROPY_POOL_BYTES`]) with a single `getrandom`
/// syscall and hands out 8 bytes per call, amortizing the syscall
/// across [`CALL_ID_ENTROPY_POOL_BYTES`]/8 mints.
///
/// Every minted id is still raw OS entropy — NOT the output of a
/// userspace PRNG — so the unpredictability-to-peers property is
/// byte-for-byte identical to the pre-§3.8 per-call fill. (An
/// earlier draft of this fix streamed ids from a thread-local
/// SplitMix64; that was unsound for this threat model: call_ids
/// are sent to callees by design, and SplitMix64's output
/// finalizer is a public bijection, so a single observed id
/// reveals the generator state and with it every FUTURE call_id
/// minted on that thread — letting one callee forge responses to
/// races on calls addressed to other peers. Raw pooled entropy
/// has no such state to recover.)
///
/// If the pool refill fails, falls back to a process-global
/// monotonic counter rather than returning `0`: two concurrent
/// callers that both minted `0` would `register(0, …)` over each
/// other, so the first caller's oneshot closes with
/// `RecvError::Closed` (a spurious `Transport` error, not the clean
/// timeout the all-distinct path yields). The counter keeps ids
/// distinct (predictable on entropy failure, but the S-4
/// `from_node` gate still blocks cross-peer forgery, and such calls
/// time out anyway). `getrandom::fill` failure is a fatal-
/// environment signal (no `/dev/urandom`, broken syscall) and the
/// broader stack won't be functional anyway; the pool cursor is
/// left exhausted so the next mint retries the refill. `0` is
/// reserved as a sentinel and never returned.
fn mint_random_call_id() -> u64 {
    thread_local! {
        // (pool, cursor). Cursor starts exhausted so the first
        // mint on each thread performs the initial refill.
        static CALL_ID_ENTROPY_POOL: std::cell::RefCell<([u8; CALL_ID_ENTROPY_POOL_BYTES], usize)> = const {
            std::cell::RefCell::new(([0u8; CALL_ID_ENTROPY_POOL_BYTES], CALL_ID_ENTROPY_POOL_BYTES))
        };
    }
    CALL_ID_ENTROPY_POOL.with(|cell| {
        let mut pool = cell.borrow_mut();
        let (buf, cursor) = &mut *pool;
        if *cursor >= CALL_ID_ENTROPY_POOL_BYTES {
            if getrandom::fill(buf).is_err() {
                // Entropy unavailable. Do NOT return 0 — concurrent
                // callers would all mint 0 and clobber each other's
                // pending entries. A process-global counter keeps ids
                // distinct (starts at 1, so it is non-zero until it
                // wraps the full u64 range, at which point the 0 is
                // mapped to 1 below).
                static CALL_ID_FALLBACK: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(1);
                let id = CALL_ID_FALLBACK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return if id == 0 { 1 } else { id };
            }
            *cursor = 0;
        }
        let mut id = [0u8; 8];
        id.copy_from_slice(&buf[*cursor..*cursor + 8]);
        *cursor += 8;
        // Reserve 0 as the "no correlation" sentinel: on the ~1-in-2^64
        // chance the pool yields all-zero bytes, remap to a fixed non-zero.
        match u64::from_le_bytes(id) {
            0 => 1,
            id => id,
        }
    })
}

/// Per-thread OS-entropy pool size for [`mint_random_call_id`].
/// 64 ids (512 bytes) per `getrandom` syscall — the syscall cost
/// is dominated by the fixed kernel round-trip, so batching 64
/// mints recovers ~98% of the per-call overhead while keeping the
/// amount of buffered future-id entropy per thread small.
const CALL_ID_ENTROPY_POOL_BYTES: usize = 64 * 8;

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

    /// Caller-side helper that pairs `rpc_route_for_service` with
    /// the `RpcError::NoRoute { target, reason }` mapping every
    /// `Mesh::call*` entry point needs. Returning `Arc<RpcRoute>`
    /// keeps the hot-path allocation profile of the cache intact
    /// (one refcount bump per caller).
    fn rpc_route_or_no_route(
        &self,
        target_node_id: u64,
        service: &str,
    ) -> Result<Arc<super::mesh::RpcRoute>, RpcError> {
        self.rpc_route_for_service(service)
            .map_err(|reason| RpcError::NoRoute {
                target: target_node_id,
                reason,
            })
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
    /// The descriptor announces `pricing_terms`, but this serve path
    /// has no payment-admission gate — an announced price this path
    /// cannot enforce must never reach discovery (callers would see a
    /// priced tool that serves free). Serve paid tools via the SDK's
    /// `Mesh::serve_tool_paid` (native gate), or publish through the
    /// MCP adapter's `ServerPublisher::publish_tools` with a
    /// `payment_admission` gate.
    #[error(
        "tool `{0}` announces pricing_terms but this serve path cannot enforce payment — \
         serve paid tools via Mesh::serve_tool_paid, or publish via \
         ServerPublisher::publish_tools with payment_admission"
    )]
    UnenforceablePricing(String),
    /// The gated serve path (`Mesh::serve_tool_paid`) got a descriptor
    /// with **no** `pricing_terms`: a payment gate on an unannounced
    /// price means every caller is refused with no way to know why.
    /// Announce the price (the gate enforces it), or serve the tool
    /// free via `Mesh::serve_tool`.
    #[error(
        "tool `{0}` is served through the payment gate but announces no pricing_terms — \
         attach terms to the descriptor, or serve it free via Mesh::serve_tool"
    )]
    MissingPricingTerms(String),
}

// ============================================================================
// Typed-call helper.
// ============================================================================

/// Wire-shape failures from [`typed_call`]. Distinct variants
/// for transport (no route, timeout, etc.) vs codec (serde /
/// postcard) so service-specific client error enums can wrap
/// each independently. Server-level (application) errors are
/// decoded into `Resp` itself — the client matches on the
/// resulting `Resp::Error(...)` variant.
#[derive(Debug, thiserror::Error)]
pub enum TypedCallError {
    /// Transport-level failure surfaced by [`MeshNode::call`].
    #[error("transport: {0}")]
    Transport(#[from] RpcError),
    /// Request serialization or response deserialization failed.
    #[error("codec: {0}")]
    Codec(String),
}

impl From<postcard::Error> for TypedCallError {
    fn from(e: postcard::Error) -> Self {
        Self::Codec(e.to_string())
    }
}

/// Send a postcard-encoded request to a remote RPC service and
/// decode the postcard-encoded reply. The shared shape every
/// substrate-internal RPC client wants:
///
/// 1. `postcard::to_allocvec(request)` → wire body.
/// 2. `MeshNode::call(target, service, body, opts{deadline})`.
/// 3. `postcard::from_bytes::<Resp>(reply.body)`.
///
/// Caller wraps the returned `Resp` in its own typed-error
/// surface (typically a `Server` variant that holds the
/// service-specific error enum decoded from `Resp`). Returning
/// `TypedCallError` here keeps the wrapper code to a one-line
/// `From<TypedCallError>` impl per client.
pub async fn typed_call<Req, Resp>(
    mesh: &std::sync::Arc<crate::adapter::net::MeshNode>,
    target_node_id: u64,
    service: &str,
    request: &Req,
    deadline: std::time::Duration,
) -> Result<Resp, TypedCallError>
where
    Req: serde::Serialize,
    Resp: serde::de::DeserializeOwned,
{
    let body = postcard::to_allocvec(request)?;
    let opts = CallOptions {
        deadline: Some(std::time::Instant::now() + deadline),
        ..Default::default()
    };
    let reply = mesh
        .call(target_node_id, service, Bytes::from(body), opts)
        .await?;
    Ok(postcard::from_bytes(&reply.body)?)
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

#[cfg(test)]
mod origin_cache_tests {
    use super::*;

    /// KC7 (Kyra E1 audit) — the response-route cache is populated
    /// ONLY for a trustworthy destination. Because the cache is keyed
    /// on the wire-claimed `origin_hash`, a frame whose claimed origin
    /// does not match the AEAD-authenticated `from_node` peer's OWN
    /// origin (a malicious node stamping a victim's origin), a control
    /// frame, an unpinned peer, or the loopback sentinel must NEVER
    /// establish a destination — so a denied/forged frame can't
    /// redirect a legitimate call's response.
    #[test]
    fn response_route_trust_requires_authenticated_direct_origin() {
        let victim_origin = 0x1111_2222_3333_4444u64;
        let peer_node = 0xABCDu64;

        // Honest: REQUEST, authenticated peer's own origin → trusted.
        assert!(response_route_is_trustworthy(
            peer_node,
            Some(DISPATCH_RPC_REQUEST),
            victim_origin,
            Some(victim_origin),
        ));

        // Forged: the claim (victim_origin) ≠ the authenticated peer's
        // real origin → REFUSED (the poison the fix prevents).
        assert!(!response_route_is_trustworthy(
            peer_node,
            Some(DISPATCH_RPC_REQUEST),
            victim_origin,
            Some(0x9999_9999_9999_9999),
        ));

        // Control frames never establish routing, even authenticated.
        for dispatch in [
            DISPATCH_RPC_CANCEL,
            DISPATCH_RPC_REQUEST_CHUNK,
            DISPATCH_RPC_REQUEST_GRANT,
        ] {
            assert!(!response_route_is_trustworthy(
                peer_node,
                Some(dispatch),
                victim_origin,
                Some(victim_origin),
            ));
        }

        // Unpinned peer (no authenticated entity) → refused.
        assert!(!response_route_is_trustworthy(
            peer_node,
            Some(DISPATCH_RPC_REQUEST),
            victim_origin,
            None,
        ));

        // Loopback/test sentinel (from_node == 0) → refused.
        assert!(!response_route_is_trustworthy(
            0,
            Some(DISPATCH_RPC_REQUEST),
            victim_origin,
            Some(victim_origin),
        ));

        // Non-decodable dispatch → refused.
        assert!(!response_route_is_trustworthy(
            peer_node,
            None,
            victim_origin,
            Some(victim_origin),
        ));
    }

    /// The crafted-origin memory-amplification guard (cubic P2): the reply-
    /// channel / origin-node caches are keyed by the *wire-claimed*
    /// `caller_origin`, which a single authed peer can vary freely. Spraying
    /// far more distinct origins than the capacity must NOT grow the cache —
    /// it stays pinned at `RPC_CALLER_CACHE_CAP`, evicting the coldest.
    #[test]
    fn origin_keyed_lru_bounds_under_crafted_origin_flood() {
        let cache: OriginKeyedLru<u64> = OriginKeyedLru::new();
        let flood = (RPC_CALLER_CACHE_CAP as u64) * 4;
        for origin in 0..flood {
            cache.insert(origin, origin);
        }
        assert_eq!(
            cache.0.lock().len(),
            RPC_CALLER_CACHE_CAP,
            "cache must stay at its capacity bound under a crafted-origin flood"
        );
        // The most-recently-seen window survives; the cold prefix is evicted.
        assert_eq!(cache.get(flood - 1), Some(flood - 1));
        assert_eq!(cache.get(0), None);
    }

    /// PERF_AUDIT §3.8 — `mint_random_call_id` mints thousands of
    /// values from the pooled-entropy path, all of which must be
    /// distinct in practice (a duplicate would let two in-flight
    /// calls collide on the per-Mesh pending map). 100k samples is
    /// far below the 2^32 birthday-paradox boundary, so a
    /// regression that recycled pool bytes (cursor mis-advance,
    /// missed refill) would fail loudly. The loop also crosses the
    /// pool-refill boundary thousands of times (pool holds 64 ids),
    /// pinning the refill/cursor arithmetic.
    #[test]
    fn mint_random_call_id_produces_distinct_values_across_thousands_of_calls() {
        let mut seen = std::collections::HashSet::with_capacity(100_000);
        for _ in 0..100_000 {
            let id = super::mint_random_call_id();
            // 0 is the fallback sentinel — should not appear under
            // a working `getrandom` refill.
            assert_ne!(id, 0, "fallback-zero path triggered unexpectedly");
            assert!(seen.insert(id), "duplicate call_id minted: {:#x}", id);
        }
    }

    /// PERF_AUDIT §3.8 — minted ids are raw OS entropy: count
    /// set-bits across 10k mints and assert the fraction is near
    /// 0.5. A pool-management bug that handed out the zeroed
    /// initial buffer (or re-served a stale window) would skew
    /// this hard; properly random 64-bit ids have expected ~0.5
    /// set bits per sample with O(1/sqrt(N)) tolerance.
    #[test]
    fn mint_random_call_id_set_bit_density_is_balanced() {
        let n = 10_000u64;
        let mut total_set: u64 = 0;
        for _ in 0..n {
            total_set += super::mint_random_call_id().count_ones() as u64;
        }
        let bits_total = n * 64;
        let fraction = total_set as f64 / bits_total as f64;
        // Expected 0.5; tolerance generous (~3σ) to keep the test
        // reliable while still catching collapsed-pool regressions.
        assert!(
            (fraction - 0.5).abs() < 0.02,
            "set-bit density {} is too far from 0.5 — pool may be mismanaged",
            fraction
        );
    }

    /// PERF_AUDIT §3.5 — the reply-subscription registry's hot
    /// path must be a `(target, xxh3(service))` lookup, not a
    /// `Mutex<Vec<(u64, String)>>` linear scan. Pin the contract
    /// via `reply_subscription_covers` (the exact hot-path check):
    /// 1. distinct (target, service) pairs are distinct keys —
    ///    same service against two targets, and two services
    ///    against one target, never alias;
    /// 2. repeat insert of the same pair is idempotent and the
    ///    fast path keeps answering `true`;
    /// 3. an xxh3 COLLISION (same hash, different service name)
    ///    must NOT count as covered — a false positive here would
    ///    skip a needed subscribe and silently drop the colliding
    ///    service's replies. The stored-name verification turns it
    ///    into a re-subscribe instead;
    /// 4. the cap is enforced via `len()`, not a separate counter
    ///    that could drift.
    #[test]
    fn reply_subscriptions_keyed_by_target_and_service_hash() {
        use dashmap::DashMap;
        let registry: DashMap<(u64, u64), Arc<str>> = DashMap::new();
        let h_a = xxhash_rust::xxh3::xxh3_64(b"svc-a");
        let h_b = xxhash_rust::xxh3::xxh3_64(b"svc-b");
        // Same target, different services → distinct entries.
        registry.insert((0xAA, h_a), Arc::from("svc-a"));
        registry.insert((0xAA, h_b), Arc::from("svc-b"));
        assert!(super::reply_subscription_covers(
            &registry, 0xAA, h_a, "svc-a"
        ));
        assert!(super::reply_subscription_covers(
            &registry, 0xAA, h_b, "svc-b"
        ));
        // Same service, different targets → distinct entries.
        assert!(!super::reply_subscription_covers(
            &registry, 0xBB, h_a, "svc-a"
        ));
        registry.insert((0xBB, h_a), Arc::from("svc-a"));
        assert!(super::reply_subscription_covers(
            &registry, 0xBB, h_a, "svc-a"
        ));
        // Idempotent — repeat insert overwrites with the identical
        // value; the fast path keeps answering true.
        registry.insert((0xAA, h_a), Arc::from("svc-a"));
        assert!(super::reply_subscription_covers(
            &registry, 0xAA, h_a, "svc-a"
        ));
        assert_eq!(registry.len(), 3);
        // xxh3 collision: "svc-evil" hashing to h_a (forced here —
        // xxh3_64 collisions are computable offline since the hash
        // isn't cryptographic) must NOT cover "svc-a"'s slot, and
        // vice versa. The hash-only DashSet shape this replaced
        // answered `true` and silently skipped the subscribe.
        assert!(
            !super::reply_subscription_covers(&registry, 0xAA, h_a, "svc-evil"),
            "hash collision must not satisfy the membership check for a \
             different service name"
        );
        // After the colliding service legitimately subscribes (slot
        // overwritten), the original service degrades to
        // re-subscribe — covered must flip to false for it, never
        // silently true for both.
        registry.insert((0xAA, h_a), Arc::from("svc-evil"));
        assert!(super::reply_subscription_covers(
            &registry, 0xAA, h_a, "svc-evil"
        ));
        assert!(!super::reply_subscription_covers(
            &registry, 0xAA, h_a, "svc-a"
        ));
    }

    /// PERF_AUDIT §3.3 — grant-stall backstop check for the
    /// window/2 auto-grant coalescing. Simulates the full
    /// credit loop for every window 1..=64: the server starts
    /// with `window` credits and consumes one per chunk; the
    /// client accumulates via `accumulate_auto_grant` and only
    /// flushes at the threshold. Asserts:
    /// 1. liveness — an actively-polling consumer never observes
    ///    the server starved (credits exhausted with nothing left
    ///    to poll), i.e. withholding sub-threshold credits cannot
    ///    deadlock the stream and no timer/drop backstop is needed;
    /// 2. coalescing — grant-packet count stays at
    ///    ~chunks / (window/2), and is strictly fewer than one
    ///    grant per chunk once window ≥ 4 (the integration suite
    ///    only exercises window=2, whose threshold degenerates
    ///    to per-chunk).
    #[test]
    fn auto_grant_coalescing_never_starves_the_server_pump() {
        for window in 1u32..=64 {
            let chunks = 1_000u32;
            let mut server_credits = window as u64;
            let mut pending = 0u32;
            let mut sent = 0u32;
            let mut delivered = 0u32;
            let mut grants = 0u32;
            while delivered < chunks {
                // Server pump: send while credits remain.
                while server_credits > 0 && sent < chunks {
                    server_credits -= 1;
                    sent += 1;
                }
                assert!(
                    sent > delivered,
                    "window {window}: server starved while the consumer is actively \
                     polling (credits {server_credits}, pending {pending}, \
                     sent {sent}, delivered {delivered})"
                );
                // Consumer polls exactly one chunk.
                delivered += 1;
                if let Some(amount) = super::accumulate_auto_grant(&mut pending, window) {
                    grants += 1;
                    server_credits += amount as u64;
                }
            }
            let threshold = (window / 2).max(1);
            assert!(
                grants <= chunks / threshold + 1,
                "window {window}: {grants} grant packets exceeds the \
                 coalesced cadence bound of {}",
                chunks / threshold + 1
            );
            if window >= 4 {
                assert!(
                    grants < chunks,
                    "window {window}: coalescing must emit fewer grants than chunks"
                );
            }
        }
    }

    /// `get` promotes to most-recently-used, so a touched entry outlives an
    /// untouched one when the cache overflows by one — confirming the wrapper
    /// gives true LRU semantics (a hot caller isn't evicted out from under an
    /// in-flight exchange).
    #[test]
    fn origin_keyed_lru_get_promotes_to_mru() {
        let cache: OriginKeyedLru<u64> = OriginKeyedLru::new();
        for origin in 0..(RPC_CALLER_CACHE_CAP as u64) {
            cache.insert(origin, origin);
        }
        // Touch origin 0 (otherwise the LRU), then overflow by one entry.
        assert_eq!(cache.get(0), Some(0));
        cache.insert(u64::MAX, 1);
        assert_eq!(cache.get(0), Some(0), "touched entry must survive eviction");
        assert_eq!(cache.get(1), None, "the now-LRU entry (1) must be evicted");
    }
}
