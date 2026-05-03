//! Superposition during migration — entity on two nodes simultaneously.
//!
//! Wraps `MigrationState` with observational semantics. During migration,
//! an entity exists in superposition until routing "collapses" it to the
//! target node.

use crate::adapter::net::compute::MigrationPhase;
use crate::adapter::net::state::causal::CausalLink;

use super::chain::ContinuityProof;

/// Observational phase of an entity during migration.
///
/// Maps to `MigrationPhase` but with physical semantics:
/// the entity's "wavefunction" spreads, superposes, and collapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperpositionPhase {
    /// Entity exists only on source (pre-snapshot).
    Localized,
    /// Snapshot taken, target restoring. Entity still authoritative on source.
    Spreading,
    /// Both nodes may hold the entity. Superposed state.
    Superposed,
    /// Target caught up. Ready to collapse to single location.
    ReadyToCollapse,
    /// Routing switched. Target is canonical. Source draining.
    Collapsed,
    /// Source cleaned up. Back to single location on target.
    Resolved,
}

impl SuperpositionPhase {
    /// Map from MigrationPhase to SuperpositionPhase.
    pub fn from_migration(phase: MigrationPhase) -> Self {
        match phase {
            MigrationPhase::Snapshot => Self::Localized,
            MigrationPhase::Transfer => Self::Spreading,
            MigrationPhase::Restore => Self::Spreading,
            MigrationPhase::Replay => Self::Superposed,
            MigrationPhase::Cutover => Self::Collapsed,
            MigrationPhase::Complete => Self::Resolved,
        }
    }

    /// Whether the entity currently exists on multiple nodes.
    pub fn is_superposed(self) -> bool {
        matches!(self, Self::Superposed | Self::ReadyToCollapse)
    }

    /// Whether routing has collapsed to a single node.
    pub fn is_collapsed(self) -> bool {
        matches!(self, Self::Collapsed | Self::Resolved)
    }
}

/// Tracks an entity's superposition state during migration.
///
/// Provides observational semantics on top of the mechanical
/// `MigrationState` from L5.
pub struct SuperpositionState {
    /// Entity being migrated.
    origin_hash: u32,
    /// Source node's chain head at snapshot time.
    source_head: CausalLink,
    /// Target node's chain head (advances during replay).
    target_head: CausalLink,
    /// Current superposition phase.
    phase: SuperpositionPhase,
    /// Events observed by source since snapshot.
    source_observed_since: u64,
    /// Events replayed on target.
    target_replayed_through: u64,
}

impl SuperpositionState {
    /// Create a new superposition state when migration begins.
    pub fn new(origin_hash: u32, source_head: CausalLink) -> Self {
        Self {
            origin_hash,
            source_head,
            target_head: source_head, // target starts from same point
            phase: SuperpositionPhase::Localized,
            source_observed_since: 0,
            target_replayed_through: source_head.sequence,
        }
    }

    /// Advance the phase based on migration progress.
    pub fn advance(&mut self, migration_phase: MigrationPhase) {
        self.phase = SuperpositionPhase::from_migration(migration_phase);
    }

    /// Record that source has processed more events since snapshot.
    pub fn source_advanced(&mut self, new_head: CausalLink) {
        self.source_head = new_head;
        self.source_observed_since = new_head
            .sequence
            .saturating_sub(self.target_replayed_through);
    }

    /// Record that target has replayed events.
    pub fn target_replayed(&mut self, new_head: CausalLink) {
        self.target_head = new_head;
        self.target_replayed_through = new_head.sequence;

        // Check if target has caught up to source. Pre-fix this
        // only transitioned from `Superposed`. If `target_replayed`
        // arrived while still in `Spreading` (target catches up
        // before `advance(Replay)` flips the phase to Superposed),
        // the catch-up was observed but the phase was stuck —
        // `target_replayed` is wire-driven, not re-invoked on phase
        // change, so `ReadyToCollapse` never fired and the
        // migration stalled.
        //
        // Transition from either `Spreading` or `Superposed` on
        // catch-up. The other phases (`Localized` before the
        // migration starts, `ReadyToCollapse` / `Collapsed` /
        // `Resolved` after) shouldn't react.
        if self.target_replayed_through >= self.source_head.sequence
            && matches!(
                self.phase,
                SuperpositionPhase::Superposed | SuperpositionPhase::Spreading
            )
        {
            self.phase = SuperpositionPhase::ReadyToCollapse;
        }
    }

    /// Whether the target has caught up to the source.
    pub fn target_caught_up(&self) -> bool {
        self.target_replayed_through >= self.source_head.sequence
    }

