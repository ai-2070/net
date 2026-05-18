//! `ReplicationCoordinator` core — Phase C slice of
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §3.
//!
//! One coordinator per replicated channel per replica. Holds the
//! validated [`ReplicaRole`] state, the channel's chain identity,
//! the per-channel [`ChannelMetricsAtomic`] handle, and a
//! `ChainTagSink` abstraction over [`MeshNode::announce_chain`] /
//! [`MeshNode::withdraw_chain`] so the coordinator's tag-lifecycle
//! discipline is unit-testable without spinning a real mesh.
//!
//! This slice covers the **state-machine + tag-lifecycle + metrics**
//! seam. The heartbeat loop, FSM-driven `elect()` triggering, and
//! `Redex::open_file` spawn integration land in subsequent slices
//! per the plan §3 / §6 / §7.
//!
//! State-machine transitions route through
//! [`StateTransition::apply`] (`replication_state.rs`) so the
//! coordinator can't accidentally advance a `(from, to, signal)`
//! triple the plan §3 doesn't enumerate. Capability-tag
//! emission / withdrawal is keyed to specific transitions per the
//! plan §3 Responsibilities:
//!
//! | Transition          | Tag side-effect                          |
//! |---------------------|------------------------------------------|
//! | `Idle → Replica`    | `announce_chain(tail_seq)` advertises hold |
//! | `Replica → Leader`  | re-`announce_chain(tail_seq)` (new role)  |
//! | `Candidate → Leader`| re-`announce_chain(tail_seq)` (new role)  |
//! | `* → Idle`          | `withdraw_chain` retracts the holder      |
//!
//! Metrics increment on every transition per `replication_metrics.rs`:
//! `leader_changes_total` on any transition INTO `Leader`,
//! `election_thrash_total` on `MissedHeartbeats` transitions within
//! the 30 s window (window enforcement in the heartbeat-loop slice).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};

use super::replication::ReplicaRole;
use super::replication_config::ReplicationConfig;
use super::replication_metrics::{ChannelMetricsAtomic, ReplicationMetricsRegistry};
use super::replication_state::{StateTransition, StateTransitionError, TransitionSignal};
use crate::adapter::net::MeshNode;
use crate::error::AdapterError;

/// Mesh-side surface the coordinator depends on for chain-tag
/// advertisement + withdrawal. Implemented by [`MeshNode`] in the
/// substrate and by a mock in unit tests.
///
/// Async / Send-bound so the coordinator can be driven from a
/// tokio task without forcing every implementor into a specific
/// runtime.
#[async_trait::async_trait]
pub trait ChainTagSink: Send + Sync {
    /// Advertise this node holds `origin_hash` up to `tip_seq`.
    /// Idempotent — repeated calls with the same `origin_hash`
    /// replace the prior advertisement.
    async fn announce_chain(&self, origin_hash: u64, tip_seq: u64) -> Result<(), AdapterError>;

    /// Withdraw every advertisement for `origin_hash`.
    /// Idempotent.
    async fn withdraw_chain(&self, origin_hash: u64) -> Result<(), AdapterError>;
}

/// Substrate impl: route through [`MeshNode::announce_chain`] /
/// [`MeshNode::withdraw_chain`]. This is the production sink the
/// [`ReplicationCoordinator`] uses when a real [`MeshNode`] is
/// wired in.
#[async_trait::async_trait]
impl ChainTagSink for MeshNode {
    async fn announce_chain(&self, origin_hash: u64, tip_seq: u64) -> Result<(), AdapterError> {
        MeshNode::announce_chain(self, origin_hash, tip_seq).await
    }

    async fn withdraw_chain(&self, origin_hash: u64) -> Result<(), AdapterError> {
        MeshNode::withdraw_chain(self, origin_hash).await
    }
}

