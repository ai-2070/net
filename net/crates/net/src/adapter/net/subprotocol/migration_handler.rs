//! Migration subprotocol message handler.
//!
//! Dispatches inbound migration messages (subprotocol 0x0500) to the
//! appropriate handler: orchestrator, source, or target.

use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::adapter::net::compute::migration_target::RestoreContext;
use crate::adapter::net::compute::orchestrator::wire;
use crate::adapter::net::compute::{
    MigrationError, MigrationMessage, MigrationOrchestrator, MigrationSourceHandler,
    MigrationTargetHandler, SnapshotReassembler,
};
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::state::snapshot::StateSnapshot;

/// Identity-transport context for automatic envelope seal/open in
/// the migration dispatcher.
///
/// When populated, the handler:
/// - **Source path**: after taking a snapshot, if the target's
///   X25519 static pub is known (via `peer_static_lookup`) AND the
///   local daemon's keypair is available, seal the envelope into
///   the snapshot before chunking.
/// - **Target path**: on `SnapshotReady` with an attached envelope,
///   call `unseal_snapshot` to recover the daemon's keypair, and
///   pass that into `restore_snapshot` instead of whatever the
///   factory registry has pre-registered.
///
/// A `None` context means the dispatcher ignores envelopes — the
/// pre-identity-envelope fallback path where both nodes pre-register
/// matching keypairs.
///
/// # Key hygiene
///
/// This struct used to carry the local Noise static private key as a
/// `pub [u8; 32]` field. Any SDK caller with access to a
/// `MigrationIdentityContext` could copy the node's long-term secret
/// out, which is unacceptable — the Noise static is what backs the
/// node's identity in the mesh, not just the envelope-open path.
///
/// The private key is now captured inside the `unseal_snapshot`
/// closure (built by [`MeshNode::migration_identity_context`](crate::adapter::net::MeshNode::migration_identity_context)) and
/// never surfaced as a struct field. Callers can still hand the
/// context to the dispatcher, but they cannot extract the key from
/// it. This matches how `peer_static_lookup` already worked.
#[derive(Clone)]
pub struct MigrationIdentityContext {
    /// Open an identity envelope attached to `snapshot`, if present,
    /// using the local static X25519 private key captured at
    /// construction time. Returns `Ok(None)` when the snapshot has
    /// no envelope, `Ok(Some(kp))` on a successful open, and an
    /// error string on seal-open / attestation failure.
    ///
    /// Built by [`MeshNode::migration_identity_context`](crate::adapter::net::MeshNode::migration_identity_context); the
    /// closure owns a `zeroize`-on-drop `StaticSecret` so the key
    /// material is wiped when the context is dropped.
    pub unseal_snapshot: EnvelopeUnsealFn,
    /// Callback: given a peer node_id, return its X25519 static
    /// public key if we have an active session with it. Used by the
    /// source path to pick the seal recipient.
    pub peer_static_lookup: Arc<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync>,
}

impl std::fmt::Debug for MigrationIdentityContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationIdentityContext")
            .field("unseal_snapshot", &"<fn>")
            .field("peer_static_lookup", &"<fn>")
            .finish()
    }
}

/// Outbound message with destination node.
#[derive(Debug)]
pub struct OutboundMigrationMessage {
    /// Destination node ID.
    pub dest_node: u64,
    /// Encoded wire message.
    pub payload: Vec<u8>,
}

/// Handles inbound migration subprotocol messages.
///
/// Routes each message type to the orchestrator, source handler, or target
/// handler as appropriate, and produces outbound response messages.
/// Callback fired after the target-side dispatcher successfully
/// restores a daemon from a migration snapshot. Invoked with the
/// daemon's `origin_hash`. Used by the SDK to drive channel
/// re-bind replay (`DAEMON_CHANNEL_REBIND_PLAN.md` Stage 3): the
/// callback walks the restored daemon's subscription ledger and
/// spawns asynchronous `subscribe_channel` calls so publishers
/// start fanning out to the target before the source tears down.
///
/// The callback runs synchronously on the dispatcher thread; it
/// should kick off any async work via `tokio::spawn` rather than
/// blocking the dispatch loop.
pub type PostRestoreCallback = Arc<dyn Fn(u64) + Send + Sync>;

/// Callback fired on the source side at `CutoverNotify` handling,
/// immediately before `source_handler.cleanup` unregisters the
/// daemon. Stage 4 of `DAEMON_CHANNEL_REBIND_PLAN.md`: the SDK's
/// hook snapshots the daemon's subscription ledger here and spawns
/// async `unsubscribe_channel` calls to each publisher so rosters
/// drop the source immediately rather than aging out over the
/// session-timeout window.
///
/// Sync on the dispatcher thread; async work must `tokio::spawn`.
pub type PreCleanupCallback = Arc<dyn Fn(u64) + Send + Sync>;

/// Readiness predicate — returns `true` when the local runtime is
/// prepared to accept inbound migrations (target path). When
/// populated and the predicate returns `false`, the dispatcher
/// responds to `SnapshotReady` with
/// [`MigrationFailureReason::NotReady`](crate::adapter::net::compute::MigrationFailureReason::NotReady)
/// so the source can retry with backoff rather than surfacing a
/// terminal error while the target is still booting.
///
/// The callback is consulted synchronously on the dispatcher
/// thread — it must return quickly.
pub type ReadinessCallback = Arc<dyn Fn() -> bool + Send + Sync>;

/// Callback fired on the source side whenever the dispatcher
/// observes an inbound `MigrationFailed` message. The SDK uses this
/// to surface the structured reason code to the
/// [`MigrationHandle::wait`](crate) caller so they can distinguish
/// retriable from terminal failures.
///
/// Sync on the dispatcher thread.
pub type FailureCallback =
    Arc<dyn Fn(u64, crate::adapter::net::compute::MigrationFailureReason) + Send + Sync>;

/// Callback that opens an identity envelope carried on a
/// [`crate::adapter::net::state::snapshot::StateSnapshot`], using a
/// local private key captured inside the closure. See
/// [`MigrationIdentityContext`] for key-hygiene rationale.
pub type EnvelopeUnsealFn = Arc<
    dyn Fn(
            &crate::adapter::net::state::snapshot::StateSnapshot,
        ) -> Result<Option<EntityKeypair>, crate::adapter::net::identity::EnvelopeError>
        + Send
        + Sync,
>;

/// Optional cross-cutting hooks consumed by
/// [`MigrationSubprotocolHandler::with_hooks`]. Each field is
/// independently opt-in so a test that cares about one hook doesn't
/// have to fabricate the others; `Default` = nothing wired.
///
/// | Field             | Side     | Purpose                                    |
/// |-------------------|----------|--------------------------------------------|
/// | `identity`        | both     | auto-seal / auto-open identity envelopes   |
/// | `post_restore`    | target   | kick off channel re-bind replay            |
/// | `pre_cleanup`     | source   | source-side Unsubscribe teardown           |
/// | `readiness`       | target   | gate inbound migrations on runtime state   |
/// | `failure`         | source   | surface structured reason to SDK caller    |
#[derive(Default, Clone)]
pub struct MigrationHandlerHooks {
    /// Identity-transport context. `None` = envelopes ignored
    /// (pre-identity-envelope fallback).
    pub identity: Option<MigrationIdentityContext>,
    /// Target-side post-restore callback — drives subscription
    /// replay.
    pub post_restore: Option<PostRestoreCallback>,
    /// Source-side pre-cleanup callback — drives Unsubscribe
    /// teardown.
    pub pre_cleanup: Option<PreCleanupCallback>,
    /// Target-side readiness predicate — returns `false` to reply
    /// `NotReady` instead of attempting restore.
    pub readiness: Option<ReadinessCallback>,
    /// Source-side failure observer — surfaces structured reason
    /// codes to the SDK.
    pub failure: Option<FailureCallback>,
}