    /// Collapse the superposition (routing switches to target).
    pub fn collapse(&mut self) {
        self.phase = SuperpositionPhase::Collapsed;
    }

    /// Mark migration as fully resolved.
    pub fn resolve(&mut self) {
        self.phase = SuperpositionPhase::Resolved;
    }

    /// Generate a continuity proof spanning the migration.
    ///
    /// Proves that the chain is intact from the source's snapshot point
    /// through the target's current head.
    ///
    /// **Hash convention.** `ContinuityProof::verify_against`
    /// computes `compute_parent_hash(event.link, event.payload)` for
    /// the event at `from_seq` / `to_seq` — i.e. the *forward* hash
    /// of the event AT that sequence. A `CausalLink`'s `parent_hash`
    /// field is the forward hash of the *previous* event (event at
    /// `sequence - 1`). So when we use `head.parent_hash` as the
    /// proof's hash, we must point `from_seq` / `to_seq` at
    /// `head.sequence - 1` — that's the event whose forward hash
    /// equals `head.parent_hash`.
    ///
    /// The match-on-min/max pattern below picks the head whose seq
    /// matches the from/to anchor, so that proofs spanning a
    /// target-behind-source case never mix identities (using
    /// target's seq with source's parent_hash would produce a proof
    /// that could never verify).
    ///
    /// Note: head events with `sequence == 0` (genesis) have no
    /// previous event, so the proof anchors at seq=0 and the hash is
    /// the link's `parent_hash` (typically zero / a genesis sentinel).
    /// `verify_against` will fail for such proofs unless the verifier
    /// holds the genesis event — by design, since you can't prove
    /// continuity of a genesis-only chain.
    pub fn continuity_proof(&self) -> ContinuityProof {
        let (lo_head, hi_head) = if self.source_head.sequence <= self.target_head.sequence {
            (&self.source_head, &self.target_head)
        } else {
            (&self.target_head, &self.source_head)
        };
        ContinuityProof {
            origin_hash: self.origin_hash,
            // Anchor at `head.sequence - 1` — that's the event whose
            // forward hash equals `head.parent_hash`. saturating_sub
            // for genesis (seq==0).
            from_seq: lo_head.sequence.saturating_sub(1),
            to_seq: hi_head.sequence.saturating_sub(1),
            from_hash: lo_head.parent_hash,
            to_hash: hi_head.parent_hash,
        }
    }

    /// Get the current phase.
    #[inline]
    pub fn phase(&self) -> SuperpositionPhase {
        self.phase
    }

    /// Get the origin hash.
    #[inline]
    pub fn origin_hash(&self) -> u32 {
        self.origin_hash
    }

    /// Get the source head.
    #[inline]
    pub fn source_head(&self) -> &CausalLink {
        &self.source_head
    }

    /// Get the target head.
    #[inline]
    pub fn target_head(&self) -> &CausalLink {
        &self.target_head
    }

    /// Events the target still needs to replay.
    pub fn replay_gap(&self) -> u64 {
        self.source_head
            .sequence
            .saturating_sub(self.target_replayed_through)
    }
}

