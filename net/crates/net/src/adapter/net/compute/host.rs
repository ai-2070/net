//! DaemonHost — runtime wrapper for a MeshDaemon.
//!
//! Owns the causal infrastructure (chain builder, horizon) and wraps
//! daemon outputs in CausalLinks. The daemon only sees events and
//! produces payloads — all chain management is the host's job.

use std::sync::Arc;

use dashmap::DashMap;

use super::bindings::{DaemonBindings, SubscriptionBinding};
use super::daemon::{DaemonError, DaemonHostConfig, DaemonStats, MeshDaemon};
use crate::adapter::net::behavior::capability::CapabilityFilter;
use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::state::causal::{CausalChainBuilder, CausalEvent, CausalLink};
use crate::adapter::net::state::horizon::ObservedHorizon;
use crate::adapter::net::state::snapshot::StateSnapshot;

/// Runtime wrapper for a `MeshDaemon`.
///
/// Manages the daemon's causal chain, observed horizon, and snapshot lifecycle.
/// Each `DaemonHost` has its own `EntityKeypair` — its identity in the mesh.
pub struct DaemonHost {
    /// The daemon implementation.
    daemon: Box<dyn MeshDaemon>,
    /// Daemon's identity in the mesh.
    keypair: EntityKeypair,
    /// Produces causally-linked output events.
    chain: CausalChainBuilder,
    /// Tracks what this daemon has observed from other entities.
    horizon: ObservedHorizon,
    /// Host configuration.
    config: DaemonHostConfig,
    /// Runtime statistics.
    stats: DaemonStats,
    /// Per-daemon subscription ledger. Populated by
    /// [`Self::record_subscription`] / [`Self::forget_subscription`]
    /// when the daemon asks to subscribe or unsubscribe from a
    /// channel; read by [`Self::take_snapshot`] to serialize into
    /// `StateSnapshot::bindings_bytes` so the migration target can
    /// replay the subscriptions. See
    /// [`DAEMON_CHANNEL_REBIND_PLAN.md`](../../../../docs/DAEMON_CHANNEL_REBIND_PLAN.md).
    ///
    /// `Arc<DashMap>` because the host itself lives under a
    /// registry-level `Mutex` (for single-threaded `process()`
    /// ordering) but the ledger is updated from the SDK's
    /// subscribe / unsubscribe path which runs outside that lock.
    subscriptions: Arc<DashMap<(u64, ChannelName), SubscriptionBinding>>,
}

impl DaemonHost {
    /// Create a new host with a genesis chain.
    pub fn new(
        daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
        config: DaemonHostConfig,
    ) -> Self {
        let chain = CausalChainBuilder::new(keypair.origin_hash());
        Self {
            daemon,
            keypair,
            chain,
            horizon: ObservedHorizon::new(),
            config,
            stats: DaemonStats::default(),
            subscriptions: Arc::new(DashMap::new()),
        }
    }

    /// Create a host from a fork.
    ///
    /// Uses a pre-built `CausalChainBuilder` from `fork_entity()` whose
    /// genesis link carries the fork sentinel as `parent_hash`. The daemon
    /// starts fresh (no state to restore) but its chain documents lineage.
    ///
    /// Validates that the chain's origin_hash matches the keypair to prevent
    /// identity/chain mismatches.
    pub fn from_fork(
        daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
        chain: CausalChainBuilder,
        config: DaemonHostConfig,
    ) -> Self {
        assert_eq!(
            chain.origin_hash(),
            keypair.origin_hash(),
            "fork chain origin {:#x} does not match keypair origin {:#x}",
            chain.origin_hash(),
            keypair.origin_hash(),
        );
        Self {
            daemon,
            keypair,
            chain,
            horizon: ObservedHorizon::new(),
            config,
            stats: DaemonStats::default(),
            subscriptions: Arc::new(DashMap::new()),
        }
    }