/// Dispatcher for migration subprotocol (`0x0500`) messages.
///
/// Wraps the three handler halves — orchestrator, source, target —
/// plus the optional cross-cutting hooks that let the SDK drive
/// identity-envelope seal/open, channel-re-bind replay,
/// source-side Unsubscribe teardown, runtime-readiness gating, and
/// source-side failure observation. Constructed by the SDK's
/// `DaemonRuntime::start` via [`Self::with_hooks`]; tests use
/// [`Self::new`] when they don't need any hooks.
///
/// Install onto a `MeshNode` via `MeshNode::set_migration_handler`.
pub struct MigrationSubprotocolHandler {
    orchestrator: Arc<MigrationOrchestrator>,
    source_handler: Arc<MigrationSourceHandler>,
    target_handler: Arc<MigrationTargetHandler>,
    local_node_id: u64,
    /// Per-target reassemblers for incoming snapshot chunks. One entry
    /// per in-flight inbound migration. Lazily created on first chunk,
    /// torn down after successful restore or failure.
    ///
    /// Wrapped in `Mutex` because `SnapshotReassembler::feed` requires
    /// `&mut self`.
    reassemblers: DashMap<u64, Mutex<SnapshotReassembler>>,
    /// Identity-transport context. `None` = envelopes ignored
    /// (pre-identity-envelope fallback).
    identity_context: Option<MigrationIdentityContext>,
    /// Post-restore callback, fired on the target side after
    /// `restore_snapshot` succeeds. Used by the SDK to drive
    /// subscription replay. `None` = no hook (used by tests and
    /// pre-Stage-3 callers).
    post_restore_callback: Option<PostRestoreCallback>,
    /// Pre-cleanup callback, fired on the source side at
    /// `CutoverNotify` handling just before the daemon is
    /// unregistered. Drives source-side Unsubscribe teardown
    /// (Stage 4 of the channel re-bind plan). `None` = no hook.
    pre_cleanup_callback: Option<PreCleanupCallback>,
    /// Target-side readiness predicate. When `Some` and returns
    /// `false`, the dispatcher responds to inbound migration
    /// attempts with `NotReady` instead of attempting restore.
    /// Drives the runtime-readiness retry path
    /// (`DAEMON_RUNTIME_READINESS_PLAN.md` Stage 3). `None` = always
    /// treat target as ready (pre-Stage-3 behaviour).
    readiness_callback: Option<ReadinessCallback>,
    /// Source-side failure observer — fired when the dispatcher
    /// receives an inbound `MigrationFailed` message. Lets the SDK
    /// hand the structured reason to the caller via
    /// [`MigrationHandle::wait`] rather than swallowing it at the
    /// dispatcher layer. `None` = no hook; failures are still
    /// processed (orchestrator aborted) but the SDK can't
    /// distinguish retriable (NotReady) from terminal.
    failure_callback: Option<FailureCallback>,
}

impl MigrationSubprotocolHandler {
    /// Create a new handler with no hooks wired. Envelopes on
    /// inbound snapshots are ignored; outbound snapshots don't
    /// carry an envelope; readiness is treated as always-ready;
    /// no failure observer; no channel-re-bind callbacks. Matches
    /// the pre-Stage-5b behaviour and is the right shape for tests
    /// that only need the bare three-handler dispatcher.
    pub fn new(
        orchestrator: Arc<MigrationOrchestrator>,
        source_handler: Arc<MigrationSourceHandler>,
        target_handler: Arc<MigrationTargetHandler>,
        local_node_id: u64,
    ) -> Self {
        Self::with_hooks(
            orchestrator,
            source_handler,
            target_handler,
            local_node_id,
            MigrationHandlerHooks::default(),
        )
    }

    /// Create a handler with an explicit [`MigrationHandlerHooks`]
    /// bundle. Each hook field is independently optional; the SDK
    /// supplies all five at once from `DaemonRuntime::start`, tests
    /// populate only the subset they need.
    pub fn with_hooks(
        orchestrator: Arc<MigrationOrchestrator>,
        source_handler: Arc<MigrationSourceHandler>,
        target_handler: Arc<MigrationTargetHandler>,
        local_node_id: u64,
        hooks: MigrationHandlerHooks,
    ) -> Self {
        Self {
            orchestrator,
            source_handler,
            target_handler,
            local_node_id,
            reassemblers: DashMap::new(),
            identity_context: hooks.identity,
            post_restore_callback: hooks.post_restore,
            pre_cleanup_callback: hooks.pre_cleanup,
            readiness_callback: hooks.readiness,
            failure_callback: hooks.failure,
        }
    }

    /// Handle an inbound migration message.
    ///
    /// Returns zero or more outbound messages to send to other nodes.
    pub fn handle_message(
        &self,
        data: &[u8],
        from_node: u64,
    ) -> Result<Vec<OutboundMigrationMessage>, MigrationError> {
        let msg = wire::decode(data)?;
        self.dispatch(msg, from_node)
    }

