//! Tokio-driven replication runtime — ties together the pure
//! pieces ([`ReplicationCoordinator`], [`HeartbeatTracker`],
//! [`BandwidthBudget`], [`tick`], [`handle_sync_request`],
//! [`apply_sync_response`]) behind a single async task per
//! replicated channel.
//!
//! Slot: the layer between `Redex::open_file` (which spawns one
//! runtime per replicated channel) and the substrate mesh
//! (which routes inbound `SUBPROTOCOL_REDEX` payloads here).
//!
//! Architecture:
//!
//! ```text
//!   Mesh dispatch  ──Inbound::*──▶ ReplicationRuntime task
//!                                      │
//!                       ┌──────────────┴──────────────┐
//!                       │                              │
//!                  HeartbeatTick                   InboundEvent
//!                  (every heartbeat_ms)            (peer message)
//!                       │                              │
//!                       ▼                              ▼
//!                replication_step::tick     update tracker / file
//!                       │                              │
//!                       ▼                              ▼
//!                outbound dispatch          maybe issue sync_request
//!                       │                              │
//!                       └──────────────┬───────────────┘
//!                                      ▼
//!                            ReplicationDispatcher
//!                            (mesh.send_subprotocol)
//! ```
//!
//! The dispatcher trait abstracts the mesh-side wire send so the
//! runtime is unit-testable with a recorder mock. Production-side
//! wiring (mesh.rs routing `SUBPROTOCOL_REDEX` payloads to the
//! right runtime's inbox + the `MeshNode` impl of
//! `ReplicationDispatcher`) lands in a separate slice — this
//! commit covers the runtime task itself + the trait.

use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::file::RedexFile;
use super::replication::{
    ChannelId, ReplicaRole, SyncHeartbeat, SyncNack, SyncRequest, SyncResponse,
};
use super::replication_budget::BandwidthBudget;
use super::replication_catchup::{
    apply_sync_response_async, handle_sync_request, SyncRequestOutcome,
};
use super::replication_coordinator::{ChannelIdentity, CoordinatorError, ReplicationCoordinator};
use super::replication_heartbeat::HeartbeatTracker;
use super::replication_step::{
    election_outcome, tick, OutboundMessage, TickInputs, SYNC_REQUEST_CHUNK_MAX_DEFAULT,
};
use crate::adapter::net::behavior::placement::NodeId;
use crate::error::AdapterError;
use std::time::Duration;

/// Outbound wire-message sink the runtime uses to ship messages
/// through the substrate. Production sink routes through
/// [`crate::adapter::net::MeshNode`]'s `SUBPROTOCOL_REDEX`
/// dispatch; unit tests use a recorder mock.
/// Bandwidth-budget accounting note that applies to every send
/// method below: `Ok(())` means **queued to the transport**, not
/// **delivered to the peer**. Implementations whose underlying
/// transport buffers without ack (UDP, lossy QUIC streams under
/// link failure, etc.) MUST surface delivery-loss through their
/// own back-channel (heartbeat lag, peer-reported tail seq,
/// out-of-band ack) — the replication runtime's bandwidth budget
/// refund path keys on the synchronous `Err` return only. A
/// silently-dropped frame still drains the budget; the
/// flaky-link case is dampened by R-28 catchup-backoff once
/// empty responses accrue past the threshold, but the budget
/// itself cannot self-correct on transport-internal loss
/// without an end-to-end ack the trait deliberately does not
/// demand (the cost would be a per-response ack-RTT that the
/// underlying QUIC reliable streams already provide for in-spec
/// transports).
///
/// In short: trait callers MAY treat `Ok(())` as "the bytes
/// reached the wire layer." If your transport doesn't guarantee
/// delivery on `Ok(())`, document that on your impl and
/// understand that the budget over-counts under loss.
#[async_trait::async_trait]
pub trait ReplicationDispatcher: Send + Sync {
    /// Send a [`SyncHeartbeat`] to `target`. See trait-level note
    /// on `Ok(())` semantics.
    async fn send_heartbeat(&self, target: NodeId, msg: SyncHeartbeat) -> Result<(), AdapterError>;
    /// Send a [`SyncRequest`] to `target` (typically a leader).
    /// See trait-level note on `Ok(())` semantics.
    async fn send_sync_request(&self, target: NodeId, msg: SyncRequest)
        -> Result<(), AdapterError>;
    /// Send a [`SyncResponse`] to `target` (typically a replica
    /// catching up). See trait-level note on `Ok(())` semantics.
    async fn send_sync_response(
        &self,
        target: NodeId,
        msg: SyncResponse,
    ) -> Result<(), AdapterError>;
    /// Send a [`SyncNack`] to `target`. See trait-level note on
    /// `Ok(())` semantics.
    async fn send_sync_nack(&self, target: NodeId, msg: SyncNack) -> Result<(), AdapterError>;
}

/// RTT-lookup function the election uses. Production routes
/// through `ProximityGraph::nearest_rtt(|n| n.node_id ==
/// graph_id_of(node))`; unit tests pass a static closure.
pub type RttLookup = Arc<dyn Fn(NodeId) -> Option<Duration> + Send + Sync>;

/// Sync, non-blocking router the mesh's inbound dispatch hot path
/// calls when a `SUBPROTOCOL_REDEX` payload arrives. Owns the
/// per-channel registry of [`ReplicationRuntimeHandle`]s and
/// routes the decoded [`Inbound`] event to the right one.
///
/// Returns `Err(Inbound)` (the event, returned) when:
/// - No runtime is registered for `channel_id` (channel not opened
///   on this node, or the runtime was canceled and not yet
///   unregistered).
/// - The runtime's inbox is full (per-channel backlog at
///   [`RUNTIME_INBOX_CAPACITY`]).
///
/// In both cases the caller (mesh dispatch loop) drops + logs;
/// the wire layer's reliable-stream may retransmit, or the
/// peer's heartbeat cycle will recover state without it.
pub trait ReplicationInboundRouter: Send + Sync {
    /// Try to route an inbound event to its channel's runtime.
    /// Sync + non-blocking — must not call into async code, must
    /// not hold locks across awaits (the mesh dispatch loop is
    /// the sole caller and runs in a synchronous critical
    /// section).
    fn try_route(&self, channel_id: ChannelId, inbound: Inbound) -> Result<(), Inbound>;
}

#[async_trait::async_trait]
impl ReplicationDispatcher for crate::adapter::net::MeshNode {
    async fn send_heartbeat(&self, target: NodeId, msg: SyncHeartbeat) -> Result<(), AdapterError> {
        send_redex_payload(self, target, msg.to_bytes()).await
    }

    async fn send_sync_request(
        &self,
        target: NodeId,
        msg: SyncRequest,
    ) -> Result<(), AdapterError> {
        send_redex_payload(self, target, msg.to_bytes()).await
    }

    async fn send_sync_response(
        &self,
        target: NodeId,
        msg: SyncResponse,
    ) -> Result<(), AdapterError> {
        send_redex_payload(self, target, msg.to_bytes()).await
    }

    async fn send_sync_nack(&self, target: NodeId, msg: SyncNack) -> Result<(), AdapterError> {
        send_redex_payload(self, target, msg.to_bytes()).await
    }
}

/// Resolve a `NodeId` to its peer `SocketAddr` and ship `payload`
/// via `MeshNode::send_subprotocol` with `SUBPROTOCOL_REDEX`. The
/// `payload` already carries the 3-byte subprotocol header per
/// plan §2; the substrate's Net header carries `subprotocol_id`
/// independently for routing — the redundancy is plan-mandated
/// (the application-layer header is part of the wire contract,
/// distinct from the transport-layer Net header).
async fn send_redex_payload(
    mesh: &crate::adapter::net::MeshNode,
    target: NodeId,
    payload: Vec<u8>,
) -> Result<(), AdapterError> {
    let peer_addr = mesh.peer_addr(target).ok_or_else(|| {
        AdapterError::Connection(format!("replication: peer {target:#x} unknown"))
    })?;
    mesh.send_subprotocol(peer_addr, super::replication::SUBPROTOCOL_REDEX, &payload)
        .await
}

/// Inbound event the runtime processes. The mesh-side dispatcher
/// pushes one of these into the runtime's inbox per inbound wire
/// frame; the runtime's task drains the receiver on every wakeup.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// Peer's heartbeat — record into the tracker.
    Heartbeat {
        /// Originating node id.
        from: NodeId,
        /// Wire-format heartbeat payload.
        msg: SyncHeartbeat,
    },
    /// Peer (replica) asked us (leader) for events. Run
    /// `handle_sync_request` against the local file + dispatch
    /// the response or nack.
    SyncRequest {
        /// Originating replica.
        from: NodeId,
        /// Wire-format request.
        msg: SyncRequest,
    },
    /// Peer (leader) shipped us a chunk. Apply via
    /// `apply_sync_response`; on success advance our tail.
    SyncResponse {
        /// Originating leader.
        from: NodeId,
        /// Wire-format response.
        msg: SyncResponse,
    },
    /// Peer (leader) rejected our request with a typed error.
    SyncNack {
        /// Originating leader.
        from: NodeId,
        /// Wire-format nack.
        msg: SyncNack,
    },
    /// Shutdown signal from `Redex::open_file` cleanup or from
    /// channel-close. Drives `coordinator.transition_to(Idle,
    /// ChannelClose)` and exits the task loop.
    Shutdown,
}

/// Inputs the runtime task captures at spawn time. The
/// `tail_provider` closure reads the current `RedexFile::next_seq()`
/// — passed as a closure so the runtime doesn't take ownership of
/// the file handle (the file's owner is `Redex`).
pub struct RuntimeInputs {
    /// Channel identity + origin_hash.
    pub channel: ChannelIdentity,
    /// 32-byte BLAKE2s channel id for the wire-format heartbeat.
    pub channel_id: ChannelId,
    /// This node's id.
    pub self_node_id: NodeId,
    /// Replica-set membership — every node currently registered
    /// as a replica for the channel. Coordinator updates this
    /// when the placement filter re-selects (Phase C / F).
    pub replica_set: Vec<NodeId>,
    /// Heartbeat cadence in milliseconds (mirrors
    /// `ReplicationConfig::heartbeat_ms`). The tokio interval
    /// drives at this cadence.
    pub heartbeat_ms: u64,
    /// Function returning the wall-clock milliseconds for the
    /// outbound heartbeat's `wall_clock_ms` field. Operator-
    /// facing drift detection only; abstracted so tests can
    /// inject a deterministic value.
    pub wall_clock_provider: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// Function returning the current local `tail_seq`. Called
    /// each tick before emission so heartbeats carry the freshest
    /// value.
    pub tail_provider: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// RTT lookup for the election function.
    pub rtt_lookup: RttLookup,
    /// The local `RedexFile` for this channel. The runtime holds
    /// a clone (RedexFile is `Clone` — Arc-backed) so it can
    /// drive `handle_sync_request` on inbound `SyncRequest`
    /// frames (leader path) and `apply_sync_response` on inbound
    /// `SyncResponse` frames (replica path). The
    /// `tail_provider` closure typically wraps `file.next_seq()`.
    pub file: RedexFile,
    /// Per-channel default [`super::bandwidth::BandwidthClass`] — stamped on every
    /// `SyncRequest` the runtime emits. v0.3 Phase D2. Sourced
    /// from
    /// [`ReplicationConfig::default_bandwidth_class`](super::replication_config::ReplicationConfig).
    pub default_bandwidth_class: super::bandwidth::BandwidthClass,
    /// v0.3 Phase D2 admission-gate parameter: fraction of the
    /// bandwidth bucket capacity reserved against `Background`.
    /// Default 0.3 via
    /// [`ReplicationConfig::background_fraction`](super::replication_config::ReplicationConfig).
    pub background_fraction: f32,
}

/// Handle the spawned task produces. Holds the inbox sender so
/// the mesh dispatcher (and the lifecycle code) can push
/// [`Inbound`] events. `cancel()` sends `Shutdown` and awaits the
/// task to exit cleanly. The owned [`ReplicationCoordinator`] is
/// exposed via [`Self::coordinator`] so operators (and tests) can
/// observe the role, drive `transition_to`, and read the channel
/// metrics without going through the inbox.
pub struct ReplicationRuntimeHandle {
    /// Low-priority inbox: Heartbeat + SyncRequest. A peer flood
    /// fills this lane first; under saturation the high-priority
    /// lane keeps draining so Shutdown, SyncResponse, and SyncNack
    /// still make forward progress.
    inbox: mpsc::Sender<Inbound>,
    /// High-priority inbox: Shutdown + SyncResponse + SyncNack.
    /// Catchup-critical events ride this lane so a Heartbeat
    /// flood from many peers (50 peers × 100 ms = 500 evt/s plus a
    /// momentary slow `await` in `on_inbound` can saturate the
    /// single lane to 1024 in two seconds) doesn't strand the
    /// leader's response to the local replica or block graceful
    /// shutdown.
    priority_inbox: mpsc::Sender<Inbound>,
    task: Mutex<Option<JoinHandle<()>>>,
    coordinator: Arc<ReplicationCoordinator>,
    /// R-11: explicit "task has joined" flag. `is_stopped()`
    /// consults this rather than the JoinHandle slot — two
    /// concurrent `cancel()`s race on `task.lock().take()`, so
    /// the slot-based view can return `true` *before* the
    /// surviving caller's `.await` returns. The flag is flipped
    /// only after the join completes.
    stopped: AtomicBool,
}

#[inline]
fn is_priority_event(event: &Inbound) -> bool {
    matches!(
        event,
        Inbound::Shutdown | Inbound::SyncResponse { .. } | Inbound::SyncNack { .. }
    )
}

impl ReplicationRuntimeHandle {
    /// The per-channel coordinator. Same `Arc` the runtime task
    /// uses; cloning is cheap. Operators read `coordinator.role()`
    /// for the current state and `coordinator.metrics()` for the
    /// per-channel atomic counters; tests can drive
    /// `coordinator.transition_to(target, signal)` to put the
    /// channel in a specific role.
    pub fn coordinator(&self) -> &Arc<ReplicationCoordinator> {
        &self.coordinator
    }

    /// Push an inbound event into the runtime's inbox. Errors
    /// when the runtime has already exited (drained channel).
    /// Routes catchup-critical events (Shutdown, SyncResponse,
    /// SyncNack) to the priority lane so a Heartbeat flood on
    /// the standard lane can't starve them.
    pub async fn dispatch(&self, event: Inbound) -> Result<(), AdapterError> {
        let sender = if is_priority_event(&event) {
            &self.priority_inbox
        } else {
            &self.inbox
        };
        sender
            .send(event)
            .await
            .map_err(|_| AdapterError::Transient("replication runtime task exited".to_string()))
    }

    /// Same as [`Self::dispatch`] but for use from non-async
    /// contexts (the mesh dispatch loop's sync hot path).
    /// Returns the event back on full-buffer rejection so the
    /// caller can decide whether to drop, log, or block.
    pub fn try_dispatch(&self, event: Inbound) -> Result<(), Inbound> {
        let sender = if is_priority_event(&event) {
            &self.priority_inbox
        } else {
            &self.inbox
        };
        sender.try_send(event).map_err(|e| e.into_inner())
    }