    /// Restore from an L4 `StateSnapshot`.
    ///
    /// Rebuilds the causal chain from the snapshot's head link and calls
    /// `daemon.restore()` with the serialized state. Any subscriptions
    /// encoded in `snapshot.bindings_bytes` are parsed into the host's
    /// ledger so the migration-target replay path can re-subscribe the
    /// daemon on the target node before cutover fires. A malformed
    /// `bindings_bytes` payload aborts the restore — attacker-controlled
    /// data must not slip past into the daemon's operational state.
    pub fn from_snapshot(
        mut daemon: Box<dyn MeshDaemon>,
        keypair: EntityKeypair,
        snapshot: &StateSnapshot,
        config: DaemonHostConfig,
    ) -> Result<Self, DaemonError> {
        // Validate snapshot belongs to this keypair
        if snapshot.entity_id != *keypair.entity_id() {
            return Err(DaemonError::RestoreFailed(format!(
                "snapshot entity {:?} does not match keypair entity {:?}",
                snapshot.entity_id,
                keypair.entity_id()
            )));
        }

        // Restore daemon state
        daemon.restore(snapshot.state.clone())?;

        // Rebuild chain from snapshot head. Use the head event's
        // payload (NOT `snapshot.state`, which is the daemon's
        // serialized state — a different thing entirely). The next
        // event's `parent_hash` is `xxh3(prev_link_bytes ++ prev_payload)`,
        // and any third-party validator that derives `prev_payload`
        // from the actual head event would mismatch us if we used
        // `snapshot.state` here.
        //
        // `head_payload` is a runtime-only field on the snapshot (not
        // on the wire). Callers populate it from the head event
        // before invoking restore. If it's empty (e.g. a snapshot
        // deserialized from wire bytes without out-of-band payload
        // transfer), we fall back to `snapshot.state` to preserve
        // the prior behavior — but this fallback only works when
        // the source side made the same choice.
        //
        // `head_payload` is `Option<Bytes>`. `Some(bytes)` is the
        // legitimate "caller populated the head event payload" path;
        // `None` is the unambiguous "context missing" sentinel — an
        // empty-`Bytes` sentinel would conflate empty payloads with
        // missing context. For genesis snapshots (sequence == 0) a
        // missing payload is fine — there's no predecessor. For
        // non-genesis with no payload, fall back to snapshot.state
        // and warn loudly.
        let head_payload = match &snapshot.head_payload {
            Some(payload) => payload.clone(),
            None => {
                if snapshot.chain_link.sequence > 0 {
                    tracing::warn!(
                        sequence = snapshot.chain_link.sequence,
                        entity_id = ?snapshot.entity_id,
                        "DaemonHost::from_snapshot: head_payload not populated for \
                         non-genesis snapshot — falling back to snapshot.state which \
                         only validates against subsequent events if the source side \
                         made the same choice. Production callers MUST populate \
                         head_payload via `StateSnapshot::with_head_payload` before \
                         passing to from_snapshot."
                    );
                }
                snapshot.state.clone()
            }
        };
        let chain = CausalChainBuilder::from_head(snapshot.chain_link, head_payload);

        // Rehydrate the subscription ledger. Empty bytes → empty
        // ledger; malformed bytes → reject. The migration-target
        // handler consults this after construction to drive the
        // re-bind replay.
        let subscriptions = Arc::new(DashMap::new());
        if !snapshot.bindings_bytes.is_empty() {
            let bindings =
                DaemonBindings::from_bytes(&snapshot.bindings_bytes).ok_or_else(|| {
                    DaemonError::RestoreFailed(
                        "snapshot bindings_bytes failed to decode — tampered or corrupt snapshot"
                            .into(),
                    )
                })?;
            for sub in bindings.subscriptions {
                subscriptions.insert((sub.publisher, sub.channel.clone()), sub);
            }
        }

        Ok(Self {
            daemon,
            keypair,
            chain,
            horizon: snapshot.horizon.clone(),
            config,
            stats: DaemonStats::default(),
            subscriptions,
        })
    }

    /// Deliver an inbound causal event to the daemon.
    ///
    /// Updates the observed horizon, calls `daemon.process()`, and wraps
    /// any outputs in CausalLinks via the chain builder.
    ///
    /// Returns the wrapped output events (ready to send on the mesh).
    pub fn deliver(&mut self, event: &CausalEvent) -> Result<Vec<CausalEvent>, DaemonError> {
        // Update horizon with what we've observed
        self.horizon
            .observe(event.link.origin_hash, event.link.sequence);

        // Process the event
        let outputs = match self.daemon.process(event) {
            Ok(outputs) => outputs,
            Err(e) => {
                self.stats.errors += 1;
                return Err(e);
            }
        };

        self.stats.events_processed += 1;

        // Wrap each output payload in a causal link
        let horizon_encoded = self.horizon.encode();
        let mut causal_outputs = Vec::with_capacity(outputs.len());
        for payload in outputs {
            let event = self
                .chain
                .append(payload, horizon_encoded)
                .ok_or_else(|| DaemonError::ProcessFailed("causal sequence overflow".into()))?;
            self.stats.events_emitted += 1;
            causal_outputs.push(event);
        }

        Ok(causal_outputs)
    }