impl std::fmt::Debug for SuperpositionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuperpositionState")
            .field("origin", &format!("{:#x}", self.origin_hash))
            .field("phase", &self.phase)
            .field("source_seq", &self.source_head.sequence)
            .field("target_seq", &self.target_head.sequence)
            .field("replay_gap", &self.replay_gap())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_link(origin: u32, seq: u64) -> CausalLink {
        CausalLink {
            origin_hash: origin,
            horizon_encoded: 0,
            sequence: seq,
            parent_hash: seq * 1000, // deterministic for testing
        }
    }

    #[test]
    fn test_lifecycle() {
        let source_head = make_link(0xAAAA, 100);
        let mut state = SuperpositionState::new(0xAAAA, source_head);

        assert_eq!(state.phase(), SuperpositionPhase::Localized);
        assert!(!state.phase().is_superposed());

        // Migration starts
        state.advance(MigrationPhase::Transfer);
        assert_eq!(state.phase(), SuperpositionPhase::Spreading);

        state.advance(MigrationPhase::Replay);
        assert_eq!(state.phase(), SuperpositionPhase::Superposed);
        assert!(state.phase().is_superposed());

        // Source advances while target replays
        state.source_advanced(make_link(0xAAAA, 105));
        assert_eq!(state.replay_gap(), 5);
        assert!(!state.target_caught_up());

        // Target catches up
        state.target_replayed(make_link(0xAAAA, 105));
        assert!(state.target_caught_up());
        assert_eq!(state.phase(), SuperpositionPhase::ReadyToCollapse);

        // Collapse
        state.collapse();
        assert!(state.phase().is_collapsed());

        state.resolve();
        assert_eq!(state.phase(), SuperpositionPhase::Resolved);
    }

    /// Pin: `target_replayed` must transition from `Spreading`
    /// to `ReadyToCollapse` when the target catches up before
    /// the migration phase has been advanced to Replay. Pre-fix
    /// the transition only fired from `Superposed`; if the
    /// target catch-up signal arrived while still in
    /// `Spreading` (target replayed faster than the
    /// orchestrator's phase advance), the migration stalled —
    /// `target_replayed` is wire-driven and isn't re-invoked
    /// when `advance(Replay)` later flips the phase.
    #[test]
    fn target_replayed_can_transition_from_spreading() {
        let source_head = make_link(0xAAAA, 100);
        let mut state = SuperpositionState::new(0xAAAA, source_head);

        // Migration starts; phase is Spreading (Transfer).
        state.advance(MigrationPhase::Transfer);
        assert_eq!(state.phase(), SuperpositionPhase::Spreading);

        // Target catches up before the orchestrator advances
        // to Replay.
        state.target_replayed(make_link(0xAAAA, 100));
        assert!(state.target_caught_up());
        // Pre-fix this stayed at `Spreading`. Post-fix it
        // transitions directly to `ReadyToCollapse`.
        assert_eq!(
            state.phase(),
            SuperpositionPhase::ReadyToCollapse,
            "target_replayed must transition Spreading → ReadyToCollapse \
             when target catches up; pre-fix this stuck at Spreading"
        );
    }

    #[test]
    fn test_continuity_proof() {
        let source_head = make_link(0xAAAA, 100);
        let mut state = SuperpositionState::new(0xAAAA, source_head);

        state.source_advanced(make_link(0xAAAA, 110));
        state.target_replayed(make_link(0xAAAA, 105));

        let proof = state.continuity_proof();
        assert_eq!(proof.origin_hash, 0xAAAA);
        // The proof anchors at `head.sequence - 1` so
        // the verifier (`compute_parent_hash` of the event AT that
        // seq) sees the same hash bytes that `head.parent_hash`
        // carries. Pre-fix the seqs were 105 / 110 and the hashes
        // were head.parent_hash — a mismatch the verifier would
        // always reject.
        assert_eq!(proof.from_seq, 104);
        assert_eq!(proof.to_seq, 109);
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #99: the proof
    /// produced by `SuperpositionState::continuity_proof` must
    /// actually verify against an `EntityLog` that contains the
    /// matching events. Pre-fix the seqs and hashes pointed to
    /// different events so verification always failed; pre-fix
    /// also mixed identities when target's seq < source's seq.
    ///
    /// We pin the round-trip:
    ///   1. Build a chain of events in an EntityLog (using the
    ///      same `CausalChainBuilder` pattern the chain.rs tests
    ///      use, so parent_hash linkage is structurally correct).
    ///   2. Build a SuperpositionState whose head links carry the
    ///      same `parent_hash` values the chain produced.
    ///   3. Generate the proof and verify it against the log.
    ///   4. Pre-fix: verify_against fails with `HashMismatch`.
    ///      Post-fix: Ok.
    #[test]
    fn continuity_proof_round_trips_through_entity_log() {
        use crate::adapter::net::identity::EntityKeypair;
        use crate::adapter::net::state::causal::{compute_parent_hash, CausalChainBuilder};
        use crate::adapter::net::state::EntityLog;
        use bytes::Bytes;

        let kp = EntityKeypair::generate();
        let origin = kp.origin_hash();
        let mut log = EntityLog::new(kp.entity_id().clone());
        let mut builder = CausalChainBuilder::new(origin);

        // Build a 5-event chain. Each event's parent_hash is the
        // forward hash of the prior event (link + payload).
        for i in 0..5usize {
            let event = builder
                .append(Bytes::from(format!("event-{}", i)), 0)
                .unwrap();
            log.append(event).unwrap();
        }

        // Source's head is event[3]; target replayed up to event[2].
        let events: Vec<_> = log.range(1, 5).into_iter().cloned().collect();
        assert_eq!(
            events.len(),
            5,
            "got {} events: {:?}",
            events.len(),
            events.iter().map(|e| e.link.sequence).collect::<Vec<_>>()
        );
        let source_head_link = events[3].link;
        let target_head_link = events[2].link;

        let mut state = SuperpositionState::new(origin, source_head_link);
        state.target_replayed(target_head_link);

        let proof = state.continuity_proof();

        // The proof's anchors are seq-1 of each head, since
        // head.parent_hash = forward hash of (head.sequence - 1).
        assert_eq!(proof.origin_hash, origin);
        let lo_seq = source_head_link.sequence.min(target_head_link.sequence);
        let hi_seq = source_head_link.sequence.max(target_head_link.sequence);
        assert_eq!(proof.from_seq, lo_seq.saturating_sub(1));
        assert_eq!(proof.to_seq, hi_seq.saturating_sub(1));

        // Round-trip: verify against the log.
        // Pre-fix: this fails with HashMismatch because the proof's
        // hashes are head.parent_hash but its from_seq/to_seq pointed
        // at the heads themselves, so the verifier hashed event at
        // from_seq and got a different value than head.parent_hash
        // (which is the hash of the event at from_seq - 1).
        proof
            .verify_against(&log)
            .expect("post-fix proof must verify against the log it was derived from");

        // Sanity-check the hash bytes: from_hash should equal
        // compute_parent_hash of event at from_seq.
        let event_at_from = log
            .range(proof.from_seq, proof.from_seq)
            .into_iter()
            .next()
            .expect("event at from_seq must exist");
        assert_eq!(
            proof.from_hash,
            compute_parent_hash(&event_at_from.link, &event_at_from.payload),
            "from_hash must match the forward hash of event at from_seq"
        );
    }

    /// CR-33: pin the genesis-edge documented limitation. A head
    /// with `sequence == 0` (the genesis event) produces a proof
    /// where `from_seq == 0` and `from_hash == parent_hash`
    /// (typically zero / a genesis sentinel). `verify_against`
    /// will fail unless the verifier holds the genesis event
    /// — by design, since you can't prove continuity of a
    /// genesis-only chain.
    ///
    /// More subtly: a head with `sequence == 1` ALSO produces
    /// `from_seq == 0` (via `saturating_sub(1)`). After ANY
    /// snapshot-prune that removes the genesis event, the
    /// resulting proof is unverifiable for the seq-1 head until
    /// the head advances past the prune anchor.
    ///
    /// This test pins both the genesis edge AND the seq-1 edge
    /// so a future maintainer touching the `saturating_sub(1)`
    /// either preserves the documented behavior or updates this
    /// test to reflect a new contract.
    #[test]
    fn cr33_continuity_proof_at_genesis_and_seq_one_edge_cases() {
        // Edge 1: head at exactly seq=0 (genesis-only chain).
        let genesis_head = make_link(0xCAFE, 0);
        let state = SuperpositionState::new(0xCAFE, genesis_head);
        let proof = state.continuity_proof();
        assert_eq!(
            proof.from_seq, 0,
            "genesis head: saturating_sub(1) yields 0 (CR-33 documented)"
        );
        assert_eq!(proof.to_seq, 0, "genesis head: from_seq == to_seq == 0");
        // The proof's hash is whatever genesis's parent_hash carries
        // — typically 0 (genesis sentinel). `verify_against` against
        // a genesis-only log MIGHT succeed if event[0]'s forward
        // hash happens to equal `genesis_head.parent_hash`, but
        // typically does not — by design.

        // Edge 2: head at seq=1 (one event past genesis). Same
        // saturating_sub(1) collapse as genesis.
        let seq1_head = make_link(0xBEEF, 1);
        let state = SuperpositionState::new(0xBEEF, seq1_head);
        let proof = state.continuity_proof();
        assert_eq!(
            proof.from_seq, 0,
            "CR-33: head at seq=1 produces from_seq=0 — same as genesis. \
             After ANY snapshot-prune that removes seq=0, the resulting \
             proof becomes unverifiable. Documented limitation: heads \
             must advance past the prune anchor before producing \
             verifiable proofs."
        );
        assert_eq!(proof.to_seq, 0);
    }

    #[test]
    fn test_auto_ready_to_collapse() {
        let source_head = make_link(0xAAAA, 50);
        let mut state = SuperpositionState::new(0xAAAA, source_head);

        state.advance(MigrationPhase::Replay);
        assert_eq!(state.phase(), SuperpositionPhase::Superposed);

        // Target catches up while in Superposed phase
        state.target_replayed(make_link(0xAAAA, 50));
        assert_eq!(state.phase(), SuperpositionPhase::ReadyToCollapse);
    }

    #[test]
    fn test_replay_gap() {
        let source_head = make_link(0xAAAA, 100);
        let mut state = SuperpositionState::new(0xAAAA, source_head);

        assert_eq!(state.replay_gap(), 0); // target starts at same point

        state.source_advanced(make_link(0xAAAA, 120));
        assert_eq!(state.replay_gap(), 20);

        state.target_replayed(make_link(0xAAAA, 110));
        assert_eq!(state.replay_gap(), 10);
    }
}
