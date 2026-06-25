//! The partition-safe `→ Active` commit driver (plan §6 / Phase D),
//! composing [`QuorumWitness`] + [`FenceLedger`] + the reservation
//! fold into the one CP edge in the whole protocol.
//!
//! A leader holding an island's `Reserved` may transition it to
//! `Active` (start irreversible GPU compute) **only** once a strict
//! majority of the island's replica set acks the commit — and each
//! replica acks only if its [`FenceLedger`] accepts the proposal's
//! epoch. The minority side of a partition reaches only its own
//! replicas, never a majority, so it never starts compute; a stale
//! ex-leader proposing at a superseded epoch is refused by every
//! replica that already witnessed the newer term.
//!
//! Partition is modeled with a `reachable` replica subset: the
//! co-located [`ReplicaCohort`] holds every replica's fence, and a
//! proposer can only solicit acks from the replicas on its side of
//! the split. In production each fence lives on its own node behind
//! `ColocationStrict` (§5) and an ack is a LAN-local RPC; the logic
//! is identical, which is why it is tested in-process here.

use std::collections::HashMap;

use crate::adapter::net::behavior::fold::{ApplyOutcome, IslandId, JobId, NodeId};

use super::claim::{activate_announcement, ClaimError, Claimant};
use super::quorum::{Epoch, FenceLedger, QuorumWitness, ReplicaSet};

/// Outcome of a partition-safe `→ Active` commit attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveCommitOutcome {
    /// A strict majority acked and the `Active` was applied to the
    /// reservation fold — the job may start compute.
    Committed,
    /// Not enough fence-accepting replicas were reachable (minority
    /// partition, or the epoch was fenced as stale). The island stays
    /// `Reserved`; NO compute started — the whole point of the gate.
    NoQuorum {
        /// Distinct fence-accepting acks gathered.
        acks: usize,
        /// Acks a strict majority would have needed.
        needed: usize,
    },
    /// Quorum was reached but the reservation fold refused the
    /// `Active` — the proposer no longer holds the island's
    /// `Reserved` (a takeover landed between reserve and commit). No
    /// compute started.
    LostReservation,
}

/// The fence ledgers of an island's replica set, co-located for
/// deterministic partition simulation. In production each ledger
/// lives on its own replica node.
#[derive(Debug, Clone)]
pub struct ReplicaCohort {
    fences: HashMap<NodeId, FenceLedger>,
}

impl ReplicaCohort {
    /// A fresh cohort — one empty [`FenceLedger`] per replica.
    pub fn new(members: &[NodeId]) -> Self {
        Self {
            fences: members.iter().map(|&m| (m, FenceLedger::new())).collect(),
        }
    }

    /// One replica's vote on a proposal: fence-check `(island,
    /// epoch)` and, on acceptance, witness it. Returns whether the
    /// replica acked. An unknown replica never acks.
    fn vote(&mut self, replica: NodeId, island: IslandId, epoch: Epoch) -> bool {
        match self.fences.get_mut(&replica) {
            Some(fence) => fence.accept_active(island, epoch),
            None => false,
        }
    }

    /// Highest epoch a given replica has witnessed for `island`
    /// (test/diagnostic).
    pub fn highest_witnessed(&self, replica: NodeId, island: IslandId) -> Epoch {
        self.fences
            .get(&replica)
            .map(|f| f.highest_witnessed(island))
            .unwrap_or(0)
    }
}