    /// Take a snapshot of the daemon's current state.
    ///
    /// Returns `None` if the daemon is stateless (`snapshot()` returns `None`).
    /// The returned snapshot carries a frozen view of the subscription
    /// ledger in its `bindings_bytes` field so the migration target
    /// can replay subscriptions during Restore. An empty ledger serializes
    /// to an empty byte slice — no wire overhead for daemons that never
    /// subscribed.
    pub fn take_snapshot(&self) -> Option<StateSnapshot> {
        let state = self.daemon.snapshot()?;
        let mut snapshot = StateSnapshot::new(
            self.keypair.entity_id().clone(),
            *self.chain.head(),
            state,
            self.horizon.clone(),
        );
        let bindings = self.bindings_snapshot();
        if !bindings.is_empty() {
            snapshot.bindings_bytes = bindings.to_bytes();
        }
        Some(snapshot)
    }

    /// Restore this host's daemon state and chain head from a
    /// snapshot taken on another daemon (typically the active in a
    /// standby group). Unlike [`from_snapshot`], this mutates an
    /// existing host in place — the keypair stays put, only the
    /// daemon-state bytes and chain head are updated.
    ///
    /// Used by `StandbyGroup::sync_standbys` to actually copy state
    /// from the active onto each standby; previously sync only
    /// updated bookkeeping and standbys stayed in their initial
    /// default-constructed state, so a promoted standby lost
    /// everything that had happened before the most recent sync.
    ///
    /// [`from_snapshot`]: Self::from_snapshot
    pub fn restore_from_snapshot(&mut self, snapshot: &StateSnapshot) -> Result<(), DaemonError> {
        // Push the daemon's state across.
        self.daemon.restore(snapshot.state.clone())?;

        // Rebuild the chain head so the next event this host
        // produces (or validates) extends from the snapshot's
        // head_link with the right `parent_hash`. Same fallback
        // logic as `from_snapshot` for the runtime-only
        // `head_payload`.
        //
        // See `from_snapshot` for full rationale.
        // `head_payload: Option<Bytes>` distinguishes legitimate
        // empty payloads from missing context.
        let head_payload = match &snapshot.head_payload {
            Some(payload) => payload.clone(),
            None => {
                if snapshot.chain_link.sequence > 0 {
                    tracing::warn!(
                        sequence = snapshot.chain_link.sequence,
                        entity_id = ?snapshot.entity_id,
                        "DaemonHost::restore_from_snapshot: head_payload not populated \
                         for non-genesis snapshot — falling back to snapshot.state. \
                         Production callers MUST populate head_payload via \
                         `StateSnapshot::with_head_payload`."
                    );
                }
                snapshot.state.clone()
            }
        };
        self.chain = CausalChainBuilder::from_head(snapshot.chain_link, head_payload);
        self.horizon = snapshot.horizon.clone();
        Ok(())
    }

    /// Record a subscription in the daemon's ledger.
    ///
    /// Called by the SDK's `DaemonRuntime::subscribe_channel` path
    /// after the membership-subscribe Ack returns successfully. The
    /// ledger is the authoritative view of what the daemon has
    /// subscribed to; migration reads from here, not from the
    /// mesh's per-node subscriber roster.
    ///
    /// Re-recording the same `(publisher, channel)` pair replaces
    /// the token (tokens refresh), but keeps the subscription
    /// single-entry — no duplicates in the ledger.
    pub fn record_subscription(
        &self,
        publisher: u64,
        channel: ChannelName,
        token_bytes: Option<Vec<u8>>,
    ) {
        let binding = SubscriptionBinding {
            publisher,
            channel: channel.clone(),
            token_bytes,
        };
        self.subscriptions.insert((publisher, channel), binding);
    }

    /// Drop a subscription from the ledger. Idempotent.
    pub fn forget_subscription(&self, publisher: u64, channel: &ChannelName) {
        self.subscriptions.remove(&(publisher, channel.clone()));
    }

