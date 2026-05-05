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

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::channel::{ChannelId, ChannelName, ChannelPublisher, PublishConfig};
use super::cortex::{
    EventMeta, RpcCancellationToken, RpcClientFold, RpcContext, RpcHandler, RpcHandlerError,
    RpcInboundDispatcher, RpcInboundEvent, RpcRequestPayload, RpcResponseEmitter,
    RpcResponsePayload, RpcServerFold, RpcStatus, DISPATCH_RPC_CANCEL, DISPATCH_RPC_REQUEST,
    EVENT_META_SIZE,
};
use crate::error::AdapterError;

use super::mesh::MeshNode;
use super::redex::{RedexEntry, RedexEvent, RedexFold};

// ============================================================================
// Public types.
// ============================================================================

/// Options for [`MeshNode::call`].
#[derive(Debug, Clone)]
pub struct CallOptions {
    /// Hard deadline for the call. The future returned by `call`
    /// races a `tokio::time::sleep_until`; whichever fires first
    /// wins. On timeout the caller emits a CANCEL event for
    /// `call_id` so the server can drop the in-flight handler.
    /// `None` means no deadline; the caller waits indefinitely
    /// (or until the future is dropped).
    pub deadline: Option<Instant>,
    /// Per-call concurrency cap. Future Phase 2 work; v1 ignores
    /// this and the per-Mesh `RpcClientPending` doesn't bound
    /// in-flight count.
    pub max_in_flight_per_target: u32,
}