/// Lifecycle event a [`ReplicaTransitionObserver`] receives
/// when this coordinator transitions through its state
/// machine. Carries `origin_hash` so observers managing many
/// coordinators (one per channel) can route the event.
///
/// `at` is the monotonic timestamp at the transition. Plain data so observers
/// can buffer / async-forward without lifetime issues.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ReplicaTransitionEvent {
    /// Coordinator entered `Replica` or `Leader` from `Idle` —
    /// this node is now a holder of the chain.
    BecameHolder {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
    /// Coordinator entered `Idle` from any non-Idle state —
    /// this node is no longer a holder.
    Idled {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
    /// Leader changed for this channel — the coordinator
    /// transitioned through a Leader entry (`Replica → Leader`
    /// or `Candidate → Leader`). MeshOS uses this to update
    /// `MeshOsState::replica_leader`.
    LeaderChanged {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
    /// This coordinator stepped down from `Leader` to `Replica`
    /// — the node remains a holder but is no longer leader.
    /// MeshOS clears its mirror of
    /// `MeshOsState::replica_leader[origin_hash]` when the
    /// observer sees this — otherwise the loop would carry a
    /// stale leader pointer until a different node's
    /// `LeaderChanged` overwrites it.
    LeaderLost {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
    /// This coordinator stepped down from `Leader` straight to
    /// `Idle` — the node is no longer leader AND no longer a
    /// holder. The two transitions are bundled into one event so
    /// downstream sinks publish a single atomic update; firing
    /// `Idled` and `LeaderLost` separately would let the events
    /// channel drop one half under backpressure, leaving the
    /// snapshot with either a phantom leader on a non-holder, or
    /// a leader-less holder set.
    LeaderLostAndIdled {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
    /// Symmetric to [`Self::LeaderLostAndIdled`] for the
    /// promotion side: this coordinator entered `Leader` directly
    /// from `Idle`, so the node BOTH became a holder AND became
    /// leader in one transition. Bundled into one event so a
    /// downstream sink can't drop half of the (holder add,
    /// leader set) pair under backpressure — pre-bundle the
    /// observer fired `BecameHolder` then `LeaderChanged` as two
    /// `try_publish`es, and a `QueueFull` between them left the
    /// snapshot with a holder set but no leader (or vice versa).
    /// `Replica → Leader` / `Candidate → Leader` still fire only
    /// `LeaderChanged` because the node was already a holder.
    BecameHolderAndLeader {
        /// Substrate-level chain identifier.
        origin_hash: u64,
        /// Monotonic timestamp of the transition.
        at: Instant,
    },
}

/// Observer hook for replication-coordinator state changes.
/// Implementations fan events out to whichever consumer wants
/// them — the MeshOS event loop being the canonical
/// near-term consumer. Methods are sync + non-blocking.
pub trait ReplicaTransitionObserver: Send + Sync + 'static {
    /// Receive one transition event. Must not block.
    fn observe(&self, event: ReplicaTransitionEvent);
}

/// Errors the coordinator surfaces from its state-machine + tag-
/// lifecycle path.
#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    /// State-machine validator rejected a transition.
    #[error("invalid state transition: {0}")]
    Transition(#[from] StateTransitionError),
    /// `MeshNode::announce_chain` / `withdraw_chain` surfaced an
    /// error. The state mutation already happened — the operator
    /// observes a divergence between local state and advertised
    /// state until the next successful announce.
    #[error("chain-tag side-effect failed: {0}")]
    TagSink(#[source] AdapterError),
}

/// Stable identity for a replicated channel. The
/// [`ReplicationCoordinator`] holds one of these per channel +
/// replica role; the metrics registry keys per-channel counters on
/// the `channel_name` string. The `origin_hash` is the substrate-
/// level identifier the capability layer's `causal:<hex>` tag
/// carries.
#[derive(Debug, Clone)]
pub struct ChannelIdentity {
    /// Human-readable name of the replicated channel (used as the
    /// metrics label).
    pub channel_name: String,
    /// Substrate-level chain identifier — passed to
    /// [`ChainTagSink::announce_chain`] / [`ChainTagSink::withdraw_chain`].
    pub origin_hash: u64,
}

/// One replication coordinator. Phase C scope:
///
/// - Holds the [`ReplicaRole`] cell under a parking_lot mutex
///   (microsecond critical sections; no async needed inside).
/// - Holds the local `tail_seq` (`AtomicU64`).
/// - Holds the channel-identity + config + sink + metrics handle.
/// - Transitions go through [`Self::transition_to`] which validates
///   via [`StateTransition::apply`], emits / withdraws capability
///   tags via the sink, and increments metrics.
///
/// Heartbeat loop, election triggering, and spawn lifecycle land
/// in the next slices.
pub struct ReplicationCoordinator {
    channel: ChannelIdentity,
    config: ReplicationConfig,
    sink: Arc<dyn ChainTagSink>,
    metrics: Arc<ChannelMetricsAtomic>,
    state: Mutex<ReplicaRole>,
    tail_seq: AtomicU64,
    /// Serializes the entire `transition_to` body — state update +
    /// metric bumps + chain-tag side effect — so two racing
    /// transitions can't interleave announce/withdraw against the
    /// capability layer. Plan §3 pins the announce/withdraw key to
    /// specific transitions; without this lock T1 could set
    /// `Replica` + queue `announce_chain` while T2 sets `Idle` +
    /// completes `withdraw_chain` first, leaving the mesh
    /// advertising a chain we've already withdrawn locally.
    transition_lock: tokio::sync::Mutex<()>,
    /// Optional observer hook. When set, every successful state
    /// transition that crosses the Idle ↔ {Replica, Leader}
    /// boundary fires through it so consumers (MeshOS, audit,
    /// dashboard) see a coherent replica-update stream.
    ///
    /// `parking_lot::RwLock` over `Option<Arc<dyn ...>>` mirrors
    /// the hot-path router pattern used elsewhere (e.g.
    /// `DaemonRegistry::observer`): uncontended read on the
    /// firing path, rare write when an observer is installed.
    observer: RwLock<Option<Arc<dyn ReplicaTransitionObserver>>>,
}

impl ReplicationCoordinator {
    /// Construct a coordinator in [`ReplicaRole::Idle`]. The
    /// caller transitions it to `Replica` once the placement
    /// filter has selected this node — `transition_to(Replica,
    /// CapabilitySelected)`. Validate the [`ReplicationConfig`]
    /// before calling; an invalid config produces undefined
    /// transition behavior (the coordinator doesn't re-validate).
    pub fn new(
        channel: ChannelIdentity,
        config: ReplicationConfig,
        sink: Arc<dyn ChainTagSink>,
        registry: &ReplicationMetricsRegistry,
    ) -> Self {
        let metrics = registry.for_channel(&channel.channel_name);
        Self {
            channel,
            config,
            sink,
            metrics,
            state: Mutex::new(ReplicaRole::Idle),
            tail_seq: AtomicU64::new(0),
            transition_lock: tokio::sync::Mutex::new(()),
            observer: RwLock::new(None),
        }
    }

    /// Install a replica-transition observer. Replaces any prior
    /// observer; returns the prior one if any. Pass `None` to
    /// detach. Lock-free on the firing path; only the install
    /// path takes the write lock.
    pub fn set_transition_observer(
        &self,
        observer: Option<Arc<dyn ReplicaTransitionObserver>>,
    ) -> Option<Arc<dyn ReplicaTransitionObserver>> {
        let mut guard = self.observer.write();
        std::mem::replace(&mut *guard, observer)
    }

    /// `true` when an observer is installed. Cheap (one RwLock
    /// read).
    pub fn has_transition_observer(&self) -> bool {
        self.observer.read().is_some()
    }

    fn fire_transition(&self, event: ReplicaTransitionEvent) {
        if let Some(observer) = self.observer.read().clone() {
            observer.observe(event);
        }
    }

    /// Read the coordinator's current state. Snapshot — the value
    /// may change immediately after the lock releases.
    pub fn role(&self) -> ReplicaRole {
        *self.state.lock()
    }

    /// Read the local `tail_seq`. The coordinator advances this
    /// via [`Self::record_tail_seq`] as appends land.
    pub fn tail_seq(&self) -> u64 {
        self.tail_seq.load(Ordering::Relaxed)
    }

    /// Record the local `tail_seq`. Monotonic — calls with a value
    /// `<=` the current tail are dropped. The heartbeat-loop slice
    /// uses this to keep the gauge fresh; Phase D pull-based
    /// catch-up advances it per applied `SYNC_RESPONSE` chunk.
    pub fn record_tail_seq(&self, seq: u64) {
        let mut current = self.tail_seq.load(Ordering::Relaxed);
        while seq > current {
            match self.tail_seq.compare_exchange_weak(
                current,
                seq,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => current = now,
            }
        }
    }

    /// Channel identity (read-only — fixed at construction time).
    pub fn channel(&self) -> &ChannelIdentity {
        &self.channel
    }

    /// Replication config (read-only).
    pub fn config(&self) -> &ReplicationConfig {
        &self.config
    }

    /// Per-channel metrics handle. Exposed so the heartbeat loop
    /// and sync path can increment counters without re-resolving
    /// through the registry on every event.
    pub fn metrics(&self) -> &ChannelMetricsAtomic {
        &self.metrics
    }

    /// Attempt to transition to `target` driven by `signal`.
    /// Validates the `(from, to, signal)` triple through
    /// [`StateTransition::apply`]; on success:
    ///
    /// 1. Updates the state cell to `target`.
    /// 2. Performs the documented capability-tag side-effect:
    ///    - `Idle → Replica`, `Replica → Leader`, `Candidate →
    ///      Leader`: `announce_chain(tail_seq)` so peers see this
    ///      node as a holder (or new leader).
    ///    - `* → Idle`: `withdraw_chain` retracts the
    ///      advertisement.
    ///    - All other valid transitions (e.g. `Candidate → Replica`)
    ///      are state-only; the holder advertisement already
    ///      reflects "replica."
    /// 3. Increments the appropriate metric.
    ///
    /// Returns:
    /// - `Ok(Some(StateTransition))` — transition applied; the
    ///   `StateTransition` is the validated triple, useful for
    ///   logging.
    /// - `Ok(None)` — `target == current_state` AND `signal ==
    ///   ChannelClose` (the idempotent shutdown shape); state
    ///   unchanged, no side-effect, no metric bump.
    /// - `Err(CoordinatorError::Transition)` — the triple is
    ///   invalid (state unchanged).
    /// - `Err(CoordinatorError::TagSink)` — state IS updated; the
    ///   tag-sink call failed. Caller logs + retries on the next
    ///   heartbeat tick.
    pub async fn transition_to(
        &self,
        target: ReplicaRole,
        signal: TransitionSignal,
    ) -> Result<Option<StateTransition>, CoordinatorError> {
        // R-3: hold a single async mutex across the whole
        // transition (state update + metric bumps + chain-tag
        // side effect) so two concurrent callers can't interleave
        // an `announce_chain` from a stale role over a
        // `withdraw_chain` from a fresher one. The inner state
        // mutex still serializes the validation + cell flip; the
        // outer transition_lock serializes the side-effect chain.
        let _guard = self.transition_lock.lock().await;
        // Acquire the state lock for the validation + cell update.
        // Drop it before the await — the sink call is async and
        // we don't want to hold a sync mutex across an await
        // point.
        let transition = {
            let mut state = self.state.lock();
            let from = *state;
            // `ChannelClose` to Idle from an already-Idle state is
            // a no-op idempotent shutdown — short-circuit without
            // touching the sink or metrics.
            if from == ReplicaRole::Idle
                && target == ReplicaRole::Idle
                && signal == TransitionSignal::ChannelClose
            {
                return Ok(None);
            }
            let t = StateTransition::apply(from, target, signal)?;
            *state = target;
            t
        };

        // Metric bumps. Done eagerly so even if the sink call
        // fails, the operator-facing counter reflects the state
        // change that actually happened.
        if transition.to == ReplicaRole::Leader {
            self.metrics.incr_leader_change();
        }
        if matches!(transition.signal, TransitionSignal::MissedHeartbeats) {
            // The election-thrash 30-s window is enforced by the
            // heartbeat loop; this counter just records every
            // MissedHeartbeats-driven transition. The aggregator
            // upstream collapses thrash via the timestamp series.
            self.metrics.incr_election_thrash();
        }

        // Side-effect on the chain-tag layer. Plan §3 pins
        // emission to exactly two transitions:
        //
        //   - `Idle → Replica`          (capability filter selected)
        //   - `Candidate → Leader`      (won the election)
        //
        // Other valid transitions stay in the "already advertising"
        // window — `Replica → Candidate` and `Candidate → Replica`
        // don't change the holder advertisement (the tag layer
        // doesn't distinguish leader-from-replica; that's a wire-
        // protocol role byte on the heartbeat). Withdrawal happens
        // on every `* → Idle`.
        let origin = self.channel.origin_hash;
        let is_withdraw = transition.to == ReplicaRole::Idle;
        let result = match (transition.from, transition.to) {
            (ReplicaRole::Idle, ReplicaRole::Replica)
            | (ReplicaRole::Candidate, ReplicaRole::Leader) => {
                let tip = self.tail_seq.load(Ordering::Relaxed);
                self.sink.announce_chain(origin, tip).await
            }
            (_, ReplicaRole::Idle) => self.sink.withdraw_chain(origin).await,
            _ => Ok(()),
        };
        if let Err(e) = result {
            if is_withdraw {
                // Local state already flipped to Idle but the
                // mesh-side withdraw failed — the mesh may still
                // advertise this node as a chain holder until
                // something else trips a re-announce. Bump the
                // divergence counter so operators can spot the
                // gap; recovery is opportunistic on the next
                // transition_to call.
                self.metrics.incr_announce_divergence();
                tracing::warn!(
                    origin = format!("{:#x}", origin),
                    from = ?transition.from,
                    error = %e,
                    "replication coordinator: state advanced to Idle but sink withdraw failed; \
                     advertised-vs-local divergence until next transition_to or cancel()",
                );
            }
            return Err(CoordinatorError::TagSink(e));
        }

        // Fire the observer AFTER the sink call succeeds. If the
        // sink call fails we don't fire — the state mutation
        // already happened, but the operator-visible advertisement
        // didn't; the next heartbeat cycle will retry and the
        // observer fires then. (The transition_lock serializes
        // both, so a retried `transition_to` runs the full chain
        // again including the observer.)
        let at = Instant::now();
        match (transition.from, transition.to) {
            (ReplicaRole::Idle, ReplicaRole::Replica) => {
                self.fire_transition(ReplicaTransitionEvent::BecameHolder {
                    origin_hash: origin,
                    at,
                });
            }
            // Idle → Leader: bundle the (holder add, leader set)
            // pair so a backpressured sink can't drop one half and
            // leave the snapshot with a phantom holder or leader.
            (ReplicaRole::Idle, ReplicaRole::Leader) => {
                self.fire_transition(ReplicaTransitionEvent::BecameHolderAndLeader {
                    origin_hash: origin,
                    at,
                });
            }
            // Leader → Idle: bundle the (holder removal, leader
            // clear) pair, symmetric to BecameHolderAndLeader above.
            (ReplicaRole::Leader, ReplicaRole::Idle) => {
                self.fire_transition(ReplicaTransitionEvent::LeaderLostAndIdled {
                    origin_hash: origin,
                    at,
                });
            }
            (_, ReplicaRole::Idle) => {
                self.fire_transition(ReplicaTransitionEvent::Idled {
                    origin_hash: origin,
                    at,
                });
            }
            _ => {}
        }
        // Replica → Leader / Candidate → Leader: already a holder,
        // so only the leader bit changes. Idle → Leader is handled
        // atomically above by BecameHolderAndLeader.
        if matches!(
            (transition.from, transition.to),
            (ReplicaRole::Replica, ReplicaRole::Leader)
                | (ReplicaRole::Candidate, ReplicaRole::Leader)
        ) {
            self.fire_transition(ReplicaTransitionEvent::LeaderChanged {
                origin_hash: origin,
                at,
            });
        }
        // Leader → Replica step-down — node remains a holder but
        // is no longer leader. The Leader → Idle case is handled
        // above by `LeaderLostAndIdled`.
        if matches!(
            (transition.from, transition.to),
            (ReplicaRole::Leader, ReplicaRole::Replica)
        ) {
            self.fire_transition(ReplicaTransitionEvent::LeaderLost {
                origin_hash: origin,
                at,
            });
        }

        Ok(Some(transition))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex as ParkingMutex;

    /// Recorder mock — captures every `announce_chain` /
    /// `withdraw_chain` call and lets the test assert on the
    /// observed sequence.
    #[derive(Default)]
    struct RecorderSink {
        calls: ParkingMutex<Vec<SinkCall>>,
        /// When set, every announce/withdraw returns this error
        /// instead of `Ok(())`. Lets tests pin the "state mutated
        /// but tag-sink failed" path.
        fail_next: ParkingMutex<Option<AdapterError>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum SinkCall {
        Announce { origin_hash: u64, tip_seq: u64 },
        Withdraw { origin_hash: u64 },
    }

    impl RecorderSink {
        fn calls(&self) -> Vec<SinkCall> {
            self.calls.lock().clone()
        }

        fn arm_failure(&self, err: AdapterError) {
            *self.fail_next.lock() = Some(err);
        }
    }

    #[async_trait::async_trait]
    impl ChainTagSink for RecorderSink {
        async fn announce_chain(&self, origin_hash: u64, tip_seq: u64) -> Result<(), AdapterError> {
            if let Some(err) = self.fail_next.lock().take() {
                return Err(err);
            }
            self.calls.lock().push(SinkCall::Announce {
                origin_hash,
                tip_seq,
            });
            Ok(())
        }

        async fn withdraw_chain(&self, origin_hash: u64) -> Result<(), AdapterError> {
            if let Some(err) = self.fail_next.lock().take() {
                return Err(err);
            }
            self.calls.lock().push(SinkCall::Withdraw { origin_hash });
            Ok(())
        }
    }

    fn build_coordinator() -> (
        Arc<RecorderSink>,
        ReplicationMetricsRegistry,
        ReplicationCoordinator,
    ) {
        let sink = Arc::new(RecorderSink::default());
        let registry = ReplicationMetricsRegistry::new();
        let coordinator = ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "payments/settlements".to_string(),
                origin_hash: 0xCAFE_BABE_DEAD_BEEF,
            },
            ReplicationConfig::new(),
            sink.clone() as Arc<dyn ChainTagSink>,
            &registry,
        );
        (sink, registry, coordinator)
    }

    #[tokio::test]
    async fn starts_in_idle_with_zero_tail() {
        let (_, _, c) = build_coordinator();
        assert_eq!(c.role(), ReplicaRole::Idle);
        assert_eq!(c.tail_seq(), 0);
    }

    #[tokio::test]
    async fn idle_to_replica_announces_chain() {
        let (sink, _, c) = build_coordinator();
        c.record_tail_seq(42);
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .expect("valid transition");
        assert_eq!(c.role(), ReplicaRole::Replica);
        assert_eq!(
            sink.calls(),
            vec![SinkCall::Announce {
                origin_hash: 0xCAFE_BABE_DEAD_BEEF,
                tip_seq: 42,
            }],
        );
    }

    #[tokio::test]
    async fn candidate_to_leader_announces_chain() {
        let (sink, _, c) = build_coordinator();
        // Idle → Replica → Candidate → Leader
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.record_tail_seq(999);
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
        assert_eq!(c.role(), ReplicaRole::Leader);
        // Two announces total: one for Replica entry, one for
        // Leader entry. Candidate is transient — no announce.
        let calls = sink.calls();
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], SinkCall::Announce { tip_seq: 0, .. }));
        assert!(matches!(calls[1], SinkCall::Announce { tip_seq: 999, .. }));
    }

    #[tokio::test]
    async fn candidate_does_not_emit_tag_side_effect() {
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        // Replica → Candidate: state-only; no tag emission.
        let baseline = sink.calls().len();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        assert_eq!(sink.calls().len(), baseline, "Candidate must not emit tags");
    }

    #[tokio::test]
    async fn candidate_to_replica_no_tag_side_effect() {
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        let baseline = sink.calls().len();
        // Losing election: Candidate → Replica. No new tag emission
        // — already advertising "replica" via the prior announce.
        c.transition_to(ReplicaRole::Replica, TransitionSignal::ElectionLost)
            .await
            .unwrap();
        assert_eq!(
            sink.calls().len(),
            baseline,
            "Candidate→Replica should not double-announce"
        );
    }

    #[tokio::test]
    async fn leader_to_idle_withdraws_chain() {
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Idle, TransitionSignal::GracefulRelinquish)
            .await
            .unwrap();
        let calls = sink.calls();
        let last = calls.last().expect("at least one call");
        assert_eq!(
            *last,
            SinkCall::Withdraw {
                origin_hash: 0xCAFE_BABE_DEAD_BEEF,
            },
            "graceful relinquish must withdraw the chain tag",
        );
    }

    #[tokio::test]
    async fn replica_to_idle_disk_pressure_withdraws() {
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Idle, TransitionSignal::DiskPressureWithdraw)
            .await
            .unwrap();
        let calls = sink.calls();
        assert_eq!(
            *calls.last().unwrap(),
            SinkCall::Withdraw {
                origin_hash: 0xCAFE_BABE_DEAD_BEEF,
            },
        );
    }

    #[tokio::test]
    async fn channel_close_from_idle_is_idempotent_noop() {
        let (sink, registry, c) = build_coordinator();
        let result = c
            .transition_to(ReplicaRole::Idle, TransitionSignal::ChannelClose)
            .await
            .unwrap();
        assert!(result.is_none(), "idempotent close must return None");
        // No tag side-effect, no metric bump.
        assert!(sink.calls().is_empty());
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.channels.len(), 1);
        let c_metrics = &snapshot.channels[0];
        assert_eq!(c_metrics.leader_changes_total, 0);
        assert_eq!(c_metrics.election_thrash_total, 0);
    }

    #[tokio::test]
    async fn channel_close_from_active_state_withdraws() {
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Idle, TransitionSignal::ChannelClose)
            .await
            .unwrap();
        let calls = sink.calls();
        assert!(matches!(*calls.last().unwrap(), SinkCall::Withdraw { .. }));
    }

    #[tokio::test]
    async fn invalid_transition_does_not_mutate_state() {
        let (sink, _, c) = build_coordinator();
        // Idle → Leader is not in the matrix.
        let err = c
            .transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .expect_err("Idle→Leader must reject");
        assert!(matches!(err, CoordinatorError::Transition(_)));
        assert_eq!(c.role(), ReplicaRole::Idle, "state must not advance");
        assert!(sink.calls().is_empty(), "no side-effect on rejection");
    }

    #[tokio::test]
    async fn record_tail_seq_is_monotonic() {
        let (_, _, c) = build_coordinator();
        c.record_tail_seq(100);
        assert_eq!(c.tail_seq(), 100);
        c.record_tail_seq(50); // monotonic; drop
        assert_eq!(c.tail_seq(), 100);
        c.record_tail_seq(200);
        assert_eq!(c.tail_seq(), 200);
    }

    #[tokio::test]
    async fn metric_increments_on_leader_entry() {
        let (_, registry, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
        let snap = registry.snapshot();
        let row = snap.channel("payments/settlements").unwrap();
        assert_eq!(row.leader_changes_total, 1);
        assert_eq!(row.election_thrash_total, 1, "MissedHeartbeats triggered");
    }

    #[tokio::test]
    async fn metric_increments_on_repeat_leader_entries() {
        // Simulate leader bounce: Replica → Candidate → Leader →
        // Idle (channel close from leader: actually GracefulRelinquish)
        // → Replica → Candidate → Leader. Counter must be 2.
        let (_, registry, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Idle, TransitionSignal::GracefulRelinquish)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Candidate, TransitionSignal::MissedHeartbeats)
            .await
            .unwrap();
        c.transition_to(ReplicaRole::Leader, TransitionSignal::ElectionWon)
            .await
            .unwrap();
        let snap = registry.snapshot();
        let row = snap.channel("payments/settlements").unwrap();
        assert_eq!(row.leader_changes_total, 2);
    }

    /// R-3 regression: concurrent `transition_to` calls must
    /// serialize their chain-tag side effects. A delaying sink
    /// lets us pin that T1's announce_chain and T2's
    /// withdraw_chain don't interleave — the observed call
    /// sequence is exactly one of the two complete orderings
    /// (announce-then-withdraw or withdraw-then-announce), never
    /// a torn one.
    #[tokio::test]
    async fn concurrent_transitions_serialize_chain_tag_side_effects() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        /// Sink that holds a barrier — every announce/withdraw
        /// awaits the barrier (so if the coordinator didn't
        /// serialize calls, both would block at the barrier
        /// concurrently). Without the transition_lock the test
        /// would observe `in_flight > 1` at some point; the
        /// regression assertion catches that.
        struct BarrierSink {
            calls: tokio::sync::Mutex<Vec<SinkCall>>,
            in_flight: AtomicUsize,
            max_in_flight: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl ChainTagSink for BarrierSink {
            async fn announce_chain(
                &self,
                origin_hash: u64,
                tip_seq: u64,
            ) -> Result<(), AdapterError> {
                let n = self.in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                self.max_in_flight.fetch_max(n, AtomicOrdering::SeqCst);
                // Give other tasks a chance to interleave if the
                // transition_lock isn't holding them off.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                self.calls.lock().await.push(SinkCall::Announce {
                    origin_hash,
                    tip_seq,
                });
                self.in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
                Ok(())
            }
            async fn withdraw_chain(&self, origin_hash: u64) -> Result<(), AdapterError> {
                let n = self.in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                self.max_in_flight.fetch_max(n, AtomicOrdering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                self.calls
                    .lock()
                    .await
                    .push(SinkCall::Withdraw { origin_hash });
                self.in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
                Ok(())
            }
        }

        let sink = Arc::new(BarrierSink {
            calls: tokio::sync::Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        let registry = ReplicationMetricsRegistry::new();
        let coord = Arc::new(ReplicationCoordinator::new(
            ChannelIdentity {
                channel_name: "concurrent/serialize".to_string(),
                origin_hash: 0xC0FFEE,
            },
            ReplicationConfig::new(),
            sink.clone() as Arc<dyn ChainTagSink>,
            &registry,
        ));

        // Drive concurrent transitions: T1 wants Idle→Replica
        // (announce); T2 racing on the same coordinator. Since
        // only one transition is valid at a time, T2 races by
        // doing Replica→Idle right after T1 announces. The
        // transition_lock ensures T2's withdraw can't START
        // before T1's announce COMPLETES.
        let c1 = coord.clone();
        let t1 = tokio::spawn(async move {
            c1.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
                .await
        });
        let c2 = coord.clone();
        let t2 = tokio::spawn(async move {
            // Yield then drive the withdraw transition. The lock
            // serializes: if T1 holds the transition_lock, T2's
            // state-mutex acquire only happens after T1's full
            // body (sink call included) finishes.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            c2.transition_to(ReplicaRole::Idle, TransitionSignal::DiskPressureWithdraw)
                .await
        });
        let (r1, r2) = tokio::join!(t1, t2);
        r1.unwrap().expect("T1 transition succeeds");
        r2.unwrap().expect("T2 transition succeeds");

        let max_concurrent = sink.max_in_flight.load(AtomicOrdering::SeqCst);
        assert_eq!(
            max_concurrent, 1,
            "transition_lock must serialize sink calls (observed max in-flight = {max_concurrent})"
        );

        // Sanity: both side-effects landed in announce-then-
        // withdraw order (T1's announce came first because the
        // lock made T2 wait).
        let calls = sink.calls.lock().await.clone();
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], SinkCall::Announce { .. }));
        assert!(matches!(calls[1], SinkCall::Withdraw { .. }));
    }

    #[tokio::test]
    async fn tag_sink_failure_surfaces_but_state_mutated() {
        // Plan §3 pin: "On graceful shutdown, transition to Idle
        // and withdraw the replica's `causal:` tag." If the
        // withdraw fails, the state still advances to Idle (the
        // coordinator can't undo the role change just because the
        // network blip happened). Operator observes a divergence
        // between local state and advertised state until the next
        // tick retries.
        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        sink.arm_failure(AdapterError::Transient(
            "simulated network blip".to_string(),
        ));
        let err = c
            .transition_to(ReplicaRole::Idle, TransitionSignal::DiskPressureWithdraw)
            .await
            .expect_err("must surface sink failure");
        assert!(matches!(err, CoordinatorError::TagSink(_)));
        // State HAS advanced — withdraw "happened locally" even
        // though the wire side missed.
        assert_eq!(c.role(), ReplicaRole::Idle);
    }

    /// Pin that a `* → Idle` sink failure bumps
    /// `announce_divergence_total` on the channel's metrics so
    /// operators see the gap between local state and advertised
    /// holder set. Recovery is opportunistic on the next
    /// `transition_to` call; the counter is the observability
    /// surface for the window in between.
    #[tokio::test]
    async fn tag_sink_failure_bumps_divergence_counter() {
        use std::sync::atomic::Ordering as AtomicOrdering;

        let (sink, _, c) = build_coordinator();
        c.transition_to(ReplicaRole::Replica, TransitionSignal::CapabilitySelected)
            .await
            .unwrap();
        let before = c
            .metrics()
            .announce_divergence_total
            .load(AtomicOrdering::Relaxed);

        sink.arm_failure(AdapterError::Transient(
            "simulated network blip".to_string(),
        ));
        let _ = c
            .transition_to(ReplicaRole::Idle, TransitionSignal::DiskPressureWithdraw)
            .await
            .expect_err("must surface sink failure");

        let after = c
            .metrics()
            .announce_divergence_total
            .load(AtomicOrdering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "announce_divergence_total must bump by exactly 1 on the failed withdraw"
        );
    }
}