    /// Number of subscriptions in the ledger.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    /// Snapshot of the subscription ledger — a cloned view for
    /// migration / diagnostic readers. Order is insertion-ish but
    /// DashMap doesn't guarantee stable iteration, so the target
    /// replay path treats the list as a set.
    pub fn bindings_snapshot(&self) -> DaemonBindings {
        DaemonBindings {
            subscriptions: self
                .subscriptions
                .iter()
                .map(|e| e.value().clone())
                .collect(),
        }
    }

    /// Get the daemon's entity ID.
    #[inline]
    pub fn entity_id(&self) -> &crate::adapter::net::identity::EntityId {
        self.keypair.entity_id()
    }

    /// Get the daemon's origin hash.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.keypair.origin_hash()
    }

    /// Read-only access to the daemon's keypair.
    ///
    /// Migration uses this to seal the daemon's ed25519 seed into
    /// an [`IdentityEnvelope`](crate::adapter::net::identity::IdentityEnvelope)
    /// before shipping the snapshot. The keypair may be public-only
    /// (see [`EntityKeypair::is_read_only`]) — sealing a public-only
    /// keypair is a logic error handled by
    /// [`IdentityEnvelope::new`](crate::adapter::net::identity::IdentityEnvelope::new),
    /// not here.
    #[inline]
    pub fn keypair(&self) -> &EntityKeypair {
        &self.keypair
    }

    /// Get the daemon's capability requirements.
    #[inline]
    pub fn requirements(&self) -> CapabilityFilter {
        self.daemon.requirements()
    }

    /// Get the daemon's name.
    #[inline]
    pub fn name(&self) -> &str {
        self.daemon.name()
    }

    /// Get the current chain sequence number.
    #[inline]
    pub fn sequence(&self) -> u64 {
        self.chain.sequence()
    }

    /// Get the current causal-chain head link.
    ///
    /// Returns the link of the most-recently-applied event:
    /// `(origin_hash, horizon_encoded, sequence, parent_hash)`.
    /// The `parent_hash` is the forward hash of the event at
    /// `sequence - 1`, i.e. the cryptographic anchor that
    /// continuity proofs verify against.
    ///
    /// Used by the migration orchestrator's
    /// `on_replay_complete` to stamp the *real* `parent_hash`
    /// into the superposition's `target_head` instead of a
    /// synthetic `0`. A synthetic `parent_hash: 0` produces a
    /// `ContinuityProof` that no downstream verifier holding the
    /// real chain can ever reconcile.
    #[inline]
    pub fn head_link(&self) -> CausalLink {
        *self.chain.head()
    }

    /// Get runtime statistics.
    #[inline]
    pub fn stats(&self) -> &DaemonStats {
        &self.stats
    }

    /// Get the daemon host configuration.
    #[inline]
    pub fn config(&self) -> &DaemonHostConfig {
        &self.config
    }
}