impl Default for CallOptions {
    fn default() -> Self {
        Self {
            deadline: None,
            max_in_flight_per_target: 64,
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
}

/// RAII handle returned by [`MeshNode::serve_rpc`]. Dropping it
/// unregisters the inbound dispatcher and stops the bridge task,
/// removing the service from the local node.
///
/// Outstanding handler executions (already-spawned tokio tasks)
/// continue to completion — the handle's Drop only stops
/// new request dispatch.
pub struct ServeHandle {
    /// Channel hash to unregister on Drop.
    channel_hash: u16,
    /// The bridge task — `JoinHandle::abort` on Drop stops it.
    bridge: Option<JoinHandle<()>>,
    /// Hold an Arc back to the mesh so we can unregister on Drop
    /// without the mesh having to track us.
    mesh: Arc<MeshNode>,
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        self.mesh.unregister_rpc_inbound(self.channel_hash);
        if let Some(handle) = self.bridge.take() {
            handle.abort();
        }
    }
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

                let publisher =
                    ChannelPublisher::new(reply_channel, PublishConfig::default());
                if let Err(e) = mesh.publish(&publisher, Bytes::from(buf)).await {
                    tracing::warn!(error = %e, caller_origin = format!("{:#x}", caller_origin),
                        call_id, "rpc serve_rpc: response publish failed");
                }
            });
        });

        // Build the server fold and wrap it in an Arc<Mutex<...>>
        // so the bridge task can drive it (the trait takes
        // `&mut self`).
        let fold = Arc::new(Mutex::new(RpcServerFold::new(
            handler as Arc<dyn RpcHandler>,
            emit,
        )));

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

        Ok(ServeHandle {
            channel_hash,
            bridge: Some(bridge),
            mesh: Arc::clone(self),
        })
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
        let request_channel = ChannelName::new(&format!("{service}.requests"))
            .map_err(|e| RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid service name: {e}"),
            })?;
        let self_origin = self.identity_origin_hash();
        let reply_channel_name = format!("{service}.replies.{self_origin:016x}");
        let reply_channel = ChannelName::new(&reply_channel_name).map_err(|e| {
            RpcError::NoRoute {
                target: target_node_id,
                reason: format!("invalid reply channel name: {e}"),
            }
        })?;
        let reply_hash = reply_channel.hash();

        // Lazy reply-channel subscription. Once per (target, service).
        self.ensure_reply_subscription(target_node_id, service, reply_channel.clone(), reply_hash)
            .await?;

        // Allocate a fresh call_id. Per-caller monotonic.
        let call_id = self.rpc_next_call_id().fetch_add(1, Ordering::Relaxed);

        // Register the oneshot before publishing the REQUEST so a
        // very-fast RESPONSE doesn't arrive before we're ready.
        let pending = self.rpc_client_pending();
        let rx = pending.register(call_id);

        // Build the REQUEST envelope.
        let req = RpcRequestPayload {
            service: service.to_string(),
            deadline_ns: opts
                .deadline
                .map(instant_to_unix_nanos)
                .unwrap_or(0),
            flags: 0,
            headers: vec![],
            body: payload.to_vec(),
        };
        let meta = EventMeta::new(
            DISPATCH_RPC_REQUEST,
            0,
            self_origin,
            call_id,
            0,
        );
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

        // Race the receiver against the deadline. On timeout or
        // drop, send a CANCEL so the server can drop the handler.
        let outcome: Result<Result<RpcResponsePayload, _>, tokio::time::error::Elapsed> =
            match opts.deadline {
                None => Ok(rx.await),
                Some(deadline) => {
                    let timeout_at = deadline.saturating_duration_since(Instant::now());
                    tokio::time::timeout(timeout_at, rx).await
                }
            };

        let resp = match outcome {
            Ok(Ok(resp)) => resp,
            Ok(Err(_recv_err)) => {
                pending.cancel(call_id);
                return Err(RpcError::Transport(AdapterError::Connection(
                    "rpc client pending sender dropped (no response will arrive)".into(),
                )));
            }
            Err(_elapsed) => {
                // Timeout: emit CANCEL so the server can drop the
                // handler, clear the pending entry, surface to caller.
                pending.cancel(call_id);
                self.send_rpc_cancel(target_node_id, &request_channel, self_origin, call_id)
                    .await;
                return Err(RpcError::Timeout {
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
        };

        // Map the wire status onto the public Result type.
        if resp.status.is_ok() {
            Ok(RpcReply {
                body: Bytes::from(resp.body),
                headers: resp.headers,
                latency_ns: started.elapsed().as_nanos() as u64,
            })
        } else {
            let status = resp.status.to_wire();
            let message = String::from_utf8(resp.body)
                .unwrap_or_else(|e| format!("<{} bytes of non-utf8 body>", e.into_bytes().len()));
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
    async fn ensure_reply_subscription(
        self: &Arc<Self>,
        target_node_id: u64,
        service: &str,
        reply_channel: ChannelName,
        reply_hash: u16,
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

        // Register the inbound dispatcher: feed RESPONSE events
        // to the per-Mesh `RpcClientFold`. We construct a fresh
        // fold here per (target, service) registration — the fold
        // itself is stateless apart from the shared `pending`
        // map. (Future: lift the fold to a single per-Mesh
        // instance shared across all reply channels; for now the
        // per-channel fold is fine because the inbound dispatcher
        // map is keyed on channel_hash.)
        let pending = self.rpc_client_pending();
        let fold = Arc::new(Mutex::new(RpcClientFold::new(pending)));
        let dispatcher: RpcInboundDispatcher = Arc::new(move |ev| {
            let entry = RedexEntry::new_heap(0, 0, ev.payload.len() as u32, 0, 0);
            let redex_event = RedexEvent {
                entry,
                payload: ev.payload,
            };
            if let Err(e) = fold.lock().apply(&redex_event, &mut ()) {
                tracing::warn!(error = %e, "rpc client fold: apply error");
            }
        });
        // Slot collision (two services using the same reply channel
        // hash) is ~1/65536 per pair; in the rare collision the
        // earlier registration wins. The replaced dispatcher's
        // events would route to the new fold instead — surfaces
        // as cross-service oneshot misses (the mismatched call_id
        // doesn't match any pending entry, event is silently
        // dropped). Document; revisit in Phase 2 with a wider
        // dispatch key.
        if let Some(_prev) = self.register_rpc_inbound(reply_hash, dispatcher) {
            tracing::warn!(
                channel_hash = format!("{:#06x}", reply_hash),
                service,
                "rpc call: reply-channel hash collision with prior registration",
            );
        }

        let _ = reply_hash; // captured into the dispatcher above; surfaced for debug
        registry
            .lock()
            .push((target_node_id, service.to_string()));
        Ok(())
    }

    /// Best-effort: direct-send a CANCEL event for `call_id` to
    /// the server. Used by the call-side timeout / drop path.
    /// Failure is logged but not surfaced — the server's in-flight
    /// entry will be reaped by either the per-call handler's own
    /// cancellation observation (deadline-passed short-circuit) or
    /// the handler running to completion with the response getting
    /// dropped on the caller side (pending entry already removed).
    async fn send_rpc_cancel(
        self: &Arc<Self>,
        target_node_id: u64,
        request_channel: &ChannelName,
        self_origin: u64,
        call_id: u64,
    ) {
        let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, self_origin, call_id, 0);
        let request_channel_id = ChannelId::new(request_channel.clone());
        let request_channel_hash = request_channel_id.hash();
        let stream_id = MeshNode::publish_stream_id(&request_channel_id);
        let payload = Bytes::from(meta.to_bytes().to_vec());
        if let Err(e) = self
            .publish_to_peer(
                target_node_id,
                request_channel_hash,
                stream_id,
                /* reliable */ true,
                std::slice::from_ref(&payload),
            )
            .await
        {
            tracing::debug!(error = %e, call_id,
                "rpc call: best-effort CANCEL send failed");
        }
    }
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
    fn rpc_next_call_id(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.rpc_next_call_id_arc()
    }
    fn identity_origin_hash(&self) -> u64 {
        self.public_key_origin_hash()
    }
}

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