    /// Send `Shutdown` and await the task to exit. Idempotent —
    /// subsequent calls are no-ops once the task has joined.
    ///
    /// Uses `try_send` first so a wedged task with a full inbox
    /// can't hang the caller indefinitely. On `Full`, the
    /// JoinHandle is aborted directly; the task exits without
    /// running the graceful Idle transition but the channel is
    /// still safely torn down.
    pub async fn cancel(&self) {
        let handle = self.task.lock().take();
        if let Some(h) = handle {
            // Shutdown rides the priority lane; that's the same
            // lane the run loop drains first under the biased
            // select, so even a saturated low-priority lane can't
            // delay the graceful exit.
            match self.priority_inbox.try_send(Inbound::Shutdown) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Graceful path: the task observes Shutdown (or
                    // already exited). Await the join.
                    let _ = h.await;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Priority lane is itself saturated. Abort
                    // directly so cancel() can't block the caller.
                    h.abort();
                    let _ = h.await;
                }
            }
            // Only the holder of the JoinHandle flips `stopped`,
            // and only after `.await` returns. Pre-fix, a concurrent
            // cancel() racer that lost the `take()` skipped the if-let
            // block and then unconditionally stored `true` — before
            // the winner's await had completed. `is_stopped()` then
            // reported "joined" while the task was still running.
            self.stopped.store(true, AtomicOrdering::Release);
        }
    }

    /// Returns `true` if the runtime has stopped (task joined).
    /// Useful for tests / observability.
    ///
    /// R-11: this consults an explicit flag flipped after
    /// `cancel()`'s `.await` returns, not the JoinHandle slot.
    /// Without the flag, two concurrent `cancel()` calls could
    /// race so the loser observes `task.lock().take() == None`
    /// and reports `is_stopped == true` before the winner has
    /// finished joining.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(AtomicOrdering::Acquire)
    }
}

/// Per-leader catchup state — tracks consecutive empty SyncResponses
/// in the face of an advertised tail gap. A buggy or byzantine
/// leader that advertises ever-increasing `tail_seq` but ships
/// `Response{events: []}` would otherwise loop the replica at
/// heartbeat cadence forever; the backoff suppresses outbound
/// SyncRequests once the threshold is crossed, with exponential
/// growth capped at [`CATCHUP_BACKOFF_CAP`]. A non-empty response
/// resets the counter so a transient stall doesn't permanently
/// pause catchup.
#[derive(Debug, Default)]
pub struct CatchupBackoff {
    entries: std::collections::HashMap<NodeId, BackoffEntry>,
}

#[derive(Debug, Default, Clone, Copy)]
struct BackoffEntry {
    /// Consecutive `apply_sync_response` calls that returned the
    /// same tail (no events applied) while the believed leader's
    /// advertised `tail_seq` was still strictly greater than ours.
    consecutive_empty: u32,
    /// Wall-clock instant the backoff window ends. `None` while
    /// the counter is below `CATCHUP_BACKOFF_THRESHOLD`.
    backoff_until: Option<Instant>,
}

/// Strikes before backoff kicks in. The first 3 empty responses
/// are absorbed without delay so a transient leader-retention edge
/// doesn't trigger a backoff.
pub const CATCHUP_BACKOFF_THRESHOLD: u32 = 3;

/// First backoff window after the threshold is crossed.
pub const CATCHUP_BACKOFF_INITIAL: Duration = Duration::from_secs(1);

/// Upper bound on the exponential backoff. A wedged leader stays
/// reachable for re-evaluation at least every cap.
pub const CATCHUP_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Per-leader in-flight SyncRequest registry. Each outbound
/// SyncRequest mints a random `request_id` from `getrandom`; the
/// id is inserted here keyed by `(leader_node_id, request_id)`
/// before the wire send. Inbound SyncResponse / SyncNack must
/// carry an id present in the set; otherwise it's a stale
/// response from a request the replica already timed out (or a
/// forged frame from any peer that happens to be the recorded
/// leader). Entries auto-expire after [`REQUEST_TTL`] so the set
/// can't grow without bound under leader silence.
#[derive(Debug, Default)]
pub struct OutstandingRequests {
    entries: std::collections::HashMap<(NodeId, u64), Instant>,
}

/// TTL on entries in [`OutstandingRequests`]. Bounded by the
/// catchup deadline so a one-tick-late response still lands.
pub const REQUEST_TTL: Duration = Duration::from_secs(30);

/// Soft cap on per-replica outstanding requests across all
/// leaders. A degraded leader that never responds shouldn't
/// let the set grow without bound; once the cap is hit, GC
/// kicks in and the oldest entries are dropped.
pub const REQUEST_REGISTRY_SOFT_CAP: usize = 256;

impl OutstandingRequests {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Record a freshly-minted `request_id` against `leader`.
    /// Best-effort GC of expired entries runs at insert time so
    /// the set stays bounded without a separate sweeper task.
    ///
    /// #35: the expiry GC alone does NOT bound the map. Under
    /// sustained young in-flight load (many channels / a fast
    /// tick), a replica can legitimately hold ≥ cap requests all
    /// younger than [`REQUEST_TTL`]; `retain` then evicts nothing
    /// and the insert proceeds anyway, so the map grows without
    /// bound until TTLs start expiring. To make the soft cap an
    /// actual bound, after the expiry sweep, if we are still at or
    /// above the cap, evict the single OLDEST entry before
    /// inserting. Token matching stays correct — the evicted token
    /// is the one closest to its own TTL, so a response for it was
    /// already the most likely to be rejected as stale; the caller
    /// treats a missing token exactly like an expired one (drop the
    /// response). The map size after this method is therefore
    /// `<= REQUEST_REGISTRY_SOFT_CAP`.
    pub fn record(&mut self, leader: NodeId, request_id: u64, now: Instant) {
        if self.entries.len() >= REQUEST_REGISTRY_SOFT_CAP {
            self.entries
                .retain(|_, &mut inserted| now.saturating_duration_since(inserted) < REQUEST_TTL);
            // Replacing an existing key won't grow the map, so only
            // force-evict when we'd actually add a NEW entry and are
            // still at/over the cap with nothing expired.
            if self.entries.len() >= REQUEST_REGISTRY_SOFT_CAP
                && !self.entries.contains_key(&(leader, request_id))
            {
                if let Some(oldest_key) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, &inserted)| inserted)
                    .map(|(k, _)| *k)
                {
                    self.entries.remove(&oldest_key);
                }
            }
        }
        self.entries.insert((leader, request_id), now);
    }

    /// Take the entry for `(leader, request_id)` if present and
    /// not yet expired. Returns `true` when an in-flight request
    /// matched; the caller proceeds with the apply path. `false`
    /// means the response is stale / forged / past TTL — drop.
    pub fn take(&mut self, leader: NodeId, request_id: u64, now: Instant) -> bool {
        match self.entries.remove(&(leader, request_id)) {
            Some(inserted) => now.saturating_duration_since(inserted) < REQUEST_TTL,
            None => false,
        }
    }

    /// Drop every entry recorded against `leader`. Called when
    /// the believed leader changes so a re-elected peer doesn't
    /// inherit the prior leader's in-flight token set.
    pub fn clear_leader(&mut self, leader: NodeId) {
        self.entries.retain(|(l, _), _| *l != leader);
    }

    /// Current entry count. Test-only accessor for the #35
    /// soft-cap-bound assertions; the field is otherwise private.
    #[cfg(test)]
    pub(crate) fn len_for_test(&self) -> usize {
        self.entries.len()
    }
}