    /// Dispatch a decoded message to the appropriate handler.
    fn dispatch(
        &self,
        msg: MigrationMessage,
        from_node: u64,
    ) -> Result<Vec<OutboundMigrationMessage>, MigrationError> {
        let mut outbound = Vec::new();

        match msg {
            MigrationMessage::TakeSnapshot {
                daemon_origin,
                target_node,
            } => {
                // We are the source — take snapshot and reply.
                // Record `from_node` as the orchestrator: it's the node
                // that sent us TakeSnapshot, and replies (SnapshotReady,
                // CleanupComplete) must reach it. The source-side handler
                // stores this so subsequent replies don't drift if a
                // future forwarding layer rewrites `from_node`.
                let mut snapshot =
                    self.source_handler
                        .start_snapshot(daemon_origin, target_node, from_node)?;

                // Identity envelope: if we have a transport context
                // and can find the target's X25519 pubkey, seal the
                // daemon's keypair into the snapshot before chunking
                // so the target can reconstruct identity without an
                // out-of-band pre-registration.
                //
                // Seal failure needs a wire reply, not just `?`. The
                // orchestrator (remote) is blocked waiting for this
                // node's `SnapshotReady`; bailing with a dispatcher
                // error would leave it waiting forever and leave our
                // own `source_handler.start_snapshot` state dangling
                // for `daemon_origin`. Convert to a `MigrationFailed`
                // reply — the orchestrator consumes it, the SDK
                // surfaces the reason, retry semantics kick in if
                // applicable.
                snapshot = match self.maybe_seal_envelope(snapshot, daemon_origin, target_node) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = self.source_handler.abort(daemon_origin);
                        let reason =
                            crate::adapter::net::compute::MigrationFailureReason::StateFailed(
                                format!("identity envelope seal failed: {e}"),
                            );
                        outbound.push(OutboundMigrationMessage {
                            dest_node: from_node,
                            payload: wire::encode(&MigrationMessage::MigrationFailed {
                                daemon_origin,
                                reason,
                            })?,
                        });
                        return Ok(outbound);
                    }
                };

                // Chunk the snapshot for transport
                //
                // Oversized state/bindings surfaces as a
                // `MigrationFailed` reply (StateFailed) rather than
                // a panic in the dispatch task. The orchestrator
                // consumes the reply and retry semantics kick in,
                // the same shape as `maybe_seal_envelope` failure
                // above.
                let snapshot_bytes = match snapshot.try_to_bytes() {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = self.source_handler.abort(daemon_origin);
                        let reason =
                            crate::adapter::net::compute::MigrationFailureReason::StateFailed(
                                format!("snapshot serialization failed: {e}"),
                            );
                        outbound.push(OutboundMigrationMessage {
                            dest_node: from_node,
                            payload: wire::encode(&MigrationMessage::MigrationFailed {
                                daemon_origin,
                                reason,
                            })?,
                        });
                        return Ok(outbound);
                    }
                };
                let chunks = crate::adapter::net::compute::orchestrator::chunk_snapshot(
                    daemon_origin,
                    snapshot_bytes,
                    snapshot.through_seq,
                )?;
                let orch = self
                    .source_handler
                    .orchestrator_node(daemon_origin)
                    .unwrap_or(from_node);
                for chunk in chunks {
                    outbound.push(OutboundMigrationMessage {
                        dest_node: orch,
                        payload: wire::encode(&chunk)?,
                    });
                }
            }

            MigrationMessage::SnapshotReady {
                daemon_origin,
                snapshot_bytes,
                seq_through,
                chunk_index,
                total_chunks,
            } => {
                // Peer-auth gate. SnapshotReady is always source→
                // {orchestrator,target}. The orchestrator records
                // source_node at start_migration time; the target's
                // local record (if any) keeps the orchestrator
                // binding from its own start path. Reject if the
                // sender doesn't match the recorded principal for
                // whichever role we are in.
                //
                // Third-tier fallback closes a TOFU window: when
                // neither orchestrator-side nor target-side state
                // has a record (the orchestrator lives on a remote
                // node and we've not yet received any messages for
                // this origin), the first `SnapshotReady` would
                // otherwise bind whoever sent it as the orchestrator
                // inside `restore_on_target`. Operators who know
                // the orchestrator out-of-band can pre-bind it via
                // `DaemonFactoryRegistry::bind_expected_orchestrator`;
                // when bound, a mismatching sender is rejected
                // here, before `restore_on_target` records them.
                if let Some(expected) = self.orchestrator.source_node(daemon_origin) {
                    if expected != from_node {
                        return Err(MigrationError::WrongPeer {
                            daemon_origin,
                            from: from_node,
                            expected,
                        });
                    }
                } else if let Some(expected) = self.target_handler.orchestrator_node(daemon_origin)
                {
                    if expected != from_node {
                        return Err(MigrationError::WrongPeer {
                            daemon_origin,
                            from: from_node,
                            expected,
                        });
                    }
                } else if let Some(expected) = self
                    .target_handler
                    .factories()
                    .expected_orchestrator(daemon_origin)
                {
                    if expected != from_node {
                        return Err(MigrationError::WrongPeer {
                            daemon_origin,
                            from: from_node,
                            expected,
                        });
                    }
                }
                // If the orchestrator is local, let it record this chunk and
                // forward to target. `target_node` identifies where the
                // snapshot should be restored; if that's us, we reassemble
                // and restore instead of forwarding.
                let orch_target = self.orchestrator.target_node(daemon_origin);

                match orch_target {
                    Some(target) if target == self.local_node_id => {
                        // We are the target — advance orchestrator state
                        // (safe: on_snapshot_ready is idempotent on the
                        // target side because it just re-derives the
                        // forward message we ignore), then reassemble.
                        //
                        // Errors are informational: the restore path below
                        // is the authoritative check for whether the
                        // snapshot is usable. We log at `debug` rather
                        // than ignore so non-idempotent failures (e.g.,
                        // unexpected phase transitions on a stale record)
                        // are observable during triage.
                        if let Err(e) = self.orchestrator.on_snapshot_ready(
                            daemon_origin,
                            snapshot_bytes.clone(),
                            seq_through,
                            chunk_index,
                            total_chunks,
                        ) {
                            tracing::debug!(
                                ?e,
                                origin = format!("{:#x}", daemon_origin),
                                "on_snapshot_ready (local target): ignored"
                            );
                        }

                        if let Some(out) = self.restore_on_target(
                            daemon_origin,
                            snapshot_bytes,
                            seq_through,
                            chunk_index,
                            total_chunks,
                            from_node,
                        )? {
                            outbound.extend(out);
                        }
                    }
                    Some(target) => {
                        // Middle of the chain (or orchestrator node forwarding
                        // to a remote target). Let the orchestrator update its
                        // own phase state and emit the forward.
                        let forward = self.orchestrator.on_snapshot_ready(
                            daemon_origin,
                            snapshot_bytes,
                            seq_through,
                            chunk_index,
                            total_chunks,
                        )?;
                        if let MigrationMessage::SnapshotReady { .. } = &forward {
                            outbound.push(OutboundMigrationMessage {
                                dest_node: target,
                                payload: wire::encode(&forward)?,
                            });
                        }
                    }
                    None => {
                        // No local migration record — this node may be a
                        // target that has no orchestrator-side state (the
                        // orchestrator lives on a different node). Try to
                        // restore anyway; the factory registry is the
                        // authority on whether this node should accept.
                        if let Some(out) = self.restore_on_target(
                            daemon_origin,
                            snapshot_bytes,
                            seq_through,
                            chunk_index,
                            total_chunks,
                            from_node,
                        )? {
                            outbound.extend(out);
                        }
                    }
                }
            }

            MigrationMessage::RestoreComplete {
                daemon_origin,
                restored_seq,
            } => {
                // Target has restored — orchestrator may send buffered events.
                // If there are no buffered events, send an empty BufferedEvents
                // anyway: the target's reply (ReplayComplete) is what drives
                // the chain forward into Cutover. Dropping the message here
                // would stall any migration whose source never buffered.
                let buffered_msg = self
                    .orchestrator
                    .on_restore_complete(daemon_origin, restored_seq)?
                    .unwrap_or(MigrationMessage::BufferedEvents {
                        daemon_origin,
                        events: Vec::new(),
                    });
                outbound.push(OutboundMigrationMessage {
                    dest_node: from_node, // send back to target
                    payload: wire::encode(&buffered_msg)?,
                });
            }

            MigrationMessage::ReplayComplete {
                daemon_origin,
                replayed_seq,
            } => {
                // Target finished replay — orchestrator initiates cutover
                let cutover_msg = self
                    .orchestrator
                    .on_replay_complete(daemon_origin, replayed_seq)?;

                // Send CutoverNotify to source (from_node is the target that reported)
                if let MigrationMessage::CutoverNotify { .. } = &cutover_msg {
                    let source_node = self
                        .orchestrator
                        .source_node(daemon_origin)
                        .unwrap_or(from_node);

                    outbound.push(OutboundMigrationMessage {
                        dest_node: source_node,
                        payload: wire::encode(&cutover_msg)?,
                    });
                }
            }

            MigrationMessage::CutoverNotify {
                daemon_origin,
                target_node,
            } => {
                // We are the source — stop accepting writes.
                //
                // `on_cutover` returns `DaemonNotFound` if this node didn't
                // handle a `TakeSnapshot` (the orchestrator took the snapshot
                // locally and never involved `source_handler`). Treat that as
                // "no buffered events to drain" rather than a hard error so
                // local-source migrations can still reach cleanup.
                let final_events = match self.source_handler.on_cutover(daemon_origin) {
                    Ok(events) => events,
                    Err(MigrationError::DaemonNotFound(_)) => Vec::new(),
                    Err(e) => return Err(e),
                };

                // If there are last-moment events, send them to target
                if !final_events.is_empty() {
                    let events_msg = MigrationMessage::BufferedEvents {
                        daemon_origin,
                        events: final_events,
                    };
                    outbound.push(OutboundMigrationMessage {
                        dest_node: target_node,
                        payload: wire::encode(&events_msg)?,
                    });
                }

                // Acknowledge cutover to the local orchestrator. When the
                // orchestrator lives on a different node, this local call
                // has no record to advance; the remote orchestrator learns
                // about cutover from `CleanupComplete`, which does the same
                // phase advance there.
                match self.orchestrator.on_cutover_acknowledged(daemon_origin) {
                    Ok(()) => {}
                    Err(MigrationError::DaemonNotFound(_)) => {}
                    Err(e) => return Err(e),
                }

                // Capture the orchestrator BEFORE `cleanup()` clears the
                // source-side migration record — once it's gone,
                // `orchestrator_node()` returns None and we'd silently
                // fall back to `from_node`, defeating the whole point of
                // recording the orchestrator at `start_snapshot` time.
                let dest = self
                    .source_handler
                    .orchestrator_node(daemon_origin)
                    .unwrap_or(from_node);

                // Fire the pre-cleanup callback BEFORE unregistering
                // the daemon — the host still holds the subscription
                // ledger, which the SDK's hook snapshots here so it
                // can send `Unsubscribe` messages to every publisher
                // after cleanup. This is Stage 4 of the channel
                // re-bind plan: without it, the publishers' rosters
                // keep pointing at the source until their session
                // timeout (~30 s), causing duplicate deliveries to
                // a now-gone daemon and unnecessary bandwidth.
                if let Some(cb) = &self.pre_cleanup_callback {
                    cb(daemon_origin);
                }

                // Cleanup source. No-op if this node never authored the
                // migration (e.g. a replayed CutoverNotify after the
                // record was already cleared); only an authored
                // migration in Cutover phase actually unregisters the
                // local daemon. A forged CutoverNotify for an origin
                // we never migrated leaves the local daemon untouched.
                let _ = self.source_handler.cleanup(daemon_origin);

                let cleanup_msg = MigrationMessage::CleanupComplete { daemon_origin };
                outbound.push(OutboundMigrationMessage {
                    dest_node: dest,
                    payload: wire::encode(&cleanup_msg)?,
                });
            }

            MigrationMessage::CleanupComplete { daemon_origin } => {
                // Peer-auth gate. CleanupComplete is source→
                // orchestrator. Without binding, a forged
                // CleanupComplete from any peer makes the
                // orchestrator emit ActivateTarget to a target that
                // hasn't necessarily finished restore.
                if let Some(expected) = self.orchestrator.source_node(daemon_origin) {
                    if expected != from_node {
                        return Err(MigrationError::WrongPeer {
                            daemon_origin,
                            from: from_node,
                            expected,
                        });
                    }
                }
                // Source reports its cleanup done. The orchestrator now
                // tells the target to activate.
                let activate = self.orchestrator.on_cleanup_complete(daemon_origin)?;
                // Route the ActivateTarget to whichever node is the target.
                let target = self
                    .orchestrator
                    .target_node(daemon_origin)
                    .unwrap_or(from_node);
                outbound.push(OutboundMigrationMessage {
                    dest_node: target,
                    payload: wire::encode(&activate)?,
                });
            }

            MigrationMessage::ActivateTarget { daemon_origin } => {
                // Peer-auth gate. ActivateTarget is orchestrator→
                // target. Without binding, any peer with subprotocol
                // reach forces the target to flip live while the
                // source still believes it owns the daemon —
                // divergent chain heads. The target_handler records
                // the orchestrator at restore_snapshot time; reject
                // unless from_node matches.
                if let Some(expected) = self.target_handler.orchestrator_node(daemon_origin) {
                    if expected != from_node {
                        return Err(MigrationError::WrongPeer {
                            daemon_origin,
                            from: from_node,
                            expected,
                        });
                    }
                }
                // We are the target — drain remaining events and go live.
                // Retry-safe: `activate()` is idempotent once the migration
                // has been completed, and we route the ack to the recorded
                // orchestrator BEFORE `complete()` transitions state to the
                // idempotency index. If the ack packet is lost, a retried
                // ActivateTarget will find the completed record, return the
                // same replayed_seq, and re-send the ack. The orchestrator
                // therefore can't get wedged waiting for a completion that
                // already happened.
                let replayed_seq = self.target_handler.activate(daemon_origin)?;
                let ack_dest = self
                    .target_handler
                    .orchestrator_node(daemon_origin)
                    .unwrap_or(from_node);
                let ack = MigrationMessage::ActivateAck {
                    daemon_origin,
                    replayed_seq,
                };
                outbound.push(OutboundMigrationMessage {
                    dest_node: ack_dest,
                    payload: wire::encode(&ack)?,
                });
                // `complete()` is idempotent: a retried ActivateTarget
                // after a lost ack re-runs `activate()` (idempotent) and
                // `complete()` (no-op once Complete).
                let _ = self.target_handler.complete(daemon_origin);
            }

            MigrationMessage::ActivateAck {
                daemon_origin,
                replayed_seq,
            } => {
                // Target acknowledged — migration terminus on the
                // orchestrator.
                self.orchestrator
                    .on_activate_ack(daemon_origin, replayed_seq)?;
            }

            MigrationMessage::MigrationFailed {
                daemon_origin,
                reason,
            } => {
                // Peer-auth gate. MigrationFailed can come from any
                // participant (orchestrator, source, or target).
                // Without binding, a forged MigrationFailed from any
                // peer drives a rollback after a legitimate
                // cutover. Accept only when from_node matches a
                // recorded principal on at least one of the local
                // handler views; if no record exists at all (e.g.,
                // late-arriving for a migration already cleaned up),
                // drop silently rather than abort phantom state.
                let recorded = [
                    self.orchestrator.source_node(daemon_origin),
                    self.orchestrator.target_node(daemon_origin),
                    self.source_handler.orchestrator_node(daemon_origin),
                    self.target_handler.orchestrator_node(daemon_origin),
                ];
                let known = recorded.iter().any(|p| p.is_some());
                if known && !recorded.contains(&Some(from_node)) {
                    return Err(MigrationError::WrongPeer {
                        daemon_origin,
                        from: from_node,
                        expected: recorded.iter().find_map(|p| *p).unwrap_or(0),
                    });
                }
                // Fire the SDK's observer BEFORE abort, so the
                // observer sees the structured reason while the
                // migration record is still alive — the SDK uses
                // this to surface NotReady vs terminal to the
                // caller of `MigrationHandle::wait`.
                if let Some(cb) = &self.failure_callback {
                    cb(daemon_origin, reason.clone());
                }
                // Abort on all local handlers. This is correct for
                // terminal reasons; for `NotReady` the SDK may
                // elect to retry, which will re-`start_migration`
                // from scratch (re-snapshotting on local source,
                // re-sending TakeSnapshot on remote source).
                let _ = self.source_handler.abort(daemon_origin);
                let _ = self.target_handler.abort(daemon_origin);
                let _ = self
                    .orchestrator
                    .abort_migration_with_reason(daemon_origin, reason);
                // Also drop any partial reassembler we accumulated
                // as the target. The local-source-failure path
                // (`fail_migration_with_reason`, line ~1061)
                // already clears this; an inbound `MigrationFailed`
                // after the target had received some snapshot
                // chunks would otherwise leave ~`chunk_size *
                // chunks_received` bytes pinned in the DashMap
                // forever (or until the same origin migrated again
                // with a higher `seq_through`). With many ephemeral
                // daemons this would be an unbounded leak.
                self.reassemblers.remove(&daemon_origin);

                // Cleanup completeness: neither `StandbyGroup` nor
                // `CapabilityIndex` holds per-daemon
                // migration-coupled state today.
                //
                //   * `StandbyGroup::promote` is synchronous —
                //     either succeeds or rolls back atomically.
                //     There is no "promotion in flight across
                //     migration phases" state.
                //   * `CapabilityIndex` indexes by `node_id`, not
                //     by `daemon_origin` (verified by `grep -rn
                //     daemon_origin src/adapter/net/behavior/
                //     capability.rs` returning no matches).
                //     Capabilities are node-level; failure of a
                //     specific daemon's migration doesn't change
                //     what the source node is advertising.
                //
                // So no additional teardown is needed today. If a
                // future change adds per-daemon coupling in either
                // subsystem, the regression test for this invariant
                // fires loudly and the maintainer must wire
                // teardown HERE.
            }

            MigrationMessage::BufferedEvents {
                daemon_origin,
                events,
            } => {
                // We are the target — replay events
                let replayed_seq = self.target_handler.replay_events(daemon_origin, events)?;

                // Tell orchestrator we're done replaying
                let reply = MigrationMessage::ReplayComplete {
                    daemon_origin,
                    replayed_seq,
                };
                let dest = self
                    .target_handler
                    .orchestrator_node(daemon_origin)
                    .unwrap_or(from_node);
                outbound.push(OutboundMigrationMessage {
                    dest_node: dest,
                    payload: wire::encode(&reply)?,
                });
            }
        }

        Ok(outbound)
    }

    /// Feed a snapshot chunk into the target-side reassembler. When the
    /// full snapshot is assembled, resolve a factory for the daemon and
    /// call `restore_snapshot`, then emit `RestoreComplete` back to the
    /// source node (`from_node`).
    ///
    /// Returns `Ok(None)` while waiting for more chunks, `Ok(Some(outbound))`
    /// with the `RestoreComplete` (or a `MigrationFailed`) once restore has
    /// been attempted.
    fn restore_on_target(
        &self,
        daemon_origin: u64,
        snapshot_bytes: Vec<u8>,
        seq_through: u64,
        chunk_index: u32,
        total_chunks: u32,
        from_node: u64,
    ) -> Result<Option<Vec<OutboundMigrationMessage>>, MigrationError> {
        let reassembler_entry = self
            .reassemblers
            .entry(daemon_origin)
            .or_insert_with(|| Mutex::new(SnapshotReassembler::new()));

        let assembled = {
            let mut reassembler = reassembler_entry.lock();
            reassembler
                .feed(
                    daemon_origin,
                    snapshot_bytes,
                    seq_through,
                    chunk_index,
                    total_chunks,
                )
                .map_err(|e| {
                    MigrationError::StateFailed(format!("snapshot reassembly failed: {:?}", e))
                })?
        };
        drop(reassembler_entry); // release DashMap read lock

        let assembled_bytes = match assembled {
            Some(bytes) => bytes,
            None => return Ok(None), // still waiting for more chunks
        };

        // Drop the reassembler entry now that we've completed.
        self.reassemblers.remove(&daemon_origin);

        // Parse the snapshot. A parse failure is a hard migration failure.
        let snapshot = match StateSnapshot::from_bytes(&assembled_bytes) {
            Some(s) => s,
            None => {
                return Ok(Some(self.fail_migration(
                    daemon_origin,
                    from_node,
                    "failed to parse snapshot bytes on target",
                )?));
            }
        };

        // `source_node` is the daemon's pre-migration host — tracked here
        // only for the target-handler's internal bookkeeping. It is NOT
        // where `RestoreComplete` gets sent (see below).
        let source_node = self
            .orchestrator
            .source_node(daemon_origin)
            .unwrap_or(from_node);

        // If this origin is already under migration here, the source is
        // retrying because our earlier `RestoreComplete` didn't make it
        // back. Don't touch the already-restored daemon; just re-emit
        // `RestoreComplete` so the orchestrator can advance. This also
        // means we DO NOT consume the factory on the retry — the factory
        // registration must survive until the migration is truly complete
        // (`ActivateAck`), not just until the first locally-successful
        // restore.
        if !self.target_handler.is_migrating(daemon_origin) {
            // Readiness check first: if the runtime is still in
            // `Registering`, respond `NotReady` so the source can
            // retry with backoff rather than burning the attempt
            // on a target that's still booting.
            if let Some(readiness) = &self.readiness_callback {
                if !readiness() {
                    return Ok(Some(self.fail_migration_with_reason(
                        daemon_origin,
                        from_node,
                        crate::adapter::net::compute::MigrationFailureReason::NotReady,
                    )?));
                }
            }

            let inputs = match self.target_handler.factories().construct(daemon_origin) {
                Some(i) => i,
                None => {
                    return Ok(Some(self.fail_migration_with_reason(
                        daemon_origin,
                        from_node,
                        crate::adapter::net::compute::MigrationFailureReason::FactoryNotFound,
                    )?));
                }
            };

            // Identity envelope: if the snapshot carries one and we
            // have the X25519 private key to unseal it, the envelope
            // supplies the real daemon keypair. Otherwise fall back
            // to whatever keypair the factory was registered with —
            // which, for public-identity migrations or pre-Stage-5b
            // callers, is either a placeholder or a manually-shared
            // keypair. A present-but-invalid envelope is a hard
            // failure, not a fallback — otherwise an attacker who
            // tampers with the envelope could downgrade identity
            // transport silently.
            let keypair = match self.resolve_restore_keypair(&snapshot, inputs.keypair.as_ref()) {
                Ok(kp) => kp,
                Err(e) => {
                    return Ok(Some(self.fail_migration(
                        daemon_origin,
                        from_node,
                        &format!("identity envelope open failed: {e}"),
                    )?));
                }
            };

            let daemon = inputs.daemon;
            if let Err(e) = self.target_handler.restore_snapshot(
                RestoreContext {
                    daemon_origin,
                    snapshot: &snapshot,
                    source_node,
                    // orchestrator: whoever forwarded SnapshotReady to us
                    orchestrator_node: from_node,
                },
                keypair,
                move || daemon,
                inputs.config,
            ) {
                // Factory is still registered — next `SnapshotReady` for
                // this origin (e.g., from an orchestrator retry) can try
                // again. On successful completion (`complete()`), the
                // factory is auto-removed so a stale or replayed
                // SnapshotReady can't re-trigger restore against what is
                // already the authoritative copy.
                return Ok(Some(self.fail_migration(
                    daemon_origin,
                    from_node,
                    &format!("restore_snapshot failed: {:?}", e),
                )?));
            }

            // Fire the post-restore callback. The SDK-supplied hook
            // drives channel re-bind replay (Stage 3 of the channel
            // re-bind plan): walks the restored daemon's ledger and
            // spawns async `subscribe_channel` calls so publishers
            // start fanning out to the target before the source
            // tears down. Sync callback; the hook itself should
            // `tokio::spawn` the actual work.
            if let Some(cb) = &self.post_restore_callback {
                cb(daemon_origin);
            }
        }

        // Route `RestoreComplete` to the recorded orchestrator. Only the
        // orchestrator holds the migration record; sending to a relay
        // would stall the state machine. `from_node` is used as a
        // fallback when the target-side record has been lost (e.g. a
        // very late chunk after the migration record timed out).
        let reply = MigrationMessage::RestoreComplete {
            daemon_origin,
            restored_seq: seq_through,
        };
        let dest = self
            .target_handler
            .orchestrator_node(daemon_origin)
            .unwrap_or(from_node);
        Ok(Some(vec![OutboundMigrationMessage {
            dest_node: dest,
            payload: wire::encode(&reply)?,
        }]))
    }

    /// Source-side helper: if we have an identity-transport context
    /// and the target's X25519 pubkey is available, seal the
    /// daemon's keypair into the snapshot.
    ///
    /// Resolution:
    /// - **Prerequisite missing** (no context, target key not known,
    ///   or daemon keypair absent from the local registry): return
    ///   the snapshot unchanged. This is the legitimate public-
    ///   identity / NKpsk0-responder fallback — the target is
    ///   expected to have a pre-registered keypair.
    /// - **Prerequisites met, seal crypto succeeds**: return the
    ///   sealed snapshot.
    /// - **Prerequisites met, seal crypto fails**: fail the
    ///   migration. Silently downgrading to unsealed transport would
    ///   break the identity-transport guarantee the caller installed
    ///   `identity_context` to obtain — the target would restore
    ///   using whatever fallback keypair the factory registry carries
    ///   (possibly nothing, possibly a stale mismatch), and any later
    ///   signature the daemon produces on the target would be bound
    ///   to the wrong identity. The only honest response is to abort.
    fn maybe_seal_envelope(
        &self,
        snapshot: StateSnapshot,
        daemon_origin: u64,
        target_node: u64,
    ) -> Result<StateSnapshot, MigrationError> {
        let Some(ctx) = &self.identity_context else {
            return Ok(snapshot);
        };
        // Skip if the snapshot already carries an envelope (e.g. the
        // SDK pre-sealed at `start_migration` time for a local-source
        // case).
        if snapshot.identity_envelope.is_some() {
            return Ok(snapshot);
        }
        let Some(target_pub) = (ctx.peer_static_lookup)(target_node) else {
            return Ok(snapshot);
        };
        // Find the daemon's keypair in the local registry. The
        // orchestrator + source_handler + target_handler share one
        // registry, so whichever owns this daemon, we see it.
        let kp = match self
            .source_handler_registry_keypair(daemon_origin)
            .or_else(|| self.target_handler_registry_keypair(daemon_origin))
        {
            Some(kp) => kp,
            None => return Ok(snapshot),
        };
        snapshot
            .with_identity_envelope(&kp, target_pub)
            .map_err(|e| {
                MigrationError::StateFailed(format!(
                    "identity envelope seal failed for daemon {daemon_origin:#x}: {e}"
                ))
            })
    }

    /// Read-only keypair fetch from the shared daemon registry
    /// reachable via `source_handler`. `source_handler` and
    /// `target_handler` hold `Arc` clones of the same registry in
    /// typical wiring, so checking via source is sufficient; the
    /// `target_handler_registry_keypair` fallback exists for
    /// asymmetric setups where they diverge.
    fn source_handler_registry_keypair(&self, daemon_origin: u64) -> Option<EntityKeypair> {
        let _ = daemon_origin;
        // `MigrationSourceHandler` doesn't expose the registry
        // publicly, so reach through the orchestrator which shares
        // the same `Arc<DaemonRegistry>`.
        self.orchestrator
            .daemon_registry()
            .daemon_keypair(daemon_origin)
    }

    fn target_handler_registry_keypair(&self, daemon_origin: u64) -> Option<EntityKeypair> {
        // Parallel path — the target-side registry may in some
        // configurations be distinct. Today it's the same `Arc`, so
        // this returns the same value as the source path; kept as a
        // seam.
        self.orchestrator
            .daemon_registry()
            .daemon_keypair(daemon_origin)
    }

    /// Target-side helper: pick the keypair to hand to
    /// `restore_snapshot`. Resolution order:
    ///
    /// 1. If the snapshot carries an identity envelope AND we have
    ///    the X25519 private key to unseal it → use the envelope's
    ///    keypair. (Non-envelope cases fall through.)
    /// 2. Otherwise, if `fallback` was provided — the factory was
    ///    registered via `DaemonFactoryRegistry::register` with a
    ///    pre-provisioned keypair — use that.
    /// 3. If neither is available (placeholder registration +
    ///    no envelope in the snapshot), fail: a placeholder factory
    ///    expects the envelope to supply the keypair, and its
    ///    absence means the source skipped identity transport
    ///    without the target being prepared for that.
    ///
    /// Once an envelope is **present** on the snapshot, envelope
    /// transport is mandatory — the fallback keypair is NEVER used
    /// on that path. Present-but-invalid envelopes are terminal
    /// (propagating the envelope error rather than falling back
    /// prevents an attacker from downgrading identity transport by
    /// tampering with the envelope bytes), and a misbehaving
    /// `unseal_snapshot` that returns `Ok(None)` despite an
    /// envelope being present is treated as a terminal error for
    /// the same reason: silently falling through to the
    /// pre-provisioned keypair would defeat the identity-transport
    /// guarantee callers installed the context to obtain.
    fn resolve_restore_keypair(
        &self,
        snapshot: &StateSnapshot,
        fallback: Option<&EntityKeypair>,
    ) -> Result<EntityKeypair, String> {
        if let (Some(ctx), Some(_)) = (&self.identity_context, &snapshot.identity_envelope) {
            // The private key stays inside the closure owned by
            // `ctx.unseal_snapshot` — the dispatcher never sees it.
            //
            // Both `Ok(None)` and `Err` terminate resolution. The
            // envelope has been attached to the snapshot, so we've
            // already committed to envelope transport; falling back
            // to the pre-provisioned keypair here would silently
            // downgrade a migration the caller asked to carry
            // identity. A conforming `unseal_snapshot` returns
            // `Ok(Some(_))` or `Err(_)` when handed a snapshot with
            // a present envelope — `Ok(None)` indicates a broken
            // unsealer and must not mask the breakage.
            return match (ctx.unseal_snapshot)(snapshot) {
                Ok(Some(kp)) => Ok(kp),
                Ok(None) => Err("identity envelope present on snapshot but \
                     `unseal_snapshot` returned Ok(None) — refusing to \
                     fall back to the pre-provisioned keypair; a \
                     present envelope mandates envelope-sourced \
                     identity transport"
                    .to_string()),
                Err(e) => Err(format!("{e}")),
            };
        }
        fallback.cloned().ok_or_else(|| {
            "placeholder factory registered but snapshot has no \
             identity envelope (and no local fallback keypair available)"
                .to_string()
        })
    }

    /// Build a `MigrationFailed` outbound message and clean up local state.
    /// Convenience wrapper that wraps `reason` in
    /// [`MigrationFailureReason::StateFailed`] for generic failures;
    /// callers that need a specific reason code (e.g. `NotReady`,
    /// `FactoryNotFound`) should use
    /// [`Self::fail_migration_with_reason`].
    fn fail_migration(
        &self,
        daemon_origin: u64,
        from_node: u64,
        reason: &str,
    ) -> Result<Vec<OutboundMigrationMessage>, MigrationError> {
        self.fail_migration_with_reason(
            daemon_origin,
            from_node,
            crate::adapter::net::compute::MigrationFailureReason::StateFailed(reason.to_string()),
        )
    }

    /// Build a `MigrationFailed` outbound message with a structured
    /// reason. Clean-up is the same as [`Self::fail_migration`]: the
    /// reassembler entry + target-handler state are dropped so a
    /// retry from the source can start fresh (unless the reason is
    /// `FactoryNotFound` — a retry won't find what isn't there).
    fn fail_migration_with_reason(
        &self,
        daemon_origin: u64,
        from_node: u64,
        reason: crate::adapter::net::compute::MigrationFailureReason,
    ) -> Result<Vec<OutboundMigrationMessage>, MigrationError> {
        tracing::warn!(
            daemon_origin = format!("{:#x}", daemon_origin),
            reason = %reason,
            "migration failed on target",
        );
        self.reassemblers.remove(&daemon_origin);
        let _ = self.target_handler.abort(daemon_origin);
        let msg = MigrationMessage::MigrationFailed {
            daemon_origin,
            reason,
        };
        Ok(vec![OutboundMigrationMessage {
            dest_node: from_node,
            payload: wire::encode(&msg)?,
        }])
    }

    /// Get a reference to the orchestrator.
    pub fn orchestrator(&self) -> &Arc<MigrationOrchestrator> {
        &self.orchestrator
    }

    /// Get a reference to the source handler.
    pub fn source_handler(&self) -> &Arc<MigrationSourceHandler> {
        &self.source_handler
    }

    /// Get a reference to the target handler.
    pub fn target_handler(&self) -> &Arc<MigrationTargetHandler> {
        &self.target_handler
    }
}