/// Attempt the partition-safe `→ Active` commit for one island.
///
/// `reachable` is the subset of `set` the proposer can currently
/// solicit (the proposer's side of a partition; pass all of `set`
/// when fully connected). Each reachable replica fence-checks `epoch`
/// and acks iff it accepts; on a strict-majority quorum the `Active`
/// is applied to `reservations` and [`ActiveCommitOutcome::Committed`]
/// is returned. Otherwise nothing is applied and the island stays
/// `Reserved`.
///
/// The epoch **rides the reservation `generation`** (locked decision
/// 3): the `Active` announcement is signed at `generation = epoch`,
/// so a leadership term and the fold's anti-reorder counter are one
/// and the same number. The proposer must therefore have reserved the
/// island at a generation strictly below `epoch`.
/// `claimant` carries the proposing leader's identity + reservation
/// fold; the `Active` is signed at `generation = epoch` (not the
/// claimant's counter), so the leader must have reserved the island
/// at a generation strictly below `epoch`.
pub fn commit_active(
    claimant: &Claimant,
    cohort: &mut ReplicaCohort,
    set: &ReplicaSet,
    reachable: &[NodeId],
    island: IslandId,
    job_id: JobId,
    epoch: Epoch,
) -> Result<ActiveCommitOutcome, ClaimError> {
    // Gather fence-accepting acks from the reachable replicas only.
    let mut witness = QuorumWitness::new(set);
    for &replica in reachable {
        if cohort.vote(replica, island, epoch) {
            witness.record_ack(replica);
        }
    }
    if !witness.has_quorum() {
        return Ok(ActiveCommitOutcome::NoQuorum {
            acks: witness.ack_count(),
            needed: set.quorum_threshold(),
        });
    }

    // Quorum reached → apply the Active (epoch rides the generation).
    let ann = activate_announcement(claimant.keypair, claimant.node_id, epoch, island, job_id)?;
    match claimant.reservations.apply(ann)? {
        ApplyOutcome::Inserted | ApplyOutcome::Replaced => Ok(ActiveCommitOutcome::Committed),
        // We gathered a quorum but no longer hold the Reserved — a
        // takeover landed first. No compute starts.
        ApplyOutcome::Rejected => Ok(ActiveCommitOutcome::LostReservation),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        Fold, ReservationFold, ReservationQuery, ReservationState,
    };
    use crate::adapter::net::behavior::gang::single_island_claim;
    use crate::adapter::net::current_timestamp_micros;
    use crate::adapter::net::identity::EntityKeypair;

    fn new_reservations() -> Fold<ReservationFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    fn fresh() -> u64 {
        current_timestamp_micros() + 60_000_000
    }

    fn state_of(fold: &Fold<ReservationFold>, island: IslandId) -> ReservationState {
        fold.query(ReservationQuery::State(island))[0].1.clone()
    }

    /// Quorum side: a leader holding the Reserved commits Active when
    /// a strict majority is reachable.
    #[test]
    fn quorum_side_commits_and_applies_active() {
        let fold = new_reservations();
        let set = ReplicaSet::new([1, 2, 3, 4, 5]);
        let mut cohort = ReplicaCohort::new(set.members());
        let leader = EntityKeypair::generate();
        let ln = leader.entity_id().node_id();

        // Leader reserves at gen 1, then commits Active at epoch 2.
        single_island_claim(&fold, &leader, ln, 1, 0xA0, fresh()).unwrap();
        let claimant = Claimant::new(&fold, &leader, ln);
        let out = commit_active(
            &claimant,
            &mut cohort,
            &set,
            &[1, 2, 3], // majority reachable
            0xA0,
            7, // job
            2, // epoch (> reserve gen 1)
        )
        .unwrap();
        assert_eq!(out, ActiveCommitOutcome::Committed);
        assert!(matches!(
            state_of(&fold, 0xA0),
            ReservationState::Active { job_id: 7, holder } if holder == ln
        ));
    }

    /// Brutal test #3 (plan Phase D): split an island's replica set;
    /// both sides attempt `→ Active`; at MOST one side ever reaches
    /// Active; the minority side never starts compute.
    #[test]
    fn partition_split_lets_at_most_one_side_commit_active() {
        let fold = new_reservations();
        let set = ReplicaSet::new([1, 2, 3, 4, 5]);
        let mut cohort = ReplicaCohort::new(set.members());

        // Leader A holds the Reserved (it won the gang claim).
        let a = EntityKeypair::generate();
        let an = a.entity_id().node_id();
        single_island_claim(&fold, &a, an, 1, 0xA0, fresh()).unwrap();
        let claimant_a = Claimant::new(&fold, &a, an);

        // Split 3 | 2. A is on the majority side {1,2,3}.
        let majority =
            commit_active(&claimant_a, &mut cohort, &set, &[1, 2, 3], 0xA0, 7, 2).unwrap();
        assert_eq!(majority, ActiveCommitOutcome::Committed);

        // A would-be competing leader B on the minority side {4,5}
        // cannot gather a quorum → never starts compute.
        let b = EntityKeypair::generate();
        let bn = b.entity_id().node_id();
        let claimant_b = Claimant::new(&fold, &b, bn);
        let minority = commit_active(&claimant_b, &mut cohort, &set, &[4, 5], 0xA0, 9, 3).unwrap();
        assert_eq!(
            minority,
            ActiveCommitOutcome::NoQuorum { acks: 2, needed: 3 },
            "minority side must never reach Active",
        );

        // After heal: exactly one Active exists, held by A.
        assert!(matches!(
            state_of(&fold, 0xA0),
            ReservationState::Active { holder, .. } if holder == an
        ));
    }

    /// Brutal test #3, fence half: after a leadership change, a stale
    /// ex-leader's late `Active` (at its old, superseded epoch) is
    /// refused by every replica that already witnessed the newer
    /// term — even with a full majority reachable.
    #[test]
    fn stale_ex_leader_active_is_fenced_even_with_a_majority_reachable() {
        let fold = new_reservations();
        let set = ReplicaSet::new([1, 2, 3]);
        let mut cohort = ReplicaCohort::new(set.members());

        // New leader N reserves + commits at epoch 5 over the whole
        // (healthy) replica set.
        let n = EntityKeypair::generate();
        let nn = n.entity_id().node_id();
        single_island_claim(&fold, &n, nn, 1, 0xA0, fresh()).unwrap();
        let claimant_n = Claimant::new(&fold, &n, nn);
        assert_eq!(
            commit_active(&claimant_n, &mut cohort, &set, &[1, 2, 3], 0xA0, 1, 5).unwrap(),
            ActiveCommitOutcome::Committed,
        );
        // Every replica now fenced at epoch 5.
        for r in [1, 2, 3] {
            assert_eq!(cohort.highest_witnessed(r, 0xA0), 5);
        }

        // Stale ex-leader O proposes at epoch 4, full majority
        // reachable — but every replica fences it → zero acks → no
        // quorum. O never commits.
        let o = EntityKeypair::generate();
        let on = o.entity_id().node_id();
        let claimant_o = Claimant::new(&fold, &o, on);
        let stale = commit_active(&claimant_o, &mut cohort, &set, &[1, 2, 3], 0xA0, 2, 4).unwrap();
        assert_eq!(
            stale,
            ActiveCommitOutcome::NoQuorum { acks: 0, needed: 2 },
            "stale ex-leader's epoch-4 Active fenced at every replica",
        );
        // The fold's Active is still N's, untouched.
        assert!(matches!(
            state_of(&fold, 0xA0),
            ReservationState::Active { holder, .. } if holder == nn
        ));
    }

    /// Quorum reached but the proposer lost the reservation to a
    /// takeover before committing → `LostReservation`, no Active.
    #[test]
    fn quorum_but_lost_reservation_does_not_commit() {
        let fold = new_reservations();
        let set = ReplicaSet::new([1, 2, 3]);
        let mut cohort = ReplicaCohort::new(set.members());

        // A holds nothing on this island (never reserved it); B holds
        // it. A reaches a quorum of acks but the fold rejects A's
        // Active because A isn't the holder.
        let a = EntityKeypair::generate();
        let an = a.entity_id().node_id();
        let b = EntityKeypair::generate();
        let bn = b.entity_id().node_id();
        single_island_claim(&fold, &b, bn, 1, 0xA0, fresh()).unwrap();

        let claimant_a = Claimant::new(&fold, &a, an);
        let out = commit_active(&claimant_a, &mut cohort, &set, &[1, 2, 3], 0xA0, 7, 2).unwrap();
        assert_eq!(out, ActiveCommitOutcome::LostReservation);
        // B still holds it, Reserved (no Active leaked).
        assert!(matches!(
            state_of(&fold, 0xA0),
            ReservationState::Reserved { holder, .. } if holder == bn
        ));
    }
}