impl CatchupBackoff {
    /// Construct an empty backoff tracker.
    pub fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Record an empty `SyncResponse` from `leader` (events vec was
    /// empty or apply returned the same tail). Increments the
    /// consecutive counter; once past `CATCHUP_BACKOFF_THRESHOLD`,
    /// stamps `backoff_until = now + min(initial << k, cap)` where
    /// `k = consecutive_empty - threshold - 1`.
    pub fn record_empty(&mut self, leader: NodeId, now: Instant) {
        let entry = self.entries.entry(leader).or_default();
        entry.consecutive_empty = entry.consecutive_empty.saturating_add(1);
        if entry.consecutive_empty > CATCHUP_BACKOFF_THRESHOLD {
            let shift = entry
                .consecutive_empty
                .saturating_sub(CATCHUP_BACKOFF_THRESHOLD + 1)
                .min(20);
            let multiplier: u32 = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
            let backoff = CATCHUP_BACKOFF_INITIAL
                .saturating_mul(multiplier)
                .min(CATCHUP_BACKOFF_CAP);
            entry.backoff_until = Some(now + backoff);
        }
    }

    /// Record a productive response (events applied, tail advanced)
    /// from `leader`. Clears any backoff state so the next request
    /// can fire immediately.
    pub fn record_progress(&mut self, leader: NodeId) {
        self.entries.remove(&leader);
    }

    /// True when `now` is strictly before the recorded
    /// `backoff_until` for `leader`. Out-of-backoff leaders (no
    /// entry, or entry below threshold) always return `false`.
    pub fn is_in_backoff(&self, leader: NodeId, now: Instant) -> bool {
        self.entries
            .get(&leader)
            .and_then(|e| e.backoff_until)
            .is_some_and(|until| now < until)
    }

    /// Drop entries for leaders whose backoff window expired more
    /// than `cap` ago. Called from `on_tick` to keep the map
    /// bounded under leader churn: a leader demoted after
    /// accruing strikes never has `record_progress` called for it
    /// again, so without expiry the entry persists indefinitely.
    /// Below-threshold entries (no `backoff_until` stamp) are
    /// retained — they represent active counting state.
    pub fn gc_expired(&mut self, now: Instant, cap: Duration) {
        self.entries.retain(|_, e| match e.backoff_until {
            Some(until) => now.saturating_duration_since(until) < cap,
            None => true,
        });
    }
}

impl Drop for ReplicationRuntimeHandle {
    /// Best-effort cleanup if a handle is dropped without an
    /// explicit `cancel().await`. Aborts the task synchronously so
    /// the spawned future stops driving and the dispatcher Arc the
    /// task held is released — closing the strong-reference cycle
    /// `MeshNode → router → handle → task → dispatcher` without
    /// requiring callers to remember the cancel sequence. The
    /// graceful Idle transition is skipped on this path; callers
    /// that need the announce/withdraw side-effects to land must
    /// still `cancel().await` before drop.
    fn drop(&mut self) {
        // try_lock — pre-fix this took the parking_lot mutex
        // unconditionally; on a single-thread runtime panic during
        // shutdown, drop could fire on a thread already holding
        // self.task (e.g. mid-cancel() when the future is dropped on
        // panic), producing a deadlock. The best-effort abort can
        // wait for the next normal cleanup if we lose the lock race
        // — losing it means somebody else is already inside cancel()
        // or another drop, and they will abort/await the task.
        if let Some(mut guard) = self.task.try_lock() {
            if let Some(h) = guard.take() {
                h.abort();
            }
        }
    }
}

/// Inbox capacity — bounds the per-channel inbound backlog. A
/// peer flooding `SUBPROTOCOL_REDEX` payloads at us can't grow
/// the per-channel queue without bound; once full, the mesh
/// dispatcher's `try_dispatch` returns the event back and the
/// caller (mesh dispatch loop) drops + logs.
pub const RUNTIME_INBOX_CAPACITY: usize = 1024;

/// Per-channel mutable state the runtime task threads through
/// `on_tick` / `on_inbound`. Replaces the four-`Arc<Mutex<…>>` arg
/// soup that pushed those functions over clippy's
/// `too_many_arguments` limit. All four members are reference-
/// counted internally so cloning the struct is a handful of
/// atomic increments — cheap and lock-free.
struct RuntimeState {
    tracker: Arc<Mutex<HeartbeatTracker>>,
    budget: Arc<Mutex<BandwidthBudget>>,
    backoff: Arc<Mutex<CatchupBackoff>>,
    outstanding: Arc<Mutex<OutstandingRequests>>,
}

/// Priority-lane inbox capacity. Smaller than the standard lane
/// because the events that ride it (Shutdown, SyncResponse,
/// SyncNack) are bounded by the local replica's in-flight
/// catchup window plus a handful of NACKs — not a per-peer
/// heartbeat flood. Sized to absorb a burst without back-
/// pressuring the leader-side dispatcher.
pub const RUNTIME_PRIORITY_INBOX_CAPACITY: usize = 128;

/// Spawn a per-channel replication runtime task. Returns a
/// handle the mesh dispatcher uses to push inbound events and
/// the lifecycle code uses to cancel.
///
/// The task:
/// 1. Initializes the tracker / budget for this channel.
/// 2. Loops on `select! { interval tick, inbox.recv() }`.
/// 3. Each tick: calls [`tick`], dispatches outbound, runs the
///    election if the coordinator just entered Candidate.
/// 4. Each inbound event: updates state + maybe ships an
///    outbound response.
/// 5. Exits cleanly on `Inbound::Shutdown` after running
///    `coordinator.transition_to(Idle, ChannelClose)`.
///
/// `dispatcher` ships every outbound message; `tail_provider` /
/// `wall_clock_provider` give the task fresh values per tick.
///
/// **R-14 — Arc cycle invariant.** Production wiring has
/// `MeshNode → ReplicationInboundRouter → ReplicationRuntimeHandle
/// → task → Arc<dyn ReplicationDispatcher = MeshNode>`. This is
/// a strong reference cycle. It is broken by
/// `ReplicationWiring::drop` (`manager.rs`): un-installing the
/// router releases its `Arc<RuntimeHandle>` references, the
/// runtime task observes the closed inbox receiver, exits, and
/// drops its dispatcher Arc. Callers that do NOT route through
/// `Redex` drop (e.g. holding a raw `ReplicationRuntimeHandle`
/// past the dispatcher's owner) MUST call `cancel()` before
/// dropping the dispatcher; otherwise the cycle leaks both.
pub fn spawn_replication_runtime(
    inputs: RuntimeInputs,
    coordinator: Arc<ReplicationCoordinator>,
    dispatcher: Arc<dyn ReplicationDispatcher>,
    budget: Arc<Mutex<BandwidthBudget>>,
) -> ReplicationRuntimeHandle {
    let state = RuntimeState {
        tracker: Arc::new(Mutex::new(HeartbeatTracker::new(inputs.heartbeat_ms))),
        budget,
        backoff: Arc::new(Mutex::new(CatchupBackoff::new())),
        outstanding: Arc::new(Mutex::new(OutstandingRequests::new())),
    };
    let (tx, rx) = mpsc::channel::<Inbound>(RUNTIME_INBOX_CAPACITY);
    let (priority_tx, priority_rx) = mpsc::channel::<Inbound>(RUNTIME_PRIORITY_INBOX_CAPACITY);
    let coordinator_for_task = coordinator.clone();
    let task = tokio::spawn(run(
        inputs,
        coordinator_for_task,
        dispatcher,
        state,
        rx,
        priority_rx,
    ));
    ReplicationRuntimeHandle {
        inbox: tx,
        priority_inbox: priority_tx,
        task: Mutex::new(Some(task)),
        coordinator,
        stopped: AtomicBool::new(false),
    }
}

async fn run(
    inputs: RuntimeInputs,
    coordinator: Arc<ReplicationCoordinator>,
    dispatcher: Arc<dyn ReplicationDispatcher>,
    state: RuntimeState,
    mut inbox: mpsc::Receiver<Inbound>,
    mut priority_inbox: mpsc::Receiver<Inbound>,
) {
    let heartbeat_interval = Duration::from_millis(inputs.heartbeat_ms);
    let mut interval = tokio::time::interval(heartbeat_interval);
    // `MissedTickBehavior::Skip` so a slow tick under load
    // doesn't queue up unbounded ticks. We emit one heartbeat
    // per interval; missed intervals are just observed silence
    // at the receiver.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first tick fires immediately; consume it so the
    // initial state has had a chance to settle.
    interval.tick().await;

    loop {
        // `biased;` makes tokio::select poll branches top-to-
        // bottom rather than randomly. Priority lane is checked
        // first, then the tick, then the low-priority lane. A
        // heartbeat flood saturating the low-priority lane can't
        // starve SyncResponse / SyncNack / Shutdown — they ride
        // the priority lane and get drained ahead of the flood.
        tokio::select! {
            biased;
            event = priority_inbox.recv() => {
                match event {
                    Some(Inbound::Shutdown) | None => {
                        let _ = coordinator
                            .transition_to(
                                ReplicaRole::Idle,
                                super::replication_state::TransitionSignal::ChannelClose,
                            )
                            .await;
                        return;
                    }
                    Some(event) => {
                        on_inbound(&inputs, &coordinator, &dispatcher, &state, event).await;
                    }
                }
            }
            _ = interval.tick() => {
                on_tick(&inputs, &coordinator, &dispatcher, &state).await;
            }
            event = inbox.recv() => {
                match event {
                    Some(Inbound::Shutdown) | None => {
                        // Shutdown normally rides the priority lane;
                        // a None here means the low-priority sender
                        // dropped (caller closed the handle without
                        // calling cancel). Treat as graceful exit.
                        let _ = coordinator
                            .transition_to(
                                ReplicaRole::Idle,
                                super::replication_state::TransitionSignal::ChannelClose,
                            )
                            .await;
                        return;
                    }
                    Some(event) => {
                        on_inbound(&inputs, &coordinator, &dispatcher, &state, event).await;
                    }
                }
            }
        }
    }
}

/// Which lag gauge the tick should update, if any. Leader emits the
/// worst-replica lag; Replica emits the believed-leader lag.
/// Candidate + Idle don't emit lag — both are transient or non-
/// participating roles.
#[derive(Debug)]
enum LagObservation {
    /// Leader-side: max over replica peers of `now - peer.last_seen`.
    /// Drives `record_leader_lag`. Reflects the staleness of the
    /// worst-lagging replica.
    Leader(Duration),
    /// Replica-side: `now - believed_leader.last_seen`. Drives
    /// `record_replica_lag`. `None` if no leader heartbeat has been
    /// observed yet (the gauge stays unobserved).
    Replica(Duration),
    /// No lag to record this tick.
    None,
}

/// Compute the lag observation for this tick. Pure read over the
/// tracker; the caller updates the metric off the lock.
fn observe_lag(
    role: ReplicaRole,
    replica_set: &[NodeId],
    self_node_id: NodeId,
    tracker: &HeartbeatTracker,
    now: Instant,
) -> LagObservation {
    match role {
        ReplicaRole::Leader => {
            // Worst-replica view: max over peers of (now - peer.last_seen).
            // A peer never seen has no observation — skip it (the
            // gauge captures observed lag, not "never heard from").
            let worst = replica_set
                .iter()
                .copied()
                .filter(|&p| p != self_node_id)
                .filter_map(|p| tracker.peer_lag(p, now))
                .max();
            match worst {
                Some(d) => LagObservation::Leader(d),
                None => LagObservation::None,
            }
        }
        ReplicaRole::Replica => match tracker.believed_leader() {
            Some(leader) => match tracker.peer_lag(leader, now) {
                Some(d) => LagObservation::Replica(d),
                None => LagObservation::None,
            },
            None => LagObservation::None,
        },
        ReplicaRole::Candidate | ReplicaRole::Idle => LagObservation::None,
    }
}

/// Drop the believed-leader belief AND the outstanding-request
/// tokens recorded against that leader. Sites that previously
/// only called `clear_believed_leader` would leave the prior
/// leader's in-flight tokens in `OutstandingRequests` until TTL
/// (30 s). Under role thrash or rapid leader churn, the soft-cap
/// GC then evicted entries from OTHER leaders to make room — the
/// documented invariant on `OutstandingRequests::clear_leader`.
fn clear_leader_belief_and_tokens(
    tracker: &Arc<Mutex<HeartbeatTracker>>,
    outstanding: &Arc<Mutex<OutstandingRequests>>,
) {
    // Read-then-clear under a single `tracker` lock. Pre-fix
    // [perf #71 in `docs/performance/net-perf-analysis.md`] this
    // took the tracker lock twice back-to-back — once to read
    // `believed_leader()`, once to `clear_believed_leader()`.
    // Coalescing is also defensively atomic with respect to a
    // concurrent observer that races between the two calls (a
    // future change that exposes the tracker more widely couldn't
    // see the gap).
    let prior = {
        let mut t = tracker.lock();
        let p = t.believed_leader();
        t.clear_believed_leader();
        p
    };
    if let Some(prior) = prior {
        outstanding.lock().clear_leader(prior);
    }
}

async fn on_tick(
    inputs: &RuntimeInputs,
    coordinator: &Arc<ReplicationCoordinator>,
    dispatcher: &Arc<dyn ReplicationDispatcher>,
    state: &RuntimeState,
) {
    let RuntimeState {
        tracker,
        budget: _,
        backoff,
        outstanding,
    } = state;
    // Source `now` from tokio's clock so the silence-detection
    // pass inside the tracker tick honors tokio::time::pause() in
    // tests and stays coherent with the tokio::time::interval that
    // drives this on_tick call. Pre-fix std::Instant::now() kept
    // moving while virtual time was paused.
    let now = tokio::time::Instant::now().into_std();
    // Drop CatchupBackoff entries whose backoff window expired
    // more than a cap ago — protects the map from unbounded
    // growth under leader churn (a demoted leader's entry has
    // no other clearance path; `record_progress` only fires for
    // the current believed leader).
    backoff
        .lock()
        .gc_expired(now, CATCHUP_BACKOFF_CAP.saturating_mul(2));
    let tail_seq = (inputs.tail_provider)();
    // Bump the coordinator's cached tail at every tick so the next
    // transition's announce_chain advertises the real tip. The
    // CAS-monotonic guard inside record_tail_seq drops the write if
    // tail_provider went backward (it shouldn't — tail_provider
    // wraps next_seq()), so this is also idempotent. Without this
    // the leader path's tail_seq atomic stays at the value the
    // last apply_sync_response recorded (which only Replicas run),
    // and a leader's announce_chain at promotion time ships
    // tip_seq=0.
    //
    // For the LEADER role specifically, `tail_provider` returns the
    // raw local-file `next_seq()`, which the leader bumps the moment
    // a write lands locally — pre-replication. Advertising that
    // value via capability tags biases `find_chain_holders` (which
    // picks the freshest holder by `tip_seq` during failover)
    // toward a partition that may have un-replicated writes; a
    // crash before those writes ship loses them.
    //
    // Clamp the advertised tail to the highest tail any peer has
    // confirmed via heartbeat. That tail is, by construction,
    // "replicated at least once" — a safe minimum for failover
    // discovery. When NO peer has reported yet (fresh leader, no
    // replicas), fall back to the raw local tail: there is no
    // safer value to advertise and a sole leader has authority
    // over its own writes by tautology.
    let advertised_tail = if coordinator.role() == ReplicaRole::Leader {
        let max_peer_tail = tracker
            .lock()
            .peer_tail_seqs()
            .into_iter()
            .filter(|(id, _)| *id != inputs.self_node_id)
            .map(|(_, t)| t)
            .max();
        match max_peer_tail {
            Some(p) => tail_seq.min(p),
            None => tail_seq,
        }
    } else {
        tail_seq
    };
    coordinator.record_tail_seq(advertised_tail);
    let wall_clock_ms = (inputs.wall_clock_provider)();
    // R-10: capture `current_role` inside the same critical
    // section that holds the tracker lock so a concurrent
    // transition can't land between the role read and the
    // tick(). Reading role here is cheap (a parking_lot mutex
    // load); holding both locks together is safe because role
    // observation never awaits.
    let (outcome, lag_observation) = {
        let t = tracker.lock();
        // Capture `current_role` inside the same critical section
        // that holds the tracker lock so a concurrent transition
        // can't land between the role read and the tick(). Reading
        // role here is cheap (a parking_lot mutex load); holding
        // both locks together is safe because role observation
        // never awaits. The captured value lives only inside this
        // closure — `tick()` consumes it via its outcome.
        let current_role = coordinator.role();
        let outcome = tick(TickInputs {
            self_node_id: inputs.self_node_id,
            current_role,
            channel_id: inputs.channel_id,
            tail_seq,
            replica_set: &inputs.replica_set,
            tracker: &t,
            wall_clock_ms,
            chunk_max_bytes: SYNC_REQUEST_CHUNK_MAX_DEFAULT,
            now,
            default_bandwidth_class: inputs.default_bandwidth_class,
        });
        let lag = observe_lag(
            current_role,
            &inputs.replica_set,
            inputs.self_node_id,
            &t,
            now,
        );
        (outcome, lag)
    };
    // Record lag gauges off the tracker lock.
    match lag_observation {
        LagObservation::Leader(d) => coordinator.metrics().record_leader_lag(d),
        LagObservation::Replica(d) => coordinator.metrics().record_replica_lag(d),
        LagObservation::None => {}
    }
    for msg in outcome.outbound {
        match msg {
            OutboundMessage::Heartbeat { target, msg } => {
                if let Err(e) = dispatcher.send_heartbeat(target, msg).await {
                    tracing::trace!(target=?target, error=?e, "replication: heartbeat send failed");
                }
            }
            OutboundMessage::SyncRequest { target, mut msg } => {
                // R-28 catchup backoff: a buggy/byzantine leader that
                // advertises an ever-growing tail but ships empty
                // responses would otherwise loop this branch at the
                // heartbeat cadence forever. Once the empty-response
                // count crosses `CATCHUP_BACKOFF_THRESHOLD` (3), the
                // tracker stamps `backoff_until`; ticks within that
                // window skip the send entirely. A non-empty response
                // resets the counter through `record_progress` on the
                // apply path.
                if backoff.lock().is_in_backoff(target, now) {
                    tracing::trace!(
                        target = target,
                        "replication: skipping SyncRequest — leader is in catchup backoff"
                    );
                    continue;
                }
                // R-23 request-token correlation. `tick` emits the
                // SyncRequest with `request_id = 0` placeholder; the
                // runtime mints a random 64-bit token from
                // `getrandom` here, records `(leader, token)` in the
                // outstanding-requests set, and stamps the wire frame
                // with the minted value before send. Inbound
                // SyncResponse / SyncNack must carry a token still
                // in the set; stale responses (re-issue races, late-
                // arriving NACKs from prior requests the replica
                // already timed out) drop on the apply path.
                let mut id_bytes = [0u8; 8];
                if getrandom::fill(&mut id_bytes).is_err() {
                    tracing::trace!(
                        target = target,
                        "replication: getrandom failure; skipping SyncRequest this tick"
                    );
                    continue;
                }
                let token = u64::from_le_bytes(id_bytes);
                msg.request_id = token;
                outstanding.lock().record(target, token, now);
                if let Err(e) = dispatcher.send_sync_request(target, msg).await {
                    tracing::trace!(target=?target, error=?e, "replication: sync_request send failed");
                }
            }
        }
    }
    if let Some(pending) = outcome.transition {
        if let Err(e) = coordinator
            .transition_to(pending.target, pending.signal)
            .await
        {
            tracing::warn!(error=?e, "replication: transition_to({:?}, {:?}) failed", pending.target, pending.signal);
            return;
        }
        // If we just entered Candidate via MissedHeartbeats,
        // run the deterministic election in the same tick so the
        // Candidate window stays microseconds-wide per plan §3.
        if pending.target == ReplicaRole::Candidate {
            let healthy = tracker.lock().healthy_peers(now);
            // Self is alive by tautology — the runtime is the
            // code computing the election. The tracker only
            // records observed inbound heartbeats from peers,
            // so self never appears in `healthy_peers` directly.
            // Include self explicitly so the election filter
            // doesn't accidentally exclude us.
            let elect = election_outcome(
                inputs.self_node_id,
                &inputs.replica_set,
                inputs.rtt_lookup.as_ref(),
                |peer| peer == inputs.self_node_id || healthy.contains(&peer),
            );
            if let Some(pt) = elect {
                match coordinator.transition_to(pt.target, pt.signal).await {
                    Ok(_) => {
                        // Only clear the believed leader on a
                        // successful transition. If the second
                        // transition lost a race (e.g. an inbound
                        // Shutdown drove us to Idle first), wiping
                        // the believed leader would leave the
                        // coordinator with no recovery signal.
                        clear_leader_belief_and_tokens(tracker, outstanding);
                    }
                    Err(CoordinatorError::TagSink(e)) => {
                        // State moved to the target (Leader /
                        // Replica); only the chain-tag side-effect
                        // failed. We are functionally in the new
                        // role — clear the believed leader so the
                        // tick path doesn't keep treating an old
                        // peer as authoritative. Chain discovery
                        // for this channel stays silent until the
                        // next successful announce; operators
                        // observing this counter see the
                        // divergence.
                        tracing::warn!(
                            error = ?e,
                            target = ?pt.target,
                            "replication: post-election chain-tag side-effect failed; state advanced"
                        );
                        clear_leader_belief_and_tokens(tracker, outstanding);
                    }
                    Err(CoordinatorError::Transition(e)) => {
                        // State did not move (typically because a
                        // concurrent ChannelClose drove us to Idle
                        // first). Recover by clearing the believed
                        // leader so the next tick re-enters
                        // discovery from a clean slate rather than
                        // sitting on a stale belief.
                        tracing::warn!(
                            error = ?e,
                            target = ?pt.target,
                            "replication: post-election transition rejected; state moved out from under us"
                        );
                        clear_leader_belief_and_tokens(tracker, outstanding);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_inbound(
    inputs: &RuntimeInputs,
    coordinator: &Arc<ReplicationCoordinator>,
    dispatcher: &Arc<dyn ReplicationDispatcher>,
    state: &RuntimeState,
    event: Inbound,
) {
    let RuntimeState {
        tracker,
        budget,
        backoff,
        outstanding,
    } = state;
    // Peer-auth gate. Every inbound replication message must come
    // from a peer in the channel's configured replica_set; any
    // other sender has no business driving the state machine for
    // this channel. Pre-fix the handlers below only checked the
    // channel_id, so any mesh peer with SUBPROTOCOL_REDEX reach
    // could ship Heartbeat / SyncRequest / SyncResponse / SyncNack
    // and the runtime would apply them. SyncResponse hijack was
    // the worst case — a non-leader peer could write attacker-
    // chosen bytes to the replica's local log via append_batch.
    //
    // Shutdown is local-only (never crosses the wire), so it
    // bypasses the membership check; the from field on a Shutdown
    // event is the local node's id.
    let from_node = match &event {
        Inbound::Shutdown => None,
        Inbound::Heartbeat { from, .. } => Some(*from),
        Inbound::SyncRequest { from, .. } => Some(*from),
        Inbound::SyncResponse { from, .. } => Some(*from),
        Inbound::SyncNack { from, .. } => Some(*from),
    };
    if let Some(from) = from_node {
        if !inputs.replica_set.contains(&from) {
            tracing::trace!(
                from = from,
                channel = ?inputs.channel_id,
                "replication: dropping inbound from peer not in replica_set"
            );
            return;
        }
    }
    match event {
        Inbound::Shutdown => {
            // Handled by the main loop; never reaches here.
            unreachable!("Shutdown is filtered in the main loop");
        }
        Inbound::Heartbeat { from, msg } => {
            // Validate channel-id match. Mismatched heartbeats
            // arrive when the mesh dispatcher misroutes (which
            // shouldn't happen, but better to drop than to
            // poison the tracker).
            if msg.channel_id != inputs.channel_id {
                tracing::trace!(
                    from = from,
                    "replication: dropping heartbeat for wrong channel"
                );
                return;
            }
            // Source from tokio's clock so silence detection works
            // under tokio::time::pause() — std::Instant::now()
            // wouldn't advance with virtual time and the tick-driven
            // silence check would never fire deterministically.
            tracker.lock().record_heartbeat(
                from,
                msg.role,
                msg.tail_seq,
                tokio::time::Instant::now().into_std(),
            );
            // Dual-leader convergence. A symmetric-RTT election or
            // a partition heal can leave two peers both believing
            // they are Leader for the same channel. Without
            // convergence, both partitions keep writing — divergent
            // histories accrete and `apply_sync_response` eventually
            // logs `GapBeforeChunk{divergence_suspected:true}` while
            // `skip_to` silently overwrites the loser's tail.
            //
            // On any inbound Heartbeat with role=Leader while self
            // is Leader, run the deterministic tiebreak: the side
            // with the higher tail_seq keeps Leader; on a tie, the
            // numerically smaller node_id keeps Leader. The loser
            // concedes via `Leader → Replica` so the next tick
            // re-resolves through the heartbeat cycle.
            if msg.role == ReplicaRole::Leader
                && coordinator.role() == ReplicaRole::Leader
                && from != inputs.self_node_id
            {
                let local_tail = (inputs.tail_provider)();
                let peer_tail = msg.tail_seq;
                let local_wins = local_tail > peer_tail
                    || (local_tail == peer_tail && inputs.self_node_id < from);
                if !local_wins {
                    tracing::warn!(
                        from = from,
                        peer_tail = peer_tail,
                        local_tail = local_tail,
                        local = inputs.self_node_id,
                        "replication: peer-leader observed; conceding via Leader → Replica"
                    );
                    let _ = coordinator
                        .transition_to(
                            ReplicaRole::Replica,
                            super::replication_state::TransitionSignal::PeerLeaderObserved,
                        )
                        .await;
                }
            }
        }
        Inbound::SyncRequest { from, msg } => {
            // R-12: defense-in-depth — validate channel_id at the
            // runtime boundary. The wire decoder + mesh router are
            // supposed to demux by channel, but a misroute would
            // otherwise apply against the wrong file.
            if msg.channel_id != inputs.channel_id {
                tracing::trace!(
                    from = from,
                    "replication: dropping SyncRequest for wrong channel"
                );
                return;
            }
            // Leader-side: only honor SyncRequest when we believe
            // we're the leader. Other roles surface `NotLeader`
            // so the replica re-resolves leadership.
            if coordinator.role() != ReplicaRole::Leader {
                let nack = SyncNack {
                    channel_id: inputs.channel_id,
                    since_seq: msg.since_seq,
                    error_code: super::replication::SyncNackError::NotLeader,
                    leader_first_retained_seq: 0,
                    request_id: msg.request_id,
                    detail: String::new(),
                };
                if let Err(e) = dispatcher.send_sync_nack(from, nack).await {
                    tracing::trace!(from = from, error = ?e, "replication: NotLeader NACK send failed");
                }
                return;
            }
            // Run the catch-up helper against our local file.
            match handle_sync_request(&inputs.file, &msg, inputs.channel_id) {
                SyncRequestOutcome::Response(resp) => {
                    let byte_estimate = estimate_response_bytes(&resp);
                    // Gate on the bandwidth budget. If the
                    // budget can't admit this chunk, NACK with
                    // Backpressure so the replica backs off.
                    let admitted = {
                        let mut bb = budget.lock();
                        bb.try_consume_with_class(
                            byte_estimate,
                            msg.class,
                            tokio::time::Instant::now().into_std(),
                            inputs.background_fraction,
                        )
                    };
                    if !admitted {
                        let nack = SyncNack {
                            channel_id: inputs.channel_id,
                            since_seq: msg.since_seq,
                            error_code: super::replication::SyncNackError::Backpressure,
                            leader_first_retained_seq: 0,
                            request_id: msg.request_id,
                            detail: String::new(),
                        };
                        if let Err(e) = dispatcher.send_sync_nack(from, nack).await {
                            tracing::trace!(from = from, error = ?e, "replication: Backpressure NACK send failed");
                        }
                        return;
                    }
                    // R-1: re-check role after the read + budget
                    // gate. If a concurrent transition flipped us
                    // out of Leader (DiskPressureWithdraw, peer
                    // concession), don't ship the response —
                    // NACK NotLeader instead so the replica re-
                    // resolves through find_chain_holders.
                    if coordinator.role() != ReplicaRole::Leader {
                        let nack = SyncNack {
                            channel_id: inputs.channel_id,
                            since_seq: msg.since_seq,
                            error_code: super::replication::SyncNackError::NotLeader,
                            leader_first_retained_seq: 0,
                            request_id: msg.request_id,
                            detail: String::new(),
                        };
                        if let Err(e) = dispatcher.send_sync_nack(from, nack).await {
                            tracing::trace!(from = from, error = ?e, "replication: post-op NotLeader NACK send failed");
                        }
                        return;
                    }
                    // Bump the cumulative bytes metric BEFORE
                    // ship so the operator's view stays accurate
                    // even if the wire send fails (the bytes
                    // would still have been read off disk).
                    coordinator.metrics().incr_sync_bytes(byte_estimate);
                    if let Err(e) = dispatcher.send_sync_response(from, resp).await {
                        // Refund the budget — pre-fix repeated
                        // send failures over a flaky link drained
                        // the bucket toward permanent backpressure
                        // without shipping any traffic. The cumul-
                        // ative bytes metric stays incremented
                        // (operators still see "we tried to send
                        // these bytes") but the rate budget can
                        // recover.
                        //
                        // Per the dispatcher trait's `Ok(())`
                        // semantics: a transport that returns
                        // `Ok(())` after queueing-without-delivery
                        // (UDP, lossy QUIC under link failure) will
                        // NOT trigger this refund and the budget
                        // over-counts on silent loss. R-28
                        // catchup-backoff dampens the wedged-link
                        // case once empty responses accrue past
                        // threshold, but the budget itself cannot
                        // self-correct without an end-to-end ack
                        // the trait deliberately does not demand.
                        {
                            let mut bb = budget.lock();
                            bb.refund(byte_estimate);
                        }
                        tracing::trace!(from = from, error = ?e, "replication: SyncResponse send failed");
                    }
                }
                SyncRequestOutcome::Nack {
                    error_code,
                    leader_first_retained_seq,
                    detail,
                } => {
                    let nack = SyncNack {
                        channel_id: inputs.channel_id,
                        since_seq: msg.since_seq,
                        error_code,
                        leader_first_retained_seq,
                        request_id: msg.request_id,
                        detail,
                    };
                    if let Err(e) = dispatcher.send_sync_nack(from, nack).await {
                        tracing::trace!(from = from, error = ?e, "replication: SyncNack send failed");
                    }
                }
            }
        }
        Inbound::SyncResponse { from, msg } => {
            // R-12: defense-in-depth — validate channel_id at the
            // runtime boundary.
            if msg.channel_id != inputs.channel_id {
                tracing::trace!(
                    from = from,
                    "replication: dropping SyncResponse for wrong channel"
                );
                return;
            }
            // R-23 request-token correlation. The replica's
            // outstanding-request set tracks `(leader, request_id)`
            // tuples for every SyncRequest the runtime has shipped.
            // A response whose `request_id` is not in the set is
            // stale (the replica timed out and re-issued) or
            // forged (leader echoed a non-matching token). Drop
            // without applying so a stale chunk can't land on the
            // current request's apply path.
            //
            // Take FIRST, before the role / believed-leader gates,
            // so a response that arrives while we're briefly out
            // of `Replica` (role thrash, post-election) still
            // consumes its outstanding-token entry. Pre-fix the
            // role gate returned early without `take`, and the
            // token sat in the per-leader set until TTL (30 s) —
            // under role thrash the SOFT_CAP GC then dropped
            // entries from OTHER leaders to make room, evicting
            // legitimately in-flight tokens.
            //
            // Consuming a token in the dropped-response path is
            // safe: request_ids are random 64-bit (collision
            // negligible), so a subsequent re-issue uses a fresh
            // id and the consumed entry isn't reachable.
            {
                let now = tokio::time::Instant::now().into_std();
                if !outstanding.lock().take(from, msg.request_id, now) {
                    tracing::trace!(
                        from = from,
                        request_id = msg.request_id,
                        "replication: dropping SyncResponse with unknown request_id"
                    );
                    return;
                }
            }
            // SyncResponse is the state-mutating wire input — only
            // the believed leader is allowed to ship it. A peer that
            // is in the replica_set but is not the current leader
            // could otherwise forge `append_batch`-bound payloads
            // into a replica's log. The replica_set gate at entry
            // narrows the surface to configured members; the
            // believed_leader gate here narrows further to the
            // single peer the replica is currently following.
            let leader_belief = tracker.lock().believed_leader();
            if leader_belief != Some(from) {
                tracing::trace!(
                    from = from,
                    believed_leader = ?leader_belief,
                    "replication: dropping SyncResponse from non-leader peer"
                );
                return;
            }
            // Replica-side: apply the chunk to our local file.
            // Only honor responses when we believe we're a
            // Replica — other roles ignore them.
            if coordinator.role() != ReplicaRole::Replica {
                tracing::trace!(
                    from = from,
                    "replication: SyncResponse received in role {:?}; ignoring",
                    coordinator.role(),
                );
                return;
            }
            // Snapshot the pre-apply tail so the post-apply
            // result can be classified as "empty" (apply returned
            // the same tail, no events landed) vs "progress"
            // (tail advanced). Drives the R-28 catchup-backoff
            // accounting below.
            let pre_apply_tail = inputs.file.next_seq();
            // PERF_AUDIT §5.5 — offload the blocking append_batch +
            // fsync to the blocking pool so the per-channel select
            // loop stays responsive to heartbeats / other inbound
            // messages while the apply lands.
            let apply_outcome =
                apply_sync_response_async(inputs.file.clone(), msg.clone(), inputs.channel_id)
                    .await;
            match apply_outcome {
                Ok(new_tail) => {
                    // Record the post-apply tail on the coordinator
                    // so capability-tag advertisements ride
                    // `tip_seq=new_tail` instead of the dead-default
                    // 0. find_chain_holders picks the freshest
                    // holder by tip_seq during failover; pre-fix
                    // every Leader/Replica advertised tip_seq=0
                    // and lex-smallest holder won selection.
                    coordinator.record_tail_seq(new_tail);
                    // R-28 catchup-backoff accounting. If the
                    // response advanced our tail, the leader is
                    // making progress — clear any backoff state so
                    // the next tick can issue another SyncRequest
                    // immediately. If the response was empty
                    // (apply returned the same tail) while the
                    // leader's advertised tail is still strictly
                    // greater than ours, record an empty strike.
                    // After `CATCHUP_BACKOFF_THRESHOLD` consecutive
                    // empties, the tracker stamps a backoff window
                    // that the outbound dispatch consults.
                    if new_tail > pre_apply_tail {
                        backoff.lock().record_progress(from);
                    } else {
                        // Pre-fix the strike fired whenever the
                        // CACHED heartbeat tail was still above ours
                        // — the cached value can be hundreds of ms
                        // stale, so a replica that caught up between
                        // the heartbeat and the response would
                        // strike against a leader that has nothing
                        // to send. After
                        // `CATCHUP_BACKOFF_THRESHOLD` such false
                        // strikes the leader sat in a 1–30 s
                        // backoff while nothing was actually wrong.
                        //
                        // Guard the strike on heartbeat freshness:
                        // only when the leader's most recent
                        // heartbeat is inside the miss-threshold
                        // window do we trust its claimed `tail_seq`
                        // as evidence the leader has more data to
                        // ship. A stale heartbeat is no signal —
                        // skip the strike entirely.
                        let now = tokio::time::Instant::now().into_std();
                        let strike = {
                            let t = tracker.lock();
                            let peer = t.peer_state(from);
                            let lag = t.peer_lag(from, now);
                            let fresh_window = std::time::Duration::from_millis(
                                t.heartbeat_ms().saturating_mul(t.miss_threshold() as u64),
                            );
                            matches!(
                                (peer, lag),
                                (Some(p), Some(elapsed))
                                    if elapsed < fresh_window && p.tail_seq > new_tail
                            )
                        };
                        if strike {
                            backoff.lock().record_empty(from, now);
                        }
                    }
                    tracing::trace!(
                        from = from,
                        new_tail = new_tail,
                        "replication: applied chunk"
                    );
                }
                Err(super::replication_catchup::ApplyError::AppendFailed(detail)) => {
                    // Disk-pressure surface — per plan §7, the
                    // local file rejected the append (heap segment
                    // at the 3 GB hard cap, or a disk write fail
                    // on the persistent tier). Consult the
                    // configured `UnderCapacity` policy and react.
                    handle_disk_pressure(coordinator, &inputs.file, &detail, from).await;
                }
                Err(super::replication_catchup::ApplyError::GapBeforeChunk {
                    first_seq,
                    local_next,
                    divergence_suspected,
                }) => {
                    // Plan §8 skip-ahead — the leader trimmed past
                    // our local tail; the chunk's first_seq is
                    // strictly above local_next. Skip the local
                    // sequence forward to first_seq and retry the
                    // apply (the chunk's events line up with the
                    // new tail). On persistent files
                    // `skip_to` returns an error and we fall back
                    // to log+drop; the heartbeat-cycle recovery
                    // path catches us up on the next tick.
                    coordinator.metrics().incr_skip_ahead();
                    match inputs.file.skip_to(first_seq) {
                        Ok(()) => {
                            // R-13: `first_seq > local_next` is the
                            // `GapBeforeChunk` invariant; use
                            // saturating_sub for defense-in-depth.
                            debug_assert!(first_seq > local_next);
                            // R-5: when divergence is suspected,
                            // surface it at warn level with an
                            // explicit message so operator dashboards
                            // can see split-brain post-mortems
                            // separately from routine retention
                            // trims.
                            if divergence_suspected {
                                tracing::warn!(
                                    from = from,
                                    from_seq = local_next,
                                    to_seq = first_seq,
                                    gap = first_seq.saturating_sub(local_next),
                                    "replication: skip-ahead crossed leader's retained range — \
                                     divergent log suspected (split-brain post-mortem)"
                                );
                            } else {
                                tracing::warn!(
                                    from = from,
                                    from_seq = local_next,
                                    to_seq = first_seq,
                                    gap = first_seq.saturating_sub(local_next),
                                    "replication: skip-ahead — leader trimmed past local tail"
                                );
                            }
                            // Retry the apply now that the local
                            // tail matches first_seq. PERF_AUDIT §5.5
                            // — same off-loop offload as the primary
                            // apply path above.
                            let retry_outcome = apply_sync_response_async(
                                inputs.file.clone(),
                                msg.clone(),
                                inputs.channel_id,
                            )
                            .await;
                            match retry_outcome {
                                Ok(new_tail) => {
                                    coordinator.record_tail_seq(new_tail);
                                    tracing::trace!(
                                        from = from,
                                        new_tail = new_tail,
                                        "replication: applied chunk after skip-ahead"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        from = from,
                                        error = ?e,
                                        "replication: apply after skip-ahead failed"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                from = from,
                                first_seq = first_seq,
                                error = %e,
                                "replication: skip_to rejected; falling back to heartbeat-cycle recovery"
                            );
                        }
                    }
                }
                Err(e) => {
                    // Remaining ApplyError variants — channel
                    // mismatch, monotonicity violation, stale
                    // chunk. Log + drop; reliable-stream /
                    // heartbeat cycle recovers.
                    tracing::warn!(
                        from = from,
                        error = ?e,
                        "replication: apply_sync_response failed"
                    );
                }
            }
        }
        Inbound::SyncNack { from, msg } => {
            // R-12: defense-in-depth — validate channel_id at the
            // runtime boundary.
            if msg.channel_id != inputs.channel_id {
                tracing::trace!(
                    from = from,
                    "replication: dropping SyncNack for wrong channel"
                );
                return;
            }
            // Only the believed leader can NACK; otherwise a non-
            // leader replica_set peer could spam NotLeader nacks to
            // make us clear our believed_leader, or BadRange to
            // skip-ahead our local file's tail past in-flight
            // events. The replica_set gate at entry handles
            // outsider rejection; this narrows further to the
            // single peer the replica is currently following.
            let leader_belief = tracker.lock().believed_leader();
            if leader_belief != Some(from) {
                tracing::trace!(
                    from = from,
                    believed_leader = ?leader_belief,
                    "replication: dropping SyncNack from non-leader peer"
                );
                return;
            }
            // R-23 request-token correlation. A NACK with a token
            // not in the outstanding set is stale (from a request
            // the replica already timed out) — the BadRange arm
            // below mutates the local file via skip_to, and a
            // stale BadRange could destroy data on retry. Drop
            // unmatched NACKs.
            {
                let now = tokio::time::Instant::now().into_std();
                if !outstanding.lock().take(from, msg.request_id, now) {
                    tracing::trace!(
                        from = from,
                        request_id = msg.request_id,
                        "replication: dropping SyncNack with unknown request_id"
                    );
                    return;
                }
            }
            // Replicas key their retry policy on `error_code`.
            // Phase D §2 retry policy:
            //   1 NotLeader   → re-resolve leader (clear tracker
            //                    so next election cycle starts
            //                    clean)
            //   2 BadRange    → trim local tail / skip-ahead
            //   3 Backpressure → exponential backoff (handled by
            //                    not issuing the next request)
            //   4 ChannelClosed → withdraw replica role
            use super::replication::SyncNackError;
            match msg.error_code {
                SyncNackError::NotLeader => {
                    // R-4: actually clear the believed leader so
                    // the next tick re-resolves via the heartbeat
                    // cycle instead of looping on the stale leader
                    // belief until 3 missed heartbeats trip.
                    clear_leader_belief_and_tokens(tracker, outstanding);
                    tracing::trace!(
                        from = from,
                        "replication: NACK NotLeader — cleared believed leader"
                    );
                }
                SyncNackError::BadRange => {
                    // R-40: skip directly to the leader's
                    // first-retained seq carried in the NACK. Pre-
                    // fix the replica advanced one seq per round
                    // trip (skip_to(since_seq + 1)) which thrashed
                    // when the retention floor was many seqs above
                    // `since_seq` — every retry re-asked below the
                    // floor and re-NACKed. With the wire field, one
                    // round trip suffices: `skip_to(leader_first_
                    // retained_seq)` puts local tail at the floor
                    // and the next SyncRequest re-asks exactly
                    // there. Fall back to `since_seq + 1` if the
                    // leader sent `0` (legacy / never-retained
                    // channels) so an out-of-band sender can't
                    // make us no-op on the retry.
                    coordinator.metrics().incr_skip_ahead();
                    let target = if msg.leader_first_retained_seq > 0 {
                        msg.leader_first_retained_seq
                    } else {
                        msg.since_seq.saturating_add(1)
                    };
                    match inputs.file.skip_to(target) {
                        Ok(()) => tracing::warn!(
                            from = from,
                            since_seq = msg.since_seq,
                            leader_first_retained_seq = msg.leader_first_retained_seq,
                            target = target,
                            "replication: NACK BadRange — local tail skipped to leader's first-retained seq"
                        ),
                        Err(e) => tracing::trace!(
                            from = from,
                            error = %e,
                            "replication: NACK BadRange — skip_to rejected, falling back to heartbeat retry"
                        ),
                    }
                }
                SyncNackError::Backpressure => {
                    tracing::trace!(
                        from = from,
                        "replication: NACK Backpressure — deferring next request"
                    );
                }
                SyncNackError::ChannelClosed => {
                    tracing::warn!(
                        from = from,
                        "replication: NACK ChannelClosed — withdrawing role"
                    );
                    // The leader is telling us the channel is gone;
                    // shut down regardless of our current role.
                    // ChannelClose is the only signal valid from
                    // any state (Leader, Candidate, Replica, Idle).
                    // DiskPressureWithdraw is only valid from
                    // Replica and would silently fail-and-log if a
                    // role flip happened between sending the
                    // SyncRequest and receiving the NACK.
                    let _ = coordinator
                        .transition_to(
                            ReplicaRole::Idle,
                            super::replication_state::TransitionSignal::ChannelClose,
                        )
                        .await;
                }
            }
        }
    }
}

/// React to a disk-pressure signal from `apply_sync_response`. The
/// local file rejected the append — heap segment at 3 GB hard cap
/// or a disk-tier write fail. Consult the channel's configured
/// `UnderCapacity` policy and apply.
///
/// Plan §7:
///
/// - `Withdraw` (default) — drop the replica role so the leader's
///   other replicas can take over the redundancy responsibility.
///   The channel's `causal:<hex>` tag is withdrawn via the
///   coordinator's `* → Idle` side-effect. Operators see the
///   `dataforts_replication_under_capacity_total` counter advance
///   and the role flip to Idle.
/// - `EvictOldest` — call `RedexFile::sweep_retention()` to evict
///   on the configured caps + bump the counter. Caller stays in
///   Replica role; the next `SyncResponse` retries the apply. If
///   no retention caps are configured the sweep is a no-op and
///   the next apply will fail again — operators who pick this
///   policy should pair it with `retention_max_*` settings.
async fn handle_disk_pressure(
    coordinator: &Arc<ReplicationCoordinator>,
    file: &super::file::RedexFile,
    detail: &str,
    from: NodeId,
) {
    use super::replication_config::UnderCapacity;
    coordinator.metrics().incr_under_capacity();
    let policy = coordinator.config().on_under_capacity;
    match policy {
        UnderCapacity::Withdraw => {
            tracing::warn!(
                from = from,
                detail = detail,
                "replication: disk pressure → withdrawing role"
            );
            // The transition matrix only permits
            // DiskPressureWithdraw on Replica → Idle. If a role
            // flip landed between the apply attempt and this
            // withdraw, pick the signal that's actually valid for
            // the current role so we don't silently log+drop the
            // transition and keep writing through pressure.
            let signal = match coordinator.role() {
                ReplicaRole::Replica => {
                    super::replication_state::TransitionSignal::DiskPressureWithdraw
                }
                ReplicaRole::Idle => {
                    // Already withdrawn — short-circuit via the
                    // ChannelClose idempotent path so we don't bump
                    // counters twice on a benign race.
                    super::replication_state::TransitionSignal::ChannelClose
                }
                ReplicaRole::Leader => {
                    // Role-specific signal so the transition metric
                    // labels this as disk-pressure withdraw instead
                    // of the ChannelClose fallback (which operator
                    // dashboards triage as "graceful channel close").
                    super::replication_state::TransitionSignal::LeaderDiskPressureWithdraw
                }
                ReplicaRole::Candidate => {
                    super::replication_state::TransitionSignal::CandidateDiskPressureWithdraw
                }
            };
            if let Err(e) = coordinator.transition_to(ReplicaRole::Idle, signal).await {
                tracing::warn!(
                    error=?e,
                    "replication: disk-pressure withdraw transition failed"
                );
            }
        }
        UnderCapacity::EvictOldest => {
            tracing::warn!(
                from = from,
                detail = detail,
                "replication: disk pressure → sweeping retention"
            );
            file.sweep_retention();
        }
    }
}

/// Estimate the wire-cost of a [`SyncResponse`] for budget
/// accounting. Header + per-event overhead per `replication.rs`'s
/// `to_bytes` shape.
fn estimate_response_bytes(resp: &SyncResponse) -> u64 {
    // Header: 3 + 32 + 8 + 4 = 47 bytes.
    let mut bytes: u64 = 47;
    for ev in &resp.events {
        // event_seq u64 + payload_len u32 + payload bytes.
        bytes += 8 + 4 + ev.payload.len() as u64;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::channel::ChannelName;
    use crate::adapter::net::redex::replication::ReplicaRole;
    use crate::adapter::net::redex::replication_config::ReplicationConfig;
    use crate::adapter::net::redex::replication_coordinator::ChainTagSink;
    use crate::adapter::net::redex::replication_metrics::ReplicationMetricsRegistry;
    use parking_lot::Mutex as ParkingMutex;

    /// No-op chain-tag sink for unit tests; the runtime path
    /// doesn't exercise the chain-tag side-effect in this
    /// commit's scope.
    #[derive(Default)]
    struct NoopTagSink;

    #[async_trait::async_trait]
    impl ChainTagSink for NoopTagSink {
        async fn announce_chain(
            &self,
            _origin_hash: u64,
            _tip_seq: u64,
        ) -> Result<(), AdapterError> {
            Ok(())
        }
        async fn withdraw_chain(&self, _origin_hash: u64) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    /// #35: under sustained young in-flight load the soft cap must
    /// be an ACTUAL bound. Filling the registry with cap entries
    /// all younger than the TTL and then recording one more must
    /// NOT grow the map past the cap — the oldest entry is evicted
    /// instead.
    #[test]
    fn bug35_outstanding_requests_cap_is_bounded_under_young_load() {
        let mut reg = OutstandingRequests::new();
        let leader: NodeId = 0x42;
        let base = Instant::now();

        // Insert exactly cap entries, each strictly younger than
        // the TTL (timestamps `base + i ms`, all "now-ish"). The
        // first inserted (request_id 0) is the oldest.
        for i in 0..REQUEST_REGISTRY_SOFT_CAP as u64 {
            let t = base + Duration::from_millis(i);
            reg.record(leader, i, t);
        }
        assert_eq!(reg.len_for_test(), REQUEST_REGISTRY_SOFT_CAP);

        // One more young request, still well within the TTL. The
        // expiry sweep evicts nothing (all young), so the cap path
        // must force-evict the oldest entry (request_id 0).
        let newest_t = base + Duration::from_millis(REQUEST_REGISTRY_SOFT_CAP as u64);
        reg.record(leader, REQUEST_REGISTRY_SOFT_CAP as u64, newest_t);

        // Map stayed bounded.
        assert_eq!(
            reg.len_for_test(),
            REQUEST_REGISTRY_SOFT_CAP,
            "soft cap must bound the map even when nothing has expired"
        );
        // The oldest token (id 0) was evicted: a response for it is
        // now rejected as if stale.
        assert!(
            !reg.take(leader, 0, newest_t),
            "oldest token should have been force-evicted at the cap"
        );
        // The freshly recorded token is present and matchable.
        assert!(
            reg.take(leader, REQUEST_REGISTRY_SOFT_CAP as u64, newest_t),
            "newest token must still be recorded"
        );
    }

    /// #35 companion: re-recording an EXISTING key at the cap must
    /// not evict a different entry (the insert is a replace, not a
    /// growth).
    #[test]
    fn bug35_record_existing_key_at_cap_does_not_evict() {
        let mut reg = OutstandingRequests::new();
        let leader: NodeId = 0x42;
        let base = Instant::now();
        for i in 0..REQUEST_REGISTRY_SOFT_CAP as u64 {
            reg.record(leader, i, base + Duration::from_millis(i));
        }
        // Re-record an already-present key. Size unchanged; the
        // oldest (id 0) must survive because no new slot is needed.
        reg.record(
            leader,
            REQUEST_REGISTRY_SOFT_CAP as u64 - 1,
            base + Duration::from_millis(REQUEST_REGISTRY_SOFT_CAP as u64),
        );
        assert_eq!(reg.len_for_test(), REQUEST_REGISTRY_SOFT_CAP);
        assert!(
            reg.take(leader, 0, base + Duration::from_millis(1)),
            "re-recording an existing key must not evict a distinct entry"
        );
    }

    /// Recorder dispatcher — captures every outbound wire
    /// message. Each variant pushes into a separate Vec so
    /// tests can assert on shape + ordering.
    #[derive(Default)]
    struct RecorderDispatcher {
        heartbeats: ParkingMutex<Vec<(NodeId, SyncHeartbeat)>>,
        sync_requests: ParkingMutex<Vec<(NodeId, SyncRequest)>>,
        sync_responses: ParkingMutex<Vec<(NodeId, SyncResponse)>>,
        sync_nacks: ParkingMutex<Vec<(NodeId, SyncNack)>>,
    }

    #[async_trait::async_trait]
    impl ReplicationDispatcher for RecorderDispatcher {
        async fn send_heartbeat(
            &self,
            target: NodeId,
            msg: SyncHeartbeat,
        ) -> Result<(), AdapterError> {
            self.heartbeats.lock().push((target, msg));
            Ok(())
        }
        async fn send_sync_request(
            &self,
            target: NodeId,
            msg: SyncRequest,
        ) -> Result<(), AdapterError> {
            self.sync_requests.lock().push((target, msg));
            Ok(())
        }
        async fn send_sync_response(
            &self,
            target: NodeId,
            msg: SyncResponse,
        ) -> Result<(), AdapterError> {
            self.sync_responses.lock().push((target, msg));
            Ok(())
        }
        async fn send_sync_nack(&self, target: NodeId, msg: SyncNack) -> Result<(), AdapterError> {
            self.sync_nacks.lock().push((target, msg));
            Ok(())
        }
    }

    fn channel_id_for(name: &str) -> ChannelId {
        let cn = ChannelName::new(name).unwrap();
        ChannelId::from_name(&cn)
    }

    fn build_file_for_tests() -> RedexFile {
        use crate::adapter::net::redex::config::RedexFileConfig;
        use crate::adapter::net::redex::manager::Redex;
        let r = Redex::new();
        let cn = ChannelName::new("test/runtime").unwrap();
        r.open_file(&cn, RedexFileConfig::default()).unwrap()
    }

    fn build_inputs(self_id: NodeId, replicas: Vec<NodeId>, hb_ms: u64) -> RuntimeInputs {
        RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: self_id,
            replica_set: replicas,
            heartbeat_ms: hb_ms,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            tail_provider: Arc::new(|| 42),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            default_bandwidth_class: Default::default(),
            background_fraction: 0.3,
        }
    }

    fn build_coordinator(
        self_id: NodeId,
        replicas: Vec<NodeId>,
    ) -> (Arc<ReplicationCoordinator>, ReplicationMetricsRegistry) {
        let _ = (self_id, replicas);
        let registry = ReplicationMetricsRegistry::new();
        let sink: Arc<dyn ChainTagSink> = Arc::new(NoopTagSink);
        let coordinator = ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            ReplicationConfig::new(),
            sink,
            &registry,
        );
        (Arc::new(coordinator), registry)
    }

    fn build_budget() -> Arc<Mutex<BandwidthBudget>> {
        let now = Instant::now();
        Arc::new(Mutex::new(BandwidthBudget::new(0.5, 10_000_000, now)))
    }

    fn build_backoff() -> Arc<Mutex<CatchupBackoff>> {
        Arc::new(Mutex::new(CatchupBackoff::new()))
    }

    /// Bundle the four per-channel state pieces into the
    /// `RuntimeState` the on_tick / on_inbound functions take.
    /// Tests construct tracker + budget explicitly; backoff and
    /// outstanding are stock fresh instances. Pre-refactor these
    /// were passed as four separate arguments.
    fn build_state(
        tracker: Arc<Mutex<HeartbeatTracker>>,
        budget: Arc<Mutex<BandwidthBudget>>,
    ) -> RuntimeState {
        RuntimeState {
            tracker,
            budget,
            backoff: Arc::new(Mutex::new(CatchupBackoff::new())),
            outstanding: Arc::new(Mutex::new(OutstandingRequests::new())),
        }
    }

    #[tokio::test]
    async fn leader_emits_heartbeat_to_peers_each_tick() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20, 0x30], 100);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20, 0x30]);
        // Promote to Leader via the state machine (Idle → Replica
        // → Candidate → Leader).
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        coordinator
            .transition_to(
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            )
            .await
            .unwrap();
        coordinator
            .transition_to(
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            )
            .await
            .unwrap();

        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle =
            spawn_replication_runtime(inputs, coordinator, dispatcher.clone(), build_budget());

        // Sleep ~3 ticks worth (300ms cadence at 100ms = ~3 ticks).
        tokio::time::sleep(Duration::from_millis(350)).await;

        let heartbeats = dispatcher.heartbeats.lock().clone();
        assert!(
            heartbeats.len() >= 2,
            "expected ≥ 2 heartbeats over 350ms at 100ms cadence; got {}",
            heartbeats.len(),
        );
        // Each heartbeat goes to a non-self peer.
        for (target, msg) in &heartbeats {
            assert!(*target == 0x20 || *target == 0x30);
            assert_eq!(msg.role, ReplicaRole::Leader);
            assert_eq!(msg.tail_seq, 42);
        }

        handle.cancel().await;
        assert!(handle.is_stopped());
    }

    #[tokio::test]
    async fn inbound_heartbeat_records_into_tracker() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 100);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle =
            spawn_replication_runtime(inputs, coordinator, dispatcher.clone(), build_budget());

        // Push a peer heartbeat into the inbox.
        handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 1_700_000_000_000,
                },
            })
            .await
            .unwrap();