impl std::fmt::Debug for MigrationSubprotocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationSubprotocolHandler")
            .field("local_node_id", &format!("{:#x}", self.local_node_id))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityFilter;
    use crate::adapter::net::compute::orchestrator::wire;
    use crate::adapter::net::compute::{
        DaemonError, DaemonHost, DaemonHostConfig, DaemonRegistry, MeshDaemon,
        MigrationOrchestrator, MigrationSourceHandler, MigrationTargetHandler,
    };
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::state::causal::CausalEvent;
    use bytes::Bytes;

    /// Regression (Cubic-AI P1: leaking Noise static private key):
    /// `MigrationIdentityContext` previously exposed
    /// `pub local_x25519_priv: [u8; 32]`, which meant any SDK caller
    /// holding a context (or calling the now-removed
    /// `MeshNode::static_x25519_priv`) could copy the node's
    /// long-term identity key out.
    ///
    /// The fix moves the key into an `unseal_snapshot` closure owned
    /// by the context, with the raw bytes never reachable as a
    /// readable field. This test pins the struct's size so a
    /// blind re-add of a `[u8; 32]` or similar secret-bearing field
    /// trips the canary.
    ///
    /// Two `Arc<dyn Fn ...>` are fat pointers — two `usize`s each —
    /// so the context is `4 * size_of::<usize>()`. If someone adds a
    /// 32-byte key field, size jumps to that + 32 and this assertion
    /// fails. Not a true API-surface guard (PR review is the real
    /// guard) but an honest canary for the specific regression.
    #[test]
    fn migration_identity_context_has_no_plaintext_secret_field_regression() {
        use std::mem::size_of;
        let fat_ptr = 2 * size_of::<usize>();
        assert_eq!(
            size_of::<MigrationIdentityContext>(),
            2 * fat_ptr,
            "MigrationIdentityContext must stay two Arc<dyn Fn ...> and \
             nothing else — a size change means a new field was added, \
             most likely re-exposing the Noise static private key the \
             way `local_x25519_priv: [u8; 32]` used to.",
        );
    }

    struct TestDaemon {
        value: u64,
    }

    impl MeshDaemon for TestDaemon {
        fn name(&self) -> &str {
            "test"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            self.value += 1;
            Ok(vec![])
        }
        fn snapshot(&self) -> Option<Bytes> {
            Some(Bytes::from(self.value.to_le_bytes().to_vec()))
        }
        fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
            self.value = u64::from_le_bytes(state[..8].try_into().unwrap());
            Ok(())
        }
    }

    fn setup() -> (MigrationSubprotocolHandler, Arc<DaemonRegistry>, u64) {
        let reg = Arc::new(DaemonRegistry::new());
        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let host = DaemonHost::new(
            Box::new(TestDaemon { value: 100 }),
            kp,
            DaemonHostConfig::default(),
        );
        reg.register(host).unwrap();

        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new(reg.clone()));

        let handler = MigrationSubprotocolHandler::new(orch, source, target, 0x1111);
        (handler, reg, origin)
    }

    #[test]
    fn test_handle_take_snapshot() {
        let (handler, _reg, origin) = setup();

        let msg = MigrationMessage::TakeSnapshot {
            daemon_origin: origin,
            target_node: 0x2222,
        };
        let encoded = wire::encode(&msg).unwrap();

        let outbound = handler.handle_message(&encoded, 0x3333).unwrap();
        assert!(!outbound.is_empty());

        // Should get SnapshotReady back
        let reply = wire::decode(&outbound[0].payload).unwrap();
        match reply {
            MigrationMessage::SnapshotReady { daemon_origin, .. } => {
                assert_eq!(daemon_origin, origin);
            }
            _ => panic!("expected SnapshotReady"),
        }
    }

    /// Regression (Cubic-AI P2): `maybe_seal_envelope` used to
    /// swallow seal-crypto errors (e.g. a public-only source
    /// keypair) and return the **unsealed** snapshot to the caller,
    /// downgrading identity transport silently. Any target that
    /// relied on the envelope to supply the daemon keypair
    /// (`expect_migration` placeholder + no out-of-band keypair)
    /// would then fail to restore, or — worse — restore under a
    /// stale fallback keypair and produce mis-signed outputs.
    ///
    /// The fix makes the helper return `Result` and propagate seal
    /// failures as `MigrationError::StateFailed`. Callers abort
    /// rather than ship an unsealed snapshot they didn't ask for.
    ///
    /// This test stages the exact failure mode: register a daemon
    /// with a public-only `EntityKeypair`, install an identity
    /// context that successfully resolves the target's static, then
    /// ask `maybe_seal_envelope` to seal. The underlying crypto
    /// rejects (public-only can't sign the attestation) and the
    /// helper must surface it.
    #[test]
    fn maybe_seal_envelope_propagates_seal_failures() {
        use crate::adapter::net::identity::IdentityEnvelope;
        use crate::adapter::net::state::snapshot::StateSnapshot;
        use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

        // Target's X25519 static — arbitrary fresh key.
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let target_priv = X25519Secret::from(seed);
        let target_pub = *X25519Pub::from(&target_priv).as_bytes();
        let target_node_id: u64 = 0x2222;

        // Daemon keypair: generate a real one for the entity_id,
        // then strip the signing half via `public_only`. The
        // registry will hand this out; the seal will try to sign
        // the attestation transcript and fail.
        let real_kp = EntityKeypair::generate();
        let origin_hash = real_kp.origin_hash();
        let public_only_kp = EntityKeypair::public_only(real_kp.entity_id().clone());
        assert!(
            public_only_kp.is_read_only(),
            "fixture: must be public-only",
        );

        // Register a DaemonHost using the public-only keypair so
        // `source_handler_registry_keypair` → `daemon_keypair`
        // returns it. The daemon body is irrelevant.
        let reg = Arc::new(DaemonRegistry::new());
        let host = DaemonHost::new(
            Box::new(TestDaemon { value: 0 }),
            public_only_kp,
            DaemonHostConfig::default(),
        );
        reg.register(host).unwrap();

        // Build a matching snapshot the normal way — same origin,
        // same entity_id.
        let snapshot = StateSnapshot {
            version: crate::adapter::net::state::snapshot::SNAPSHOT_VERSION,
            entity_id: real_kp.entity_id().clone(),
            chain_link: crate::adapter::net::state::causal::CausalLink {
                origin_hash,
                horizon_encoded: 0,
                sequence: 0,
                parent_hash: 0,
            },
            through_seq: 0,
            state: Bytes::from_static(&[0u8; 8]),
            horizon: Default::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };

        // Wire the identity context so `peer_static_lookup` returns
        // the target pub — i.e., every prerequisite is satisfied.
        // The unseal closure isn't exercised on the source path.
        let unseal_snapshot: EnvelopeUnsealFn =
            Arc::new(move |snap: &StateSnapshot| snap.open_identity_envelope(&target_priv));
        let peer_static_lookup: Arc<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync> =
            Arc::new(move |nid| {
                if nid == target_node_id {
                    Some(target_pub)
                } else {
                    None
                }
            });
        let ctx = MigrationIdentityContext {
            unseal_snapshot,
            peer_static_lookup,
        };

        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new(reg));
        let handler = MigrationSubprotocolHandler::with_hooks(
            orch,
            source,
            target,
            0x1111,
            MigrationHandlerHooks {
                identity: Some(ctx),
                ..Default::default()
            },
        );

        // With all prerequisites satisfied but crypto guaranteed to
        // fail (public-only keypair can't attest), the helper must
        // surface an error — not return the unsealed snapshot.
        let err = handler
            .maybe_seal_envelope(snapshot, origin_hash, target_node_id)
            .expect_err(
                "public-only daemon keypair must fail to seal; silently returning the \
                 unsealed snapshot breaks the identity-transport guarantee",
            );
        match err {
            MigrationError::StateFailed(ref msg) => {
                assert!(
                    msg.contains("envelope seal failed"),
                    "expected 'envelope seal failed' in message, got: {msg}",
                );
                assert!(
                    msg.contains(&format!("{origin_hash:#x}")),
                    "expected origin_hash in message, got: {msg}",
                );
            }
            other => panic!("expected StateFailed, got {other:?}"),
        }

        // Belt-and-braces: the unsealed-fallback path (no context)
        // still works — proves this test isn't accidentally
        // asserting `maybe_seal_envelope` always errors.
        let handler_no_ctx = MigrationSubprotocolHandler::new(
            Arc::new(MigrationOrchestrator::new(
                Arc::new(DaemonRegistry::new()),
                0x1111,
            )),
            Arc::new(MigrationSourceHandler::new(Arc::new(DaemonRegistry::new()))),
            Arc::new(MigrationTargetHandler::new(Arc::new(DaemonRegistry::new()))),
            0x1111,
        );
        let snapshot2 = StateSnapshot {
            version: crate::adapter::net::state::snapshot::SNAPSHOT_VERSION,
            entity_id: real_kp.entity_id().clone(),
            chain_link: crate::adapter::net::state::causal::CausalLink {
                origin_hash,
                horizon_encoded: 0,
                sequence: 0,
                parent_hash: 0,
            },
            through_seq: 0,
            state: Bytes::from_static(&[0u8; 8]),
            horizon: Default::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: None,
            head_payload: None,
        };
        let passthrough = handler_no_ctx
            .maybe_seal_envelope(snapshot2, origin_hash, target_node_id)
            .expect("no ctx = ok unchanged");
        assert!(passthrough.identity_envelope.is_none());
        let _ = IdentityEnvelope::new; // silence unused import
    }

    /// Regression (Cubic-AI P1): seal failure inside the
    /// `TakeSnapshot` dispatcher path was propagated as a
    /// dispatcher error via `?`, leaving the source's
    /// `start_snapshot` record in place AND starving the remote
    /// orchestrator — it's waiting for a `SnapshotReady` that
    /// will never arrive.
    ///
    /// The fix converts seal failures into a `MigrationFailed`
    /// wire reply back to the orchestrator, aborts the local
    /// source-handler record, and returns the single-message
    /// outbound so the caller dispatches it normally.
    ///
    /// Test: construct a public-only daemon keypair (seal will
    /// fail at attestation), wire an identity context that
    /// surfaces the target's static, drive the handler with a
    /// `TakeSnapshot` message. Assert:
    /// 1. `handle_message` returns `Ok(outbound)` — no bubble-up.
    /// 2. The outbound contains exactly one `MigrationFailed`
    ///    addressed to the originator (`from_node`).
    /// 3. The `source_handler` no longer tracks this daemon
    ///    (abort ran).
    #[test]
    fn take_snapshot_seal_failure_emits_migration_failed_reply() {
        use crate::adapter::net::state::snapshot::StateSnapshot;
        use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

        // Target static for the context's peer lookup. The value
        // isn't exercised by the seal (it fails at attestation
        // first due to public-only keypair), but the context needs
        // a non-None for the lookup or it'd short-circuit before
        // hitting the seal at all.
        let mut x25519_seed = [0u8; 32];
        getrandom::fill(&mut x25519_seed).unwrap();
        let target_priv = X25519Secret::from(x25519_seed);
        let target_pub = *X25519Pub::from(&target_priv).as_bytes();
        let target_node_id: u64 = 0x2222;
        let orchestrator_node_id: u64 = 0x3333;

        // Daemon registered with a public-only keypair — seal's
        // attestation step needs the signing half, so this guarantees
        // `maybe_seal_envelope` returns Err once the seal runs.
        let real_kp = EntityKeypair::generate();
        let origin = real_kp.origin_hash();
        let public_only_kp = EntityKeypair::public_only(real_kp.entity_id().clone());

        let reg = Arc::new(DaemonRegistry::new());
        let host = DaemonHost::new(
            Box::new(TestDaemon { value: 7 }),
            public_only_kp,
            DaemonHostConfig::default(),
        );
        reg.register(host).unwrap();

        let unseal: EnvelopeUnsealFn =
            Arc::new(move |snap: &StateSnapshot| snap.open_identity_envelope(&target_priv));
        let peer_static_lookup: Arc<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync> =
            Arc::new(move |nid| {
                if nid == target_node_id {
                    Some(target_pub)
                } else {
                    None
                }
            });
        let ctx = MigrationIdentityContext {
            unseal_snapshot: unseal,
            peer_static_lookup,
        };

        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new(reg));
        let handler = MigrationSubprotocolHandler::with_hooks(
            orch,
            source.clone(),
            target,
            0x1111,
            MigrationHandlerHooks {
                identity: Some(ctx),
                ..Default::default()
            },
        );

        // Drive a `TakeSnapshot` from the fictional orchestrator.
        let msg = MigrationMessage::TakeSnapshot {
            daemon_origin: origin,
            target_node: target_node_id,
        };
        let encoded = wire::encode(&msg).unwrap();
        let outbound = handler
            .handle_message(&encoded, orchestrator_node_id)
            .expect("seal failure must not bubble up as dispatcher error");

        // Exactly one message back, addressed to the orchestrator
        // that sent TakeSnapshot.
        assert_eq!(
            outbound.len(),
            1,
            "expected single MigrationFailed reply, got {} messages",
            outbound.len(),
        );
        assert_eq!(outbound[0].dest_node, orchestrator_node_id);

        let reply = wire::decode(&outbound[0].payload).unwrap();
        match reply {
            MigrationMessage::MigrationFailed {
                daemon_origin,
                reason,
            } => {
                assert_eq!(daemon_origin, origin);
                let reason_msg = format!("{reason}");
                assert!(
                    reason_msg.contains("identity envelope seal failed"),
                    "expected seal-failure reason, got: {reason_msg}",
                );
            }
            other => panic!("expected MigrationFailed, got {other:?}"),
        }

        // Source-handler record was aborted — the pre-fix code
        // left this in place indefinitely.
        assert!(
            source.phase(origin).is_none(),
            "source_handler must have cleared its record for {origin:#x} after a failed TakeSnapshot",
        );
    }

    #[test]
    fn test_handle_migration_failed() {
        let (handler, _reg, origin) = setup();

        let msg = MigrationMessage::MigrationFailed {
            daemon_origin: origin,
            reason: crate::adapter::net::compute::MigrationFailureReason::StateFailed(
                "test failure".into(),
            ),
        };
        let encoded = wire::encode(&msg).unwrap();

        // Should not error — just cleans up
        let outbound = handler.handle_message(&encoded, 0x3333).unwrap();
        assert!(outbound.is_empty());
    }

    /// Regression for a test that the SDK-level suite could not
    /// honestly exercise: when the factory registry carries a
    /// pre-provisioned **fallback keypair** AND the snapshot carries
    /// a **valid identity envelope**, the envelope's keypair must
    /// win. The SDK test that used to assert this could only register
    /// a fallback keyed by the envelope's own `origin_hash`, because
    /// `origin_hash` is derived from the keypair bytes — there's no
    /// way for a user-level API to supply a "wrong" keypair at a
    /// given `origin_hash`.
    ///
    /// This unit test reaches directly into `resolve_restore_keypair`
    /// with two genuinely-distinct keypairs and asserts the envelope
    /// overrides. If someone later flips the resolution order (e.g.
    /// preferring the fallback for some misguided "backward-
    /// compatibility" reason), this test trips.
    #[test]
    fn envelope_keypair_overrides_fallback_placeholder() {
        use crate::adapter::net::identity::IdentityEnvelope;
        use crate::adapter::net::state::causal::CausalLink;
        use crate::adapter::net::state::snapshot::StateSnapshot;
        use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

        // Target's Noise static X25519 keypair — used to seal and
        // then unseal the envelope.
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let target_priv = X25519Secret::from(seed);
        let target_pub = *X25519Pub::from(&target_priv).as_bytes();

        // Real source-side daemon keypair: the one that should end
        // up being used for restore.
        let real_kp = EntityKeypair::generate();
        // Wrong fallback keypair: the one that would be used if
        // someone flipped the resolution order.
        let wrong_fallback = EntityKeypair::generate();
        assert_ne!(
            real_kp.entity_id(),
            wrong_fallback.entity_id(),
            "fixture: real and fallback must differ",
        );

        let chain_link = CausalLink {
            origin_hash: real_kp.origin_hash(),
            horizon_encoded: 0,
            sequence: 0,
            parent_hash: 0,
        };
        let envelope =
            IdentityEnvelope::new(&real_kp, target_pub, &chain_link).expect("seal envelope");

        // Snapshot carrying the envelope. The envelope's origin_hash
        // matches `real_kp`; the test doesn't need the rest of the
        // snapshot to validate, only the envelope-open path.
        let snapshot = StateSnapshot {
            version: crate::adapter::net::state::snapshot::SNAPSHOT_VERSION,
            entity_id: real_kp.entity_id().clone(),
            chain_link,
            through_seq: 0,
            state: Bytes::new(),
            horizon: Default::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: Some(envelope),
            head_payload: None,
        };

        // Build a handler with an identity_context whose unseal
        // closure holds the target's private key. This mirrors what
        // `MeshNode::migration_identity_context` produces.
        let priv_for_closure = target_priv.clone();
        let unseal_snapshot: EnvelopeUnsealFn =
            Arc::new(move |snap: &StateSnapshot| snap.open_identity_envelope(&priv_for_closure));
        let peer_static_lookup: Arc<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync> =
            Arc::new(|_| None);
        let ctx = MigrationIdentityContext {
            unseal_snapshot,
            peer_static_lookup,
        };

        let reg = Arc::new(DaemonRegistry::new());
        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new(reg));
        let handler = MigrationSubprotocolHandler::with_hooks(
            orch,
            source,
            target,
            0x1111,
            MigrationHandlerHooks {
                identity: Some(ctx),
                ..Default::default()
            },
        );

        // Both envelope and fallback present — envelope wins.
        let resolved = handler
            .resolve_restore_keypair(&snapshot, Some(&wrong_fallback))
            .expect("resolve");
        assert_eq!(
            resolved.entity_id(),
            real_kp.entity_id(),
            "envelope's keypair must win over the pre-provisioned fallback — \
             flipping this order silently downgrades identity transport to \
             whatever the factory registry was pre-populated with",
        );
        assert_ne!(
            resolved.entity_id(),
            wrong_fallback.entity_id(),
            "fallback must NOT leak through when the envelope is valid",
        );

        // Sanity: with no envelope on the snapshot, fallback is
        // returned verbatim. Proves the `Some(envelope) → envelope`
        // branch above wasn't passing by coincidence.
        let snapshot_no_envelope = StateSnapshot {
            identity_envelope: None,
            head_payload: None,
            ..snapshot.clone()
        };
        let resolved_fallback = handler
            .resolve_restore_keypair(&snapshot_no_envelope, Some(&wrong_fallback))
            .expect("resolve with fallback only");
        assert_eq!(resolved_fallback.entity_id(), wrong_fallback.entity_id());
    }

    /// Regression (Cubic-AI P2): once an identity envelope is
    /// present on the snapshot, resolution must commit to
    /// envelope transport. A misbehaving `unseal_snapshot` that
    /// returns `Ok(None)` — e.g., a partially-implemented or
    /// buggy custom closure — previously made the dispatcher
    /// fall through to the pre-provisioned fallback keypair,
    /// silently downgrading a migration the caller had opted
    /// into envelope transport for.
    ///
    /// The fix treats `Ok(None)` from a present-envelope snapshot
    /// as a terminal error, matching the policy for an explicit
    /// `Err(...)` from unseal.
    ///
    /// Test: construct a snapshot that carries an envelope, wire
    /// an identity context whose `unseal_snapshot` ignores the
    /// snapshot entirely and returns `Ok(None)`. Provide a
    /// (wrong) fallback keypair. The resolver must refuse,
    /// returning `Err(...)` — not the fallback.
    #[test]
    fn envelope_present_but_unseal_returns_none_fails_rather_than_fallback() {
        use crate::adapter::net::identity::IdentityEnvelope;
        use crate::adapter::net::state::snapshot::StateSnapshot;
        use x25519_dalek::{PublicKey as X25519Pub, StaticSecret as X25519Secret};

        // Fresh X25519 keypair — seal recipient.
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let target_priv = X25519Secret::from(seed);
        let target_pub = *X25519Pub::from(&target_priv).as_bytes();

        // Real daemon keypair: builds a valid envelope so the
        // snapshot is well-formed from the wire's perspective.
        let real_kp = EntityKeypair::generate();
        let chain_link = crate::adapter::net::state::causal::CausalLink {
            origin_hash: real_kp.origin_hash(),
            horizon_encoded: 0,
            sequence: 0,
            parent_hash: 0,
        };
        let envelope =
            IdentityEnvelope::new(&real_kp, target_pub, &chain_link).expect("seal envelope");

        let snapshot = StateSnapshot {
            version: crate::adapter::net::state::snapshot::SNAPSHOT_VERSION,
            entity_id: real_kp.entity_id().clone(),
            chain_link,
            through_seq: 0,
            state: Bytes::new(),
            horizon: Default::default(),
            created_at: 0,
            bindings_bytes: Vec::new(),
            identity_envelope: Some(envelope),
            head_payload: None,
        };

        // Misbehaving unsealer: always returns `Ok(None)`, even
        // when handed a snapshot with a real envelope. Simulates
        // a partial implementation or a bug that would have
        // triggered the silent-downgrade previously.
        let unseal_snapshot: EnvelopeUnsealFn = Arc::new(|_snap: &StateSnapshot| Ok(None));
        let peer_static_lookup: Arc<dyn Fn(u64) -> Option<[u8; 32]> + Send + Sync> =
            Arc::new(|_| None);
        let ctx = MigrationIdentityContext {
            unseal_snapshot,
            peer_static_lookup,
        };

        let reg = Arc::new(DaemonRegistry::new());
        let orch = Arc::new(MigrationOrchestrator::new(reg.clone(), 0x1111));
        let source = Arc::new(MigrationSourceHandler::new(reg.clone()));
        let target = Arc::new(MigrationTargetHandler::new(reg));
        let handler = MigrationSubprotocolHandler::with_hooks(
            orch,
            source,
            target,
            0x1111,
            MigrationHandlerHooks {
                identity: Some(ctx),
                ..Default::default()
            },
        );

        // Fallback would have "succeeded" (wrong keypair, but
        // syntactically present) pre-fix. Post-fix the resolver
        // rejects because the envelope-present invariant commits
        // us to envelope transport.
        let wrong_fallback = EntityKeypair::generate();
        let err = handler
            .resolve_restore_keypair(&snapshot, Some(&wrong_fallback))
            .expect_err(
                "envelope present + unseal Ok(None) must fail; silently \
                 returning the fallback downgrades identity transport",
            );
        assert!(
            err.contains("refusing to fall back"),
            "expected 'refusing to fall back' in error message, got: {err}",
        );
    }

    /// CR-24: pin the no-per-daemon-coupling invariant for
    /// `StandbyGroup` and `CapabilityIndex`. The audit suggested
    /// the `MigrationFailed` arm needed teardown for both
    /// subsystems; investigation showed neither holds
    /// per-daemon migration-coupled state today (see the comment
    /// block at the MigrationFailed arm). This test fires loudly
    /// if a future change introduces such coupling, signalling
    /// that the maintainer MUST wire teardown into the arm.
    ///
    /// Mechanism: scan the source files for the canonical coupling
    /// shapes — `daemon_origin` field on `StandbyGroup` /
    /// `CapabilityIndex`, or migration-handler import of either
    /// type. Any match indicates the contract has changed and
    /// `migration_handler.rs:MigrationFailed` likely needs to
    /// call cleanup on the new state.
    #[test]
    fn cr24_no_per_daemon_migration_coupling_in_standby_or_capability() {
        let standby_src = include_str!("../compute/standby_group.rs");
        let capability_src = include_str!("../behavior/capability.rs");

        // Cubic P2: strip both line comments (`// ...`) AND block
        // comments (`/* ... */`, including `/** ... */` doc
        // comments) before scanning. The earlier filter only
        // skipped `//` lines, so a token mention inside
        // `/** ... */` would falsely trip the regression. We
        // strip block-comment ranges first; the per-line filter
        // then handles the line-comment case.
        fn strip_comments(src: &str) -> String {
            let bytes = src.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                // Skip block comment.
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        // Preserve newlines so per-line scanning still aligns.
                        if bytes[i] == b'\n' {
                            out.push(b'\n');
                        }
                        i += 1;
                    }
                    if i + 1 < bytes.len() {
                        i += 2; // skip closing */
                    }
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).into_owned()
        }

        let capability_clean = strip_comments(capability_src);
        let standby_clean = strip_comments(standby_src);

        // CapabilityIndex must NOT index by daemon_origin. Pinned
        // separately because it's the audit's specific claim.
        let capability_uses_daemon_origin = capability_clean.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//") && trimmed.contains("daemon_origin")
        });
        assert!(
            !capability_uses_daemon_origin,
            "CR-24 regression: CapabilityIndex now references `daemon_origin` in \
             non-comment source. The audit's CR-24 concern was that capabilities \
             tied to a migrating daemon need teardown on MigrationFailed. With \
             this new coupling the migration_handler MUST call \
             `capability_index.cleanup_origin(daemon_origin)` (or equivalent) \
             in the MigrationFailed arm. Add the call AND update this test."
        );

        // StandbyGroup must NOT have an "in-flight migration
        // promotion" field. The audit's scenario was "promotion
        // mid-flight" — for that to be a real concern, there
        // would need to be a state field (e.g. `pending_promotion:
        // Option<...>`) that survives across multiple migration-
        // handler dispatches.
        let standby_has_pending = standby_clean.lines().any(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                return false;
            }
            trimmed.contains("pending_promotion")
                || trimmed.contains("migration_in_flight")
                || trimmed.contains("in_migration:")
        });
        assert!(
            !standby_has_pending,
            "CR-24 regression: StandbyGroup now has a pending-promotion or \
             in-migration field. The audit's CR-24 concern was that a mid- \
             flight standby promotion needs teardown on MigrationFailed. With \
             this new coupling the migration_handler MUST call rollback on \
             StandbyGroup in the MigrationFailed arm. Add the rollback call \
             AND update this test."
        );
    }
}