impl std::fmt::Debug for DaemonHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonHost")
            .field("name", &self.daemon.name())
            .field("origin_hash", &format!("{:#x}", self.origin_hash()))
            .field("sequence", &self.sequence())
            .field("stats", &self.stats)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::state::causal::CausalLink;
    use bytes::Bytes;

    /// A simple stateless echo daemon for testing.
    struct EchoDaemon;

    impl MeshDaemon for EchoDaemon {
        fn name(&self) -> &str {
            "echo"
        }

        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }

        fn process(&mut self, event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            // Echo the payload back
            Ok(vec![event.payload.clone()])
        }
    }

    /// A stateful counter daemon for testing.
    struct CounterDaemon {
        count: u64,
    }

    impl CounterDaemon {
        fn new() -> Self {
            Self { count: 0 }
        }
    }

    impl MeshDaemon for CounterDaemon {
        fn name(&self) -> &str {
            "counter"
        }

        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }

        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            self.count += 1;
            Ok(vec![Bytes::from(self.count.to_le_bytes().to_vec())])
        }

        fn snapshot(&self) -> Option<Bytes> {
            Some(Bytes::from(self.count.to_le_bytes().to_vec()))
        }

        fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
            if state.len() != 8 {
                return Err(DaemonError::RestoreFailed("bad state size".into()));
            }
            self.count = u64::from_le_bytes(state[..8].try_into().unwrap());
            Ok(())
        }
    }

    fn make_event(origin: u64, seq: u64, payload: &[u8]) -> CausalEvent {
        CausalEvent {
            link: CausalLink {
                origin_hash: origin,
                horizon_encoded: 0,
                sequence: seq,
                parent_hash: 0,
            },
            payload: Bytes::copy_from_slice(payload),
            received_at: 0,
        }
    }

    #[test]
    fn test_echo_daemon() {
        let kp = EntityKeypair::generate();
        let mut host = DaemonHost::new(Box::new(EchoDaemon), kp, DaemonHostConfig::default());

        let event = make_event(0xAAAA, 1, b"hello");
        let outputs = host.deliver(&event).unwrap();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].payload, Bytes::from_static(b"hello"));
        assert_eq!(outputs[0].link.sequence, 1); // first output
        assert_eq!(host.stats().events_processed, 1);
        assert_eq!(host.stats().events_emitted, 1);
    }

    #[test]
    fn test_counter_daemon() {
        let kp = EntityKeypair::generate();
        let mut host = DaemonHost::new(
            Box::new(CounterDaemon::new()),
            kp,
            DaemonHostConfig::default(),
        );

        for i in 1..=5 {
            let event = make_event(0xBBBB, i, b"tick");
            let outputs = host.deliver(&event).unwrap();
            assert_eq!(outputs.len(), 1);

            let count = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
            assert_eq!(count, i);
        }

        assert_eq!(host.sequence(), 5);
        assert_eq!(host.stats().events_processed, 5);
    }

    #[test]
    fn test_stateless_snapshot_is_none() {
        let kp = EntityKeypair::generate();
        let host = DaemonHost::new(Box::new(EchoDaemon), kp, DaemonHostConfig::default());

        assert!(host.take_snapshot().is_none());
    }

    #[test]
    fn test_stateful_snapshot_and_restore() {
        let kp = EntityKeypair::generate();
        let mut host = DaemonHost::new(
            Box::new(CounterDaemon::new()),
            kp.clone(),
            DaemonHostConfig::default(),
        );

        // Process some events
        for i in 1..=10 {
            let event = make_event(0xCCCC, i, b"tick");
            host.deliver(&event).unwrap();
        }

        // Take snapshot
        let snapshot = host.take_snapshot().unwrap();
        assert_eq!(snapshot.through_seq, 10);

        // Restore on a new host
        let kp2 = kp.clone();
        let mut restored = DaemonHost::from_snapshot(
            Box::new(CounterDaemon::new()),
            kp2,
            &snapshot,
            DaemonHostConfig::default(),
        )
        .unwrap();

        // Next event should continue counting from 10
        let event = make_event(0xCCCC, 11, b"tick");
        let outputs = restored.deliver(&event).unwrap();
        let count = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
        assert_eq!(count, 11);
    }

    #[test]
    fn test_chain_continuity_across_events() {
        let kp = EntityKeypair::generate();
        let mut host = DaemonHost::new(Box::new(EchoDaemon), kp, DaemonHostConfig::default());

        let mut prev_link = None;
        for i in 1..=5 {
            let event = make_event(0xDDDD, i, b"data");
            let outputs = host.deliver(&event).unwrap();

            let link = outputs[0].link;
            assert_eq!(link.sequence, i);
            assert_eq!(link.origin_hash, host.origin_hash());

            if let Some(prev) = prev_link {
                // parent_hash should link to previous
                assert_ne!(link.parent_hash, 0);
                assert_ne!(link.parent_hash, prev);
            }
            prev_link = Some(link.parent_hash);
        }
    }

    #[test]
    fn test_horizon_updated_before_process() {
        let kp = EntityKeypair::generate();
        let mut host = DaemonHost::new(Box::new(EchoDaemon), kp, DaemonHostConfig::default());

        let event = make_event(0xEEEE, 42, b"test");
        let outputs = host.deliver(&event).unwrap();

        // Output should carry horizon info about the observed event
        assert_ne!(outputs[0].link.horizon_encoded, 0);
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_from_snapshot_rejects_wrong_keypair() {
        // Regression: from_snapshot accepted any snapshot regardless of
        // entity identity, allowing chain/identity divergence.
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        // Create snapshot for entity A
        let chain = CausalChainBuilder::new(kp_a.origin_hash());
        let snapshot = StateSnapshot::new(
            kp_a.entity_id().clone(),
            *chain.head(),
            Bytes::from_static(b"state"),
            ObservedHorizon::new(),
        );

        // Try to restore on entity B — must fail
        let result = DaemonHost::from_snapshot(
            Box::new(EchoDaemon),
            kp_b,
            &snapshot,
            DaemonHostConfig::default(),
        );
        assert!(
            result.is_err(),
            "from_snapshot must reject snapshot from a different entity"
        );
    }
}