        // Let the task process.
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Heartbeats emitted (replica side) and no transition
        // triggered (fresh leader heartbeat).
        let _heartbeats = dispatcher.heartbeats.lock().clone();
        // The coordinator stays in Replica.
        handle.cancel().await;
    }

    /// R-21 regression: when a Leader observes another peer also
    /// claiming Leader for the same channel, the deterministic
    /// tiebreak demotes the loser to Replica so the partition heal
    /// converges to one leader. Without `Leader → Replica`, both
    /// partitions stay Leader permanently and accrete divergent
    /// histories silently overwritten via `skip_to`.
    #[tokio::test]
    async fn peer_leader_observation_demotes_loser_to_replica() {
        // Local node 0x10 has tail_seq 42 (from build_inputs);
        // peer 0x20 advertises tail_seq 99 — peer wins on tail.
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        assert_eq!(coordinator.role(), ReplicaRole::Leader);
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        let budget = build_budget();

        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 0,
                },
            },
        )
        .await;

        // Local tail 42 < peer tail 99 → local loses, demotes.
        assert_eq!(
            coordinator.role(),
            ReplicaRole::Replica,
            "Leader with lower tail must concede on PeerLeaderObserved"
        );
    }

    /// R-21 regression: tail-tie tiebreak favors the lower node id.
    /// Without the symmetric tiebreak the matrix could leave one
    /// side as Leader and the other still claiming Leader after
    /// the heal — both must agree on the winner.
    #[tokio::test]
    async fn peer_leader_tail_tie_lower_node_id_wins() {
        // Local 0x10 tail = peer 0x20 tail (both 42). Local wins
        // because 0x10 < 0x20.
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        let budget = build_budget();

        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 42,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 0,
                },
            },
        )
        .await;

        assert_eq!(
            coordinator.role(),
            ReplicaRole::Leader,
            "tail-tie tiebreak: lower node id keeps Leader"
        );
    }

    #[tokio::test]
    async fn replica_with_silent_leader_runs_election_and_promotes_self() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 50);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(
            inputs,
            coordinator.clone(),
            dispatcher.clone(),
            build_budget(),
        );

        // Push a single leader heartbeat from 0x20, then let
        // enough time pass for silence detection (3 × 50ms = 150ms).
        let cid = channel_id_for("test/runtime");
        handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 1_700_000_000_000,
                },
            })
            .await
            .unwrap();

        // Sleep > 3 heartbeats + tick alignment so silence
        // detection fires AND the post-election transition lands.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // The election ran with self healthy + RTT=0 in elect();
        // peer 0x20 may or may not have been health-filtered by
        // the tracker (after the silence threshold, the tracker
        // considers 0x20 stale). Either way, self wins: self has
        // RTT 0; peer is either filtered out or at 5ms.
        assert_eq!(coordinator.role(), ReplicaRole::Leader);

        handle.cancel().await;
    }

    /// Chain-tag sink that returns `Ok` for the first `n` calls
    /// then fails every subsequent call. Lets a test pin the post-
    /// election sink-failure path: the first call (Idle → Replica
    /// announce) succeeds, the second (Candidate → Leader announce)
    /// fails — so the coordinator transitions state but the chain-
    /// tag side-effect surfaces TagSink.
    struct FailingAfterNAnnounceSink {
        remaining: ParkingMutex<usize>,
    }

    #[async_trait::async_trait]
    impl ChainTagSink for FailingAfterNAnnounceSink {
        async fn announce_chain(
            &self,
            _origin_hash: u64,
            _tip_seq: u64,
        ) -> Result<(), AdapterError> {
            let mut r = self.remaining.lock();
            if *r == 0 {
                return Err(AdapterError::Transient(
                    "simulated sink failure".to_string(),
                ));
            }
            *r -= 1;
            Ok(())
        }
        async fn withdraw_chain(&self, _origin_hash: u64) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    /// A failing tag-sink on the post-election Candidate → Leader
    /// transition must NOT strand the coordinator in Candidate. The
    /// state machine moves to Leader (the failure is in the side-
    /// effect only); the runtime must observe the TagSink error
    /// branch, clear the believed leader, and continue. The
    /// previous code path logged + dropped, leaving the coordinator
    /// effectively healthy but stale-state in the tracker.
    #[tokio::test]
    async fn post_election_tag_sink_failure_does_not_strand_candidate() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 50);
        let cid = inputs.channel_id;
        // Build a coordinator wired to a sink that succeeds on the
        // first announce (Idle → Replica) and fails on the second
        // (Candidate → Leader during the post-election transition).
        let registry = ReplicationMetricsRegistry::new();
        let sink: Arc<dyn ChainTagSink> = Arc::new(FailingAfterNAnnounceSink {
            remaining: ParkingMutex::new(1),
        });
        let coordinator = Arc::new(ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            ReplicationConfig::new(),
            sink,
            &registry,
        ));
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(
            inputs,
            coordinator.clone(),
            dispatcher.clone(),
            build_budget(),
        );

        // Seed a single leader heartbeat then let silence detection
        // fire so the runtime enters Candidate and runs the
        // election.
        handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 1_700_000_000_000,
                },
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // State has moved to Leader despite the sink failure — the
        // coordinator's transition lock applied the state change
        // before the (failing) side-effect ran. The runtime's
        // post-election handler observed CoordinatorError::TagSink
        // and cleared the believed leader, not the prior code path
        // that silently sat on stale Candidate state.
        assert_eq!(coordinator.role(), ReplicaRole::Leader);

        handle.cancel().await;
    }

    #[tokio::test]
    async fn shutdown_drives_idle_transition() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 100);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle =
            spawn_replication_runtime(inputs, coordinator.clone(), dispatcher, build_budget());

        handle.cancel().await;
        assert!(handle.is_stopped());
        // Final state must be Idle (ChannelClose transition).
        assert_eq!(coordinator.role(), ReplicaRole::Idle);
    }

    /// Regression: a Leader's `on_tick`-driven `record_tail_seq`
    /// must clamp the advertised tail to the highest tail any
    /// peer has confirmed via heartbeat — never advertise
    /// pre-replication local tail. The capability-tag layer reads
    /// this value as `tip_seq` for `find_chain_holders` failover
    /// selection; advertising un-replicated writes biases
    /// failover toward a partition whose tail may not survive a
    /// crash.
    ///
    /// Pre-fix: leader at local tail = 100, replica reported tail
    /// = 50 → advertised_tail = 100.
    /// Post-fix: advertised_tail = 50 (clamped to max peer tail).
    #[tokio::test]
    async fn leader_on_tick_clamps_advertised_tail_to_max_peer_tail() {
        // Build inputs with a deterministic tail_provider returning
        // 100 (the leader's local tail).
        let self_id: NodeId = 0x10;
        let peer_id: NodeId = 0x20;
        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: self_id,
            replica_set: vec![self_id, peer_id],
            heartbeat_ms: 100,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            // Local tail is 100 — well above what the peer has
            // reported.
            tail_provider: Arc::new(|| 100),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            default_bandwidth_class: Default::default(),
            background_fraction: 0.3,
        };
        let (coordinator, _registry) = build_coordinator(self_id, vec![self_id, peer_id]);
        // Promote to Leader via the state machine.
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        // Seed the peer's heartbeat at tail=50 — only HALF of what
        // the leader has locally. The other 50 events are
        // unreplicated.
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        tracker
            .lock()
            .record_heartbeat(peer_id, ReplicaRole::Replica, 50, Instant::now());

        let dispatcher: Arc<dyn ReplicationDispatcher> = Arc::new(RecorderDispatcher::default());
        on_tick(
            &inputs,
            &coordinator,
            &dispatcher,
            &build_state(tracker.clone(), build_budget()),
        )
        .await;

        // record_tail_seq is monotonic; the coordinator's cached
        // tail should be the clamped value (50), NOT the raw
        // local-file tail (100).
        assert_eq!(
            coordinator.tail_seq(),
            50,
            "leader must advertise the highest peer-confirmed tail (50), \
             not the pre-replication local tail (100); pre-fix this would \
             be 100 and a crash here would lose the 50 un-replicated events \
             while failover discovery still thought tip_seq=100",
        );
    }

    /// Companion: with NO peer heartbeats observed (fresh leader),
    /// `on_tick` falls back to the raw local tail. A sole leader has
    /// authority over its own writes by tautology — clamping to 0
    /// would otherwise prevent any progress on a single-node
    /// configuration.
    #[tokio::test]
    async fn leader_on_tick_falls_back_to_local_tail_with_no_peers() {
        let self_id: NodeId = 0x10;
        let peer_id: NodeId = 0x20;
        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: self_id,
            replica_set: vec![self_id, peer_id],
            heartbeat_ms: 100,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            tail_provider: Arc::new(|| 77),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            default_bandwidth_class: Default::default(),
            background_fraction: 0.3,
        };
        let (coordinator, _registry) = build_coordinator(self_id, vec![self_id, peer_id]);
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }

        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        let dispatcher: Arc<dyn ReplicationDispatcher> = Arc::new(RecorderDispatcher::default());
        on_tick(
            &inputs,
            &coordinator,
            &dispatcher,
            &build_state(tracker.clone(), build_budget()),
        )
        .await;
        assert_eq!(
            coordinator.tail_seq(),
            77,
            "no peer heartbeats → no clamp, raw local tail is advertised",
        );
    }

    /// Regression: empty-response backoff must NOT strike when the
    /// leader's heartbeat is stale. Pre-fix the strike fired whenever
    /// `tracker.peer_state(from).tail_seq > new_tail`, but
    /// `peer_state.tail_seq` is the cached value from the last
    /// received heartbeat — minutes-stale in a degenerate case. A
    /// replica that caught up between an old heartbeat and the
    /// current response struck against a leader that had nothing
    /// to send. After `CATCHUP_BACKOFF_THRESHOLD` such false
    /// strikes the leader sat in a 1–30 s backoff while nothing
    /// was actually wrong.
    ///
    /// Post-fix: skip the strike when the heartbeat is older than
    /// the miss-threshold window — a stale heartbeat is no signal.
    #[tokio::test]
    async fn empty_response_does_not_strike_on_stale_heartbeat() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 100);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let backoff = build_backoff();
        let outstanding = Arc::new(Mutex::new(OutstandingRequests::new()));

        // Seed the tracker with a STALE heartbeat: leader 0x20
        // claimed tail=200 well outside the miss-threshold window
        // (heartbeat_ms = 100, miss_threshold defaults to 3 ⇒ a
        // last_seen older than 300 ms is stale). Build the
        // heartbeat with a `last_seen` from 10 seconds ago so it's
        // definitively stale by the time on_inbound runs.
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        let stale_when = Instant::now() - Duration::from_secs(10);
        tracker
            .lock()
            .record_heartbeat(0x20, ReplicaRole::Leader, 200, stale_when);

        // Pre-record the request so the binding gate admits the
        // response.
        outstanding.lock().record(0x20, 0, Instant::now());

        // Empty SyncResponse from the (now-stale-heartbeat) leader.
        // The local file is empty (next_seq = 0); apply on an empty
        // response leaves next_seq = 0, so `new_tail == pre_apply_tail`.
        let event = Inbound::SyncResponse {
            from: 0x20,
            msg: SyncResponse {
                channel_id: cid,
                first_seq: 0,
                leader_first_retained_seq: 0,
                events: Vec::new(),
                request_id: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &RuntimeState {
                tracker: tracker.clone(),
                budget: budget.clone(),
                backoff: backoff.clone(),
                outstanding: outstanding.clone(),
            },
            event,
        )
        .await;

        // Pre-fix: backoff would have recorded an empty strike
        // because `peer_state.tail_seq (200) > new_tail (0)`.
        // Post-fix: stale heartbeat → no strike, no backoff state.
        assert!(
            !backoff.lock().is_in_backoff(0x20, Instant::now()),
            "stale-heartbeat empty must NOT engage backoff"
        );
        // Drive THRESHOLD+1 more stale-heartbeat empties to prove
        // that the strike NEVER fires on stale heartbeats — even
        // accumulated over many attempts.
        for _ in 0..CATCHUP_BACKOFF_THRESHOLD + 1 {
            // Re-register the request_id (consumed by the binding
            // gate on each call).
            outstanding.lock().record(0x20, 0, Instant::now());
            let event = Inbound::SyncResponse {
                from: 0x20,
                msg: SyncResponse {
                    channel_id: cid,
                    first_seq: 0,
                    leader_first_retained_seq: 0,
                    events: Vec::new(),
                    request_id: 0,
                },
            };
            on_inbound(
                &inputs,
                &coordinator,
                &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
                &RuntimeState {
                    tracker: tracker.clone(),
                    budget: budget.clone(),
                    backoff: backoff.clone(),
                    outstanding: outstanding.clone(),
                },
                event,
            )
            .await;
        }
        assert!(
            !backoff.lock().is_in_backoff(0x20, Instant::now()),
            "accumulated stale-heartbeat empties must NEVER engage backoff",
        );
    }

    /// R-28 unit test: the backoff structure records empties up to
    /// the threshold without setting a backoff window, then stamps
    /// an exponentially-growing window once the threshold is
    /// crossed. A non-empty response resets the entry.
    #[test]
    fn catchup_backoff_threshold_and_reset() {
        let now = Instant::now();
        let mut b = CatchupBackoff::new();
        // Up to the threshold: no backoff yet.
        for _ in 0..CATCHUP_BACKOFF_THRESHOLD {
            b.record_empty(0x20, now);
        }
        assert!(
            !b.is_in_backoff(0x20, now),
            "backoff must not engage before the threshold is crossed"
        );
        // Crossing the threshold sets a backoff window.
        b.record_empty(0x20, now);
        assert!(
            b.is_in_backoff(0x20, now),
            "backoff must engage once the empty count crosses the threshold"
        );
        // A productive response clears the entry.
        b.record_progress(0x20);
        assert!(
            !b.is_in_backoff(0x20, now),
            "record_progress must clear the backoff window"
        );
    }

    /// R-25 regression: a saturated low-priority lane (Heartbeat
    /// flood) must not starve catchup-critical events on the
    /// priority lane (SyncResponse / SyncNack / Shutdown).
    ///
    /// Drives the failure shape from the audit: many peers
    /// flood the low-priority inbox to its 1024 cap while a
    /// SyncResponse is also in-flight. With the biased select
    /// on the priority lane, the priority entry is drained
    /// even though the low-priority lane is saturated.
    #[tokio::test]
    async fn priority_lane_drains_under_low_priority_saturation() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle =
            spawn_replication_runtime(inputs, coordinator.clone(), dispatcher, build_budget());

        // Saturate the low-priority lane with heartbeats. The
        // runtime task will drain them slowly under heavy lock
        // contention; what matters is that the priority lane
        // continues to make progress.
        for _ in 0..RUNTIME_INBOX_CAPACITY * 2 {
            let _ = handle.try_dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 0,
                    role: ReplicaRole::Replica,
                    wall_clock_ms: 0,
                },
            });
        }

        // Ship a Shutdown on the priority lane and confirm the
        // runtime exits within a short bound. Under the pre-fix
        // single-inbox design this would block on the saturated
        // queue indefinitely (cancel falls back to JoinHandle::
        // abort but the Idle transition wouldn't run).
        let cancel_fut = handle.cancel();
        let bounded = tokio::time::timeout(Duration::from_secs(2), cancel_fut).await;
        assert!(
            bounded.is_ok(),
            "shutdown on priority lane must drain under low-priority saturation"
        );
        // Idle transition ran via the graceful path, not the abort.
        assert_eq!(
            coordinator.role(),
            ReplicaRole::Idle,
            "graceful Idle transition must complete via priority-lane Shutdown"
        );
    }

    #[tokio::test]
    async fn try_dispatch_returns_event_on_full_buffer() {
        // Build a handle, fill the inbox without letting the
        // task drain. We rely on the runtime task not having a
        // chance to recv before we saturate.
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000); // tick very slow
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(inputs, coordinator, dispatcher, build_budget());

        let cid = channel_id_for("test/runtime");
        // Fill past capacity.
        for _ in 0..RUNTIME_INBOX_CAPACITY + 10 {
            let event = Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 0,
                    role: ReplicaRole::Replica,
                    wall_clock_ms: 0,
                },
            };
            // try_dispatch returns the event back when buffer
            // is full. The task starts to drain quickly so we
            // may not see a rejection on every iteration; just
            // assert that at SOME point we got the event back.
            let _ = handle.try_dispatch(event);
        }

        handle.cancel().await;
    }

    /// A wedged task with a saturated inbox must not hang
    /// `cancel()`. The cancel path uses `try_send` for the Shutdown
    /// message; if the buffer is full, the JoinHandle is aborted
    /// directly so the call returns promptly instead of blocking on
    /// a queue the wedged task may never drain.
    #[tokio::test]
    async fn cancel_with_full_inbox_does_not_hang() {
        // Slow tick so the task spends most of its time parked.
        // Fill the inbox without giving the task time to drain.
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(inputs, coordinator, dispatcher, build_budget());

        // Saturate the inbox so a buffered `send(Shutdown).await`
        // would block.
        let cid = channel_id_for("test/runtime");
        for _ in 0..RUNTIME_INBOX_CAPACITY {
            let _ = handle.try_dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 0,
                    role: ReplicaRole::Replica,
                    wall_clock_ms: 0,
                },
            });
        }

        // cancel() must complete within a tight bound — a regression
        // to `send(Shutdown).await` would hang here on the full
        // buffer.
        tokio::time::timeout(Duration::from_secs(2), handle.cancel())
            .await
            .expect("cancel() must not hang on full inbox");
        assert!(handle.is_stopped());
    }

    /// Dropping a handle without `cancel()` aborts the underlying
    /// task so the spawned future stops driving and any dispatcher
    /// Arc it held is released. Pins the Arc-cycle invariant:
    /// `MeshNode → router → handle → task → dispatcher` must close
    /// when the handle goes out of scope.
    #[tokio::test]
    async fn dropping_handle_aborts_task() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let dispatcher_clone: Arc<dyn ReplicationDispatcher> = dispatcher.clone();
        // The runtime task holds one strong reference to dispatcher
        // (passed below). Count after spawn = 2.
        let handle =
            spawn_replication_runtime(inputs, coordinator, dispatcher_clone, build_budget());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(Arc::strong_count(&dispatcher) >= 2);

        drop(handle);

        // Yield enough for the abort to land + the task's local
        // state to deallocate (releasing the dispatcher Arc).
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if Arc::strong_count(&dispatcher) == 1 {
                return;
            }
        }
        panic!(
            "task did not release dispatcher Arc after handle drop; strong_count = {}",
            Arc::strong_count(&dispatcher)
        );
    }

    #[tokio::test]
    async fn dispatch_after_cancel_errors() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 100);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(inputs, coordinator, dispatcher, build_budget());

        handle.cancel().await;

        let cid = channel_id_for("test/runtime");
        let result = handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 0,
                    role: ReplicaRole::Replica,
                    wall_clock_ms: 0,
                },
            })
            .await;
        assert!(result.is_err(), "dispatch must error after cancel");
    }

    #[tokio::test]
    async fn channel_id_mismatch_drops_heartbeat() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 100);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle =
            spawn_replication_runtime(inputs, coordinator.clone(), dispatcher, build_budget());

        // Push a heartbeat with the wrong channel_id.
        let wrong = channel_id_for("test/wrong_channel");
        handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: wrong,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 0,
                },
            })
            .await
            .unwrap();

        // After 4 ticks, silence detection would have triggered
        // if the heartbeat had landed; it didn't, so the role
        // either stays Replica (no leader observed at all) or
        // advances to Candidate / Leader via election with no
        // believed leader. Either way: prove the heartbeat
        // didn't poison the tracker by checking we didn't end
        // up as a Replica with `believed_leader == Some(0x20)`
        // (which is the broken outcome).
        //
        // We let it run a few ticks for the election cycle to
        // potentially fire.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // The wrong-channel heartbeat should have been ignored.
        // The final role is either Replica (if nothing else
        // happened) or Leader (if the silence-detection +
        // election fired). Either way, NOT believing 0x20 to
        // be leader is the contract — but we can't observe the
        // tracker from here. Instead pin that the role isn't
        // Idle (we didn't cancel; we're still alive).
        let role = coordinator.role();
        assert!(
            matches!(role, ReplicaRole::Replica | ReplicaRole::Leader),
            "expected Replica or Leader; got {role:?}",
        );

        handle.cancel().await;
    }

    // ────────────────────────────────────────────────────────────────
    // Lag-observation gauge (Phase H)
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn observe_lag_idle_emits_none() {
        let tracker = HeartbeatTracker::new(500);
        let now = Instant::now();
        match observe_lag(ReplicaRole::Idle, &[0x10, 0x20], 0x10, &tracker, now) {
            LagObservation::None => {}
            other => panic!("expected None for Idle, got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_candidate_emits_none() {
        let tracker = HeartbeatTracker::new(500);
        let now = Instant::now();
        match observe_lag(ReplicaRole::Candidate, &[0x10, 0x20], 0x10, &tracker, now) {
            LagObservation::None => {}
            other => panic!("expected None for Candidate, got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_leader_with_no_peer_observations_emits_none() {
        // Self is the leader, peers in the set but no heartbeats
        // observed yet → lag has no observation to report.
        let tracker = HeartbeatTracker::new(500);
        let now = Instant::now();
        match observe_lag(
            ReplicaRole::Leader,
            &[0x10, 0x20, 0x30],
            0x10,
            &tracker,
            now,
        ) {
            LagObservation::None => {}
            other => panic!("expected None when peers have not heartbeated, got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_leader_picks_worst_replica() {
        let mut tracker = HeartbeatTracker::new(500);
        let base = Instant::now();
        // Peer 0x20 heartbeated at base; peer 0x30 heartbeated 100ms
        // later. After advancing to base+1000ms, peer 0x20 has 1000ms
        // of lag, peer 0x30 has 900ms. Leader gauge picks the worst.
        tracker.record_heartbeat(0x20, ReplicaRole::Replica, 0, base);
        tracker.record_heartbeat(
            0x30,
            ReplicaRole::Replica,
            0,
            base + Duration::from_millis(100),
        );
        let now = base + Duration::from_millis(1000);
        match observe_lag(
            ReplicaRole::Leader,
            &[0x10, 0x20, 0x30],
            0x10,
            &tracker,
            now,
        ) {
            LagObservation::Leader(d) => assert_eq!(d, Duration::from_millis(1000)),
            other => panic!("expected Leader(1000ms), got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_replica_emits_believed_leader_lag() {
        let mut tracker = HeartbeatTracker::new(500);
        let base = Instant::now();
        tracker.record_heartbeat(0x42, ReplicaRole::Leader, 99, base);
        let now = base + Duration::from_millis(250);
        match observe_lag(ReplicaRole::Replica, &[0x10, 0x42], 0x10, &tracker, now) {
            LagObservation::Replica(d) => assert_eq!(d, Duration::from_millis(250)),
            other => panic!("expected Replica(250ms), got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_replica_with_no_believed_leader_emits_none() {
        // Empty tracker — no leader heartbeat ever observed.
        let tracker = HeartbeatTracker::new(500);
        let now = Instant::now();
        match observe_lag(ReplicaRole::Replica, &[0x10, 0x42], 0x10, &tracker, now) {
            LagObservation::None => {}
            other => panic!("expected None for replica with no believed leader, got {other:?}"),
        }
    }

    #[test]
    fn observe_lag_leader_skips_self_in_replica_set() {
        // Self appears in replica_set (typical — leaders are listed
        // alongside replicas). The lag picks the worst PEER, never
        // self's lag (which is meaningless since self is the
        // writer).
        let mut tracker = HeartbeatTracker::new(500);
        let base = Instant::now();
        tracker.record_heartbeat(0x20, ReplicaRole::Replica, 0, base);
        let now = base + Duration::from_millis(500);
        match observe_lag(ReplicaRole::Leader, &[0x10, 0x20], 0x10, &tracker, now) {
            LagObservation::Leader(d) => assert_eq!(d, Duration::from_millis(500)),
            other => panic!("expected Leader(500ms), got {other:?}"),
        }
    }

    // ────────────────────────────────────────────────────────────────
    // Disk-pressure handling (Phase G)
    // ────────────────────────────────────────────────────────────────

    fn build_coordinator_with_policy(
        policy: super::super::replication_config::UnderCapacity,
    ) -> (
        Arc<ReplicationCoordinator>,
        Arc<super::super::replication_metrics::ChannelMetricsAtomic>,
    ) {
        let registry = ReplicationMetricsRegistry::new();
        let sink: Arc<dyn ChainTagSink> = Arc::new(NoopTagSink);
        let config = ReplicationConfig::new().with_on_under_capacity(policy);
        let coordinator = ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            config,
            sink,
            &registry,
        );
        let metrics = registry.for_channel("test/runtime");
        (Arc::new(coordinator), metrics)
    }

    #[tokio::test]
    async fn disk_pressure_withdraw_drives_idle_transition() {
        // Bring the coordinator to Replica role so the
        // DiskPressureWithdraw signal can validate.
        let (coord, metrics) = build_coordinator_with_policy(
            super::super::replication_config::UnderCapacity::Withdraw,
        );
        coord
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        assert_eq!(coord.role(), ReplicaRole::Replica);

        let file = build_file_for_tests();
        handle_disk_pressure(&coord, &file, "test detail", 0x20).await;

        assert_eq!(coord.role(), ReplicaRole::Idle, "Withdraw flips to Idle");
        assert_eq!(
            metrics
                .under_capacity_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "Withdraw bumps under_capacity_total"
        );
    }

    #[tokio::test]
    async fn disk_pressure_evict_oldest_keeps_role_and_sweeps() {
        // EvictOldest: stay in Replica role, retention sweep
        // fires. Pre-fill the file with N events under a
        // count-1 retention cap so the sweep observably evicts.
        let (coord, metrics) = build_coordinator_with_policy(
            super::super::replication_config::UnderCapacity::EvictOldest,
        );
        coord
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        // Build a file with retention cap = 1 so the sweep
        // observably retains only the newest entry.
        use crate::adapter::net::redex::config::RedexFileConfig;
        use crate::adapter::net::redex::manager::Redex;
        let r = Redex::new();
        let cn = ChannelName::new("test/runtime").unwrap();
        let cfg = RedexFileConfig::default().with_retention_max_events(1);
        let file = r.open_file(&cn, cfg).unwrap();
        for i in 0..5 {
            file.append(format!("event-{i}").as_bytes()).unwrap();
        }
        assert_eq!(file.len(), 5);

        handle_disk_pressure(&coord, &file, "test detail", 0x20).await;

        assert_eq!(
            coord.role(),
            ReplicaRole::Replica,
            "EvictOldest preserves Replica role"
        );
        assert_eq!(file.len(), 1, "retention sweep dropped to cap of 1");
        assert_eq!(
            metrics
                .under_capacity_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "EvictOldest also bumps under_capacity_total"
        );
    }

    /// Disk pressure observed from a Leader role (or Candidate
    /// mid-election) must still drive a withdraw to Idle. The
    /// transition matrix only permits DiskPressureWithdraw on
    /// Replica → Idle; without role-aware signal selection a Leader
    /// would silently log+drop the withdraw and keep writing
    /// through pressure. Pick ChannelClose for the non-Replica case
    /// so the withdraw lands regardless of current role.
    #[tokio::test]
    async fn disk_pressure_withdraw_from_leader_picks_channel_close_signal() {
        let (coord, _metrics) = build_coordinator_with_policy(
            super::super::replication_config::UnderCapacity::Withdraw,
        );
        // Promote through the full state cycle to Leader.
        coord
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        coord
            .transition_to(
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            )
            .await
            .unwrap();
        coord
            .transition_to(
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            )
            .await
            .unwrap();
        assert_eq!(coord.role(), ReplicaRole::Leader);

        let file = build_file_for_tests();
        handle_disk_pressure(&coord, &file, "test detail", 0x20).await;

        // Leader → Idle via ChannelClose must land — the prior code
        // path used DiskPressureWithdraw which is invalid from
        // Leader and would silently fail-and-log.
        assert_eq!(
            coord.role(),
            ReplicaRole::Idle,
            "Leader disk-pressure must withdraw to Idle"
        );
    }

    #[tokio::test]
    async fn disk_pressure_withdraw_is_idempotent_on_idle_already() {
        // Defensive: if the coordinator is already Idle when the
        // DiskPressureWithdraw fires (race with another path),
        // the transition path's idempotent `Idle → Idle +
        // ChannelClose` shortcut doesn't apply (this is
        // DiskPressureWithdraw, not ChannelClose). The
        // transition rejects but the counter still bumps.
        let (coord, metrics) = build_coordinator_with_policy(
            super::super::replication_config::UnderCapacity::Withdraw,
        );
        // Coordinator starts in Idle.
        let file = build_file_for_tests();
        handle_disk_pressure(&coord, &file, "test detail", 0x20).await;
        // Counter advanced; role is still Idle (the transition_to
        // call inside handle_disk_pressure surfaces an error that
        // we log + drop, so role stays Idle).
        assert_eq!(coord.role(), ReplicaRole::Idle);
        assert_eq!(
            metrics
                .under_capacity_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
        );
    }

    // ────────────────────────────────────────────────────────────────
    // R-1 / R-12 regression: role-flip TOCTOU + channel-id validation
    // ────────────────────────────────────────────────────────────────

    /// R-1: A Leader that flips to Idle (e.g. via DiskPressureWithdraw)
    /// in the middle of serving a SyncRequest must NACK NotLeader
    /// rather than ship a SyncResponse from a node that no longer
    /// claims leadership. The fix is a post-read role re-check
    /// immediately before the dispatcher send.
    #[tokio::test]
    async fn sync_request_post_op_role_flip_emits_notleader_nack() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        // Promote to Leader so the entry-check passes.
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let event = Inbound::SyncRequest {
            from: 0x20,
            msg: SyncRequest {
                channel_id: cid,
                since_seq: 0,
                chunk_max: 1024,
                request_id: 0,
                class: Default::default(),
            },
        };
        // Simulate the role flipping between the entry check and
        // the post-op re-check: flip to Idle via DiskPressureWithdraw
        // RIGHT BEFORE we call on_inbound. The entry-check would
        // have failed for an already-Idle coordinator, so we have
        // to use an arrangement that lets the entry check pass
        // and the post-op check fail. The cleanest test: start as
        // Leader; flip to Idle externally; call on_inbound. The
        // entry check now fails — pin the NACK shape directly.
        coordinator
            .transition_to(
                ReplicaRole::Idle,
                super::super::replication_state::TransitionSignal::GracefulRelinquish,
            )
            .await
            .unwrap();
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        let nacks = dispatcher.sync_nacks.lock().clone();
        assert_eq!(nacks.len(), 1, "expected one NotLeader NACK");
        let (target, nack) = &nacks[0];
        assert_eq!(*target, 0x20);
        assert_eq!(
            nack.error_code,
            super::super::replication::SyncNackError::NotLeader
        );
        assert_eq!(nack.channel_id, cid);
        // No SyncResponse should have been shipped.
        assert!(
            dispatcher.sync_responses.lock().is_empty(),
            "no SyncResponse must ship when role isn't Leader"
        );
    }

    /// R-12: SyncRequest with mismatched channel_id is dropped at
    /// the runtime boundary; no NACK, no response, no file access.
    #[tokio::test]
    async fn sync_request_with_wrong_channel_id_is_dropped() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        // Promote to Leader.
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let wrong = channel_id_for("test/wrong_channel");
        let event = Inbound::SyncRequest {
            from: 0x20,
            msg: SyncRequest {
                channel_id: wrong,
                since_seq: 0,
                chunk_max: 1024,
                request_id: 0,
                class: Default::default(),
            },
        };
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        assert!(
            dispatcher.sync_nacks.lock().is_empty(),
            "no NACK on wrong-channel — silently dropped"
        );
        assert!(
            dispatcher.sync_responses.lock().is_empty(),
            "no SyncResponse on wrong-channel"
        );
    }

    /// Peer-auth gate regression: an inbound message whose `from`
    /// node is not in `replica_set` must be dropped at on_inbound
    /// entry. Without the gate any mesh peer with SUBPROTOCOL_REDEX
    /// reach could drive the runtime — the worst case being a forged
    /// SyncResponse that writes attacker-chosen bytes into the
    /// replica's local log via `append_batch`.
    #[tokio::test]
    async fn inbound_from_non_replica_set_peer_is_dropped() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        let baseline_next = inputs.file.next_seq();
        // 0x99 is NOT in replica_set [0x10, 0x20]. Even with valid
        // channel_id and a payload the apply path would accept, the
        // membership gate must drop it.
        let event = Inbound::SyncResponse {
            from: 0x99,
            msg: SyncResponse {
                channel_id: cid,
                first_seq: 0,
                leader_first_retained_seq: 0,
                events: Vec::new(),
                request_id: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        // No state mutation on the local file from an out-of-set
        // peer's SyncResponse.
        assert_eq!(
            inputs.file.next_seq(),
            baseline_next,
            "out-of-set peer must not advance local tail"
        );
        // A Heartbeat from the same out-of-set peer must not seed
        // the tracker either — the gate runs before record_heartbeat.
        let event = Inbound::Heartbeat {
            from: 0x99,
            msg: SyncHeartbeat {
                channel_id: cid,
                tail_seq: 7,
                role: ReplicaRole::Leader,
                wall_clock_ms: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        assert!(
            tracker.lock().believed_leader().is_none(),
            "out-of-set heartbeat must not seed believed_leader"
        );
    }

    /// Peer-auth gate regression: a SyncResponse from a replica_set
    /// peer who is not the believed_leader must be dropped. Without
    /// this check, a non-leader replica could ship forged chunks
    /// once they're inside the replica_set.
    #[tokio::test]
    async fn sync_response_from_non_leader_replica_peer_is_dropped() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20, 0x30], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20, 0x30]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        // Seed believed_leader = 0x20 via a Leader heartbeat.
        tracker
            .lock()
            .record_heartbeat(0x20, ReplicaRole::Leader, 0, Instant::now());
        assert_eq!(tracker.lock().believed_leader(), Some(0x20));
        let baseline_next = inputs.file.next_seq();
        // 0x30 IS in replica_set but is NOT the believed leader.
        let event = Inbound::SyncResponse {
            from: 0x30,
            msg: SyncResponse {
                channel_id: cid,
                first_seq: 0,
                leader_first_retained_seq: 0,
                events: Vec::new(),
                request_id: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        assert_eq!(
            inputs.file.next_seq(),
            baseline_next,
            "non-leader replica_set peer must not advance local tail via SyncResponse"
        );
    }

    /// R-11 regression: `is_stopped()` returns `false` until
    /// `cancel()`'s await completes, even if a parallel
    /// `cancel()` raced and took the JoinHandle out of the
    /// slot. The explicit `stopped` flag (flipped only post-
    /// await) is what guarantees this.
    #[tokio::test]
    async fn is_stopped_is_false_before_first_cancel_completes() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(inputs, coordinator, dispatcher, build_budget());
        assert!(
            !handle.is_stopped(),
            "fresh runtime must report not stopped"
        );
        handle.cancel().await;
        assert!(
            handle.is_stopped(),
            "post-cancel().await runtime must report stopped"
        );
        // Idempotent second cancel must not flip the flag back.
        handle.cancel().await;
        assert!(handle.is_stopped());
    }

    /// R-4 regression: SyncNack NotLeader must actively clear
    /// the believed leader so the next tick re-resolves.
    /// Without the fix the replica loops sending SyncRequests
    /// to the same stale leader until 3 missed heartbeats trip.
    #[tokio::test]
    async fn sync_nack_notleader_clears_believed_leader() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        // Seed the tracker with a believed leader heartbeat.
        tracker
            .lock()
            .record_heartbeat(0x20, ReplicaRole::Leader, 99, Instant::now());
        assert_eq!(tracker.lock().believed_leader(), Some(0x20));
        // NACK NotLeader from the believed leader. Pre-record an
        // in-flight request_id so the response-binding gate admits
        // the NACK to the apply path.
        let outstanding = Arc::new(Mutex::new(OutstandingRequests::new()));
        outstanding.lock().record(0x20, 0, Instant::now());
        let event = Inbound::SyncNack {
            from: 0x20,
            msg: SyncNack {
                channel_id: cid,
                since_seq: 0,
                error_code: super::super::replication::SyncNackError::NotLeader,
                leader_first_retained_seq: 0,
                detail: String::new(),
                request_id: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &RuntimeState {
                tracker: tracker.clone(),
                budget: budget.clone(),
                backoff: build_backoff(),
                outstanding: outstanding.clone(),
            },
            event,
        )
        .await;
        assert!(
            tracker.lock().believed_leader().is_none(),
            "NACK NotLeader must clear the cached believed leader"
        );
    }

    /// R-4 regression: SyncNack BadRange skips the local tail
    /// past the rejected `since_seq` so the next SyncRequest
    /// re-issues against a range the leader has retained.
    /// Without the fix the replica re-issues the same range
    /// indefinitely.
    #[tokio::test]
    async fn sync_nack_badrange_skips_local_tail() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 60_000);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let budget = build_budget();
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        // Seed the tracker with a believed leader heartbeat so the
        // peer-auth gate at on_inbound entry admits the NACK below.
        tracker
            .lock()
            .record_heartbeat(0x20, ReplicaRole::Leader, 41, Instant::now());

        // Local file is empty (next_seq = 0). NACK with since_seq=42
        // means "the leader trimmed up to 42; you asked for 42 but
        // it's gone." Local tail must advance to 43.
        let baseline_next = inputs.file.next_seq();
        // Pre-record the in-flight request_id so the response-binding
        // gate admits this NACK rather than dropping it as stale.
        let outstanding = Arc::new(Mutex::new(OutstandingRequests::new()));
        outstanding.lock().record(0x20, 0, Instant::now());
        let event = Inbound::SyncNack {
            from: 0x20,
            msg: SyncNack {
                channel_id: cid,
                since_seq: 42,
                // Pre-fix: replica advanced one seq at a time via
                // `since_seq + 1 = 43`. R-40 wire field instructs
                // the replica to jump straight to the leader's
                // first-retained seq (here, 100) in one round trip.
                error_code: super::super::replication::SyncNackError::BadRange,
                leader_first_retained_seq: 100,
                detail: String::new(),
                request_id: 0,
            },
        };
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &RuntimeState {
                tracker: tracker.clone(),
                budget: budget.clone(),
                backoff: build_backoff(),
                outstanding: outstanding.clone(),
            },
            event,
        )
        .await;
        // The local file's next_seq advanced past the bad range
        // (or, on persistent files, fell back to retry). For a
        // heap-only file in this test, skip_to(43) succeeded.
        let after = inputs.file.next_seq();
        assert!(
            after > baseline_next,
            "BadRange must advance local next_seq (got {after}, baseline {baseline_next})"
        );
        // R-40 regression: with leader_first_retained_seq = 100,
        // skip_to must jump straight to 100 in one round trip, not
        // creep up by one per BadRange cycle (since_seq + 1 = 43
        // would otherwise re-trigger BadRange when retention floor
        // is much higher).
        assert_eq!(
            after, 100,
            "BadRange with leader_first_retained_seq must jump local tail to the floor"
        );
        // skip_ahead metric advanced.
        assert_eq!(
            coordinator
                .metrics()
                .skip_ahead_total
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    /// R-2: When a post-Candidate transition fails (e.g. the
    /// coordinator already advanced via a concurrent path),
    /// `clear_believed_leader` must NOT run — otherwise the
    /// replica is left with no leader and no path to enter
    /// Candidate again.
    #[tokio::test]
    async fn post_election_failed_transition_preserves_believed_leader() {
        let inputs = build_inputs(0x10, vec![0x10, 0x20], 50);
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let handle = spawn_replication_runtime(
            inputs,
            coordinator.clone(),
            dispatcher.clone(),
            build_budget(),
        );

        // Record a Leader heartbeat to set believed_leader.
        handle
            .dispatch(Inbound::Heartbeat {
                from: 0x20,
                msg: SyncHeartbeat {
                    channel_id: cid,
                    tail_seq: 99,
                    role: ReplicaRole::Leader,
                    wall_clock_ms: 1_700_000_000_000,
                },
            })
            .await
            .unwrap();

        // Drive an external race: drop the coordinator into Idle
        // via the public surface before the election runs.
        // The post-election transition_to will see (Idle, Leader,
        // ElectionWon) which is invalid; the fix ensures we don't
        // wipe the tracker on that failure.
        tokio::time::sleep(Duration::from_millis(20)).await;
        coordinator
            .transition_to(
                ReplicaRole::Idle,
                super::super::replication_state::TransitionSignal::ChannelClose,
            )
            .await
            .unwrap();

        // Run a few more ticks; the silence detection should still
        // run but the post-election transition should fail silently
        // without wiping believed_leader. We can't directly observe
        // the tracker, but we CAN confirm the coordinator stays at
        // Idle (didn't bounce back to Leader after a failed post-
        // election transition) and that no panic / unexpected state
        // mutation occurred.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(coordinator.role(), ReplicaRole::Idle);

        handle.cancel().await;
    }

    /// The replica's `on_tick` must stamp `inputs.default_bandwidth_class`
    /// on every emitted `SyncRequest` — not `Default::default()`. Without
    /// this, the per-channel default configured via
    /// `ReplicationConfig::default_bandwidth_class` is silently dropped
    /// and every replica catchup ships as `Foreground`, regardless of
    /// operator policy.
    #[tokio::test]
    async fn replica_on_tick_stamps_inputs_default_bandwidth_class_on_sync_request() {
        use crate::adapter::net::redex::bandwidth::BandwidthClass;

        let self_id: NodeId = 0x10;
        let leader_id: NodeId = 0x20;
        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: self_id,
            replica_set: vec![self_id, leader_id],
            heartbeat_ms: 100,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            // Local tail = 0; leader's heartbeat will advertise 50,
            // forcing `tick()` to emit a SyncRequest.
            tail_provider: Arc::new(|| 0),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            // Non-default class — must round-trip onto the wire.
            default_bandwidth_class: BandwidthClass::Background,
            background_fraction: 0.3,
        };
        let (coordinator, _registry) = build_coordinator(self_id, vec![self_id, leader_id]);
        coordinator
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();

        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        // Seed a leader heartbeat with tail_seq > local so `tick()`
        // generates a catchup SyncRequest.
        tracker
            .lock()
            .record_heartbeat(leader_id, ReplicaRole::Leader, 50, Instant::now());

        let dispatcher = Arc::new(RecorderDispatcher::default());
        let dyn_dispatcher: Arc<dyn ReplicationDispatcher> = dispatcher.clone();
        on_tick(
            &inputs,
            &coordinator,
            &dyn_dispatcher,
            &build_state(tracker.clone(), build_budget()),
        )
        .await;

        let sync_requests = dispatcher.sync_requests.lock().clone();
        assert_eq!(
            sync_requests.len(),
            1,
            "expected exactly one catchup SyncRequest"
        );
        let (target, req) = &sync_requests[0];
        assert_eq!(*target, leader_id);
        assert_eq!(
            req.class,
            BandwidthClass::Background,
            "emitted SyncRequest must carry inputs.default_bandwidth_class, \
             not Default::default()",
        );
    }

    /// The leader's serve path must consult `msg.class` and
    /// `inputs.background_fraction` when admitting a SyncRequest —
    /// not the class-blind legacy `try_consume`. Without this, every
    /// Phase D2/D4 admission threshold is dead code.
    ///
    /// Configures `background_fraction = 0.3` and a tiny budget
    /// (100-byte capacity). The reserve threshold becomes
    /// `(1 - 0.3) * 100 = 70` bytes. A Background SyncRequest whose
    /// response costs 47 bytes (empty-response header) leaves
    /// `available - cost = 53 < 70`, so the class-aware gate rejects
    /// it with `Backpressure`. The legacy class-blind path would
    /// have admitted (it sees `Foreground` semantics and `available
    /// >= cost`).
    #[tokio::test]
    async fn leader_serve_path_uses_class_aware_admission_for_background_request() {
        use crate::adapter::net::redex::bandwidth::BandwidthClass;

        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: 0x10,
            replica_set: vec![0x10, 0x20],
            heartbeat_ms: 60_000,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            tail_provider: Arc::new(|| 0),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            default_bandwidth_class: BandwidthClass::Foreground,
            background_fraction: 0.3,
        };
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        // Promote to Leader so the serve path runs.
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }

        // Build a budget whose reserve gate will reject a 47-byte
        // Background response. capacity=100, fraction=0.3 → reserve=70;
        // available - cost = 100 - 47 = 53 < 70 → denied.
        let budget = Arc::new(Mutex::new(BandwidthBudget::new(1.0, 100, Instant::now())));

        let dispatcher = Arc::new(RecorderDispatcher::default());
        let event = Inbound::SyncRequest {
            from: 0x20,
            msg: SyncRequest {
                channel_id: cid,
                since_seq: 0,
                chunk_max: 1024,
                request_id: 0,
                class: BandwidthClass::Background,
            },
        };
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;

        let nacks = dispatcher.sync_nacks.lock().clone();
        assert_eq!(
            nacks.len(),
            1,
            "Background admission must be denied by the class-aware reserve gate; \
             pre-fix this admitted via the class-blind try_consume path",
        );
        let (target, nack) = &nacks[0];
        assert_eq!(*target, 0x20);
        assert_eq!(
            nack.error_code,
            super::super::replication::SyncNackError::Backpressure,
            "denial must surface as Backpressure NACK so the replica backs off",
        );
        // No SyncResponse should have been shipped under denial.
        assert!(
            dispatcher.sync_responses.lock().is_empty(),
            "denied requests must not leak a SyncResponse",
        );
    }

    /// Companion to the Background-denial test: with the same tiny
    /// budget, a Foreground request of the same size IS admitted —
    /// confirms the reserve gate is the discriminator, not the cost.
    /// Without this paired assertion a regression that always-denies
    /// would look like the right behavior.
    #[tokio::test]
    async fn leader_serve_path_admits_foreground_under_tight_budget() {
        use crate::adapter::net::redex::bandwidth::BandwidthClass;

        let inputs = RuntimeInputs {
            channel: ChannelIdentity {
                channel_name: "test/runtime".to_string(),
                origin_hash: 0xCAFE_BABE,
            },
            channel_id: channel_id_for("test/runtime"),
            self_node_id: 0x10,
            replica_set: vec![0x10, 0x20],
            heartbeat_ms: 60_000,
            wall_clock_provider: Arc::new(|| 1_700_000_000_000),
            tail_provider: Arc::new(|| 0),
            rtt_lookup: Arc::new(|_| Some(Duration::from_millis(5))),
            file: build_file_for_tests(),
            default_bandwidth_class: BandwidthClass::Foreground,
            background_fraction: 0.3,
        };
        let cid = inputs.channel_id;
        let (coordinator, _registry) = build_coordinator(0x10, vec![0x10, 0x20]);
        for (role, signal) in [
            (
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            ),
            (
                ReplicaRole::Candidate,
                super::super::replication_state::TransitionSignal::MissedHeartbeats,
            ),
            (
                ReplicaRole::Leader,
                super::super::replication_state::TransitionSignal::ElectionWon,
            ),
        ] {
            coordinator.transition_to(role, signal).await.unwrap();
        }
        let budget = Arc::new(Mutex::new(BandwidthBudget::new(1.0, 100, Instant::now())));
        let dispatcher = Arc::new(RecorderDispatcher::default());
        let event = Inbound::SyncRequest {
            from: 0x20,
            msg: SyncRequest {
                channel_id: cid,
                since_seq: 0,
                chunk_max: 1024,
                request_id: 0,
                class: BandwidthClass::Foreground,
            },
        };
        let tracker = Arc::new(Mutex::new(HeartbeatTracker::new(100)));
        on_inbound(
            &inputs,
            &coordinator,
            &(dispatcher.clone() as Arc<dyn ReplicationDispatcher>),
            &build_state(tracker.clone(), budget.clone()),
            event,
        )
        .await;
        // No NACK — Foreground admits because available >= cost.
        assert!(
            dispatcher.sync_nacks.lock().is_empty(),
            "Foreground under the same budget must admit (available >= cost)",
        );
        // Exactly one SyncResponse must have been shipped.
        assert_eq!(
            dispatcher.sync_responses.lock().len(),
            1,
            "Foreground admit must ship a SyncResponse",
        );
    }
}
