//! Partition-safe `→ Active` commit (plan §6 / Phase D) — the
//! correctness bar.
//!
//! `Reserved` stays AP/optimistic (revocable; a reconcile discards a
//! losing reserve, nothing spent). Only the `→ Active` edge — the one
//! that starts irreversible GPU compute — is made CP:
//!
//! ```text
//! leader may emit Active(island, job, epoch) iff
//!     majority(island.replica_set) ack the commit      // quorum witness
//! replica accepts Active iff
//!     epoch >= highest epoch it has witnessed            // fence stale ex-leader
//! ```
//!
//! The minority side of a split can't reach a majority → no `Active`
//! → no double-run; its job stays `Reserved` and the caller
//! re-queries. The epoch **rides the existing causal-chain /
//! `generation` machinery** (locked decision 3) — there is no
//! parallel Raft term; this module models the two pure predicates the
//! gate is built from (quorum + fence), which the plan's test
//! strategy pins as "pure fns, pure tests". Wiring these onto the live
//! single-leader RedEX replication + `ColocationStrict` island
//! placement is the remaining Phase D integration.
//!
//! Both predicates are deliberately leaderless and side-effect-free:
//! a quorum is a count over a known replica set, a fence is a
//! monotonic per-island epoch comparison.

use std::collections::{HashMap, HashSet};

use crate::adapter::net::behavior::fold::{IslandId, NodeId};

/// Leadership epoch for an island — a monotonic term number that
/// rides the causal chain / reservation `generation` (locked
/// decision 3). A higher epoch means a later leadership term; the
/// fence rejects any `Active` carrying an epoch below what a replica
/// has already witnessed.
pub type Epoch = u64;

/// The replica set backing one island's reservation chain. Pinned to
/// a single fault domain by `ColocationStrict` (plan §5) so a
/// cross-DC partition leaves the whole set — and therefore the
/// quorum — on one side, LAN-local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaSet {
    members: Vec<NodeId>,
}

impl ReplicaSet {
    /// Build a replica set, deduping members. An empty set can never
    /// reach quorum (and a one-node set needs that one node).
    pub fn new(members: impl IntoIterator<Item = NodeId>) -> Self {
        let mut members: Vec<NodeId> = members.into_iter().collect();
        members.sort_unstable();
        members.dedup();
        Self { members }
    }

    /// Number of replicas.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Is the set empty? An empty set never reaches quorum.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The replica node ids.
    pub fn members(&self) -> &[NodeId] {
        &self.members
    }

    /// Is `node` a member of this replica set?
    pub fn contains(&self, node: NodeId) -> bool {
        self.members.binary_search(&node).is_ok()
    }

    /// Strict-majority threshold: the minimum acks that constitute a
    /// quorum. `floor(n/2) + 1`. Undefined (and unreachable) for an
    /// empty set.
    pub fn quorum_threshold(&self) -> usize {
        self.members.len() / 2 + 1
    }

    /// Does `ack_count` distinct acks constitute a strict majority of
    /// this set? `ack_count >= quorum_threshold()`, false for an empty
    /// set. Defers to [`Self::quorum_threshold`] so "majority" has a
    /// single definition the CP gate can't drift from. The minority
    /// side of a split can never satisfy this — the whole point.
    pub fn has_quorum(&self, ack_count: usize) -> bool {
        !self.members.is_empty() && ack_count >= self.quorum_threshold()
    }
}

/// Collects the distinct replica acks for one `(island, epoch)`
/// `→ Active` commit and reports when a strict majority has been
/// witnessed. Acks from non-members and duplicate acks are ignored,
/// so a single replica can't inflate the count.
#[derive(Debug, Clone)]
pub struct QuorumWitness<'a> {
    set: &'a ReplicaSet,
    acked: HashSet<NodeId>,
}

impl<'a> QuorumWitness<'a> {
    /// Start collecting acks for a commit against `set`.
    pub fn new(set: &'a ReplicaSet) -> Self {
        Self {
            set,
            acked: HashSet::new(),
        }
    }

    /// Record an ack from `node`. Returns `true` if it counted (a
    /// member acking for the first time), `false` if it was a non-
    /// member or a duplicate.
    pub fn record_ack(&mut self, node: NodeId) -> bool {
        if !self.set.contains(node) {
            return false;
        }
        self.acked.insert(node)
    }

    /// Distinct member acks recorded so far.
    pub fn ack_count(&self) -> usize {
        self.acked.len()
    }

    /// Has a strict majority of the replica set acked? The leader may
    /// emit the `Active` only once this is `true`.
    pub fn has_quorum(&self) -> bool {
        self.set.has_quorum(self.acked.len())
    }
}

/// Per-island fence: the highest leadership epoch each island has
/// witnessed. A replica consults this before accepting an `Active`,
/// rejecting any that carries an epoch below what it has already
/// seen — which is exactly how a stale ex-leader's late `Active`
/// (emitted at its old, now-superseded epoch) is refused after a
/// leadership change.
#[derive(Debug, Clone, Default)]
pub struct FenceLedger {
    highest: HashMap<IslandId, Epoch>,
}

impl FenceLedger {
    /// A fresh ledger that has witnessed nothing (every island at
    /// epoch 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest epoch witnessed for `island` (0 if never seen).
    pub fn highest_witnessed(&self, island: IslandId) -> Epoch {
        self.highest.get(&island).copied().unwrap_or(0)
    }

    /// Would an `Active` for `island` at `epoch` be accepted? `true`
    /// iff `epoch >= highest_witnessed(island)`. Pure — does not
    /// record anything (use [`Self::accept_active`] to also witness).
    pub fn accepts(&self, island: IslandId, epoch: Epoch) -> bool {
        epoch >= self.highest_witnessed(island)
    }

    /// Witness `epoch` for `island`, advancing the fence to the max
    /// of the current and incoming epoch. Idempotent / monotonic.
    pub fn witness(&mut self, island: IslandId, epoch: Epoch) {
        let slot = self.highest.entry(island).or_insert(0);
        if epoch > *slot {
            *slot = epoch;
        }
    }

    /// Replica-side accept: fence-check `(island, epoch)`, and on
    /// acceptance witness it (so a *later, equal-epoch* duplicate
    /// still passes, but a strictly-lower stale one is now refused).
    /// Returns `true` if the `Active` is accepted.
    pub fn accept_active(&mut self, island: IslandId, epoch: Epoch) -> bool {
        if !self.accepts(island, epoch) {
            return false;
        }
        self.witness(island, epoch);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_majority_threshold() {
        assert_eq!(ReplicaSet::new([1, 2, 3]).quorum_threshold(), 2);
        assert_eq!(ReplicaSet::new([1, 2, 3, 4, 5]).quorum_threshold(), 3);
        assert_eq!(ReplicaSet::new([1]).quorum_threshold(), 1);
        // Even sets need a strict majority (n/2 + 1).
        assert_eq!(ReplicaSet::new([1, 2, 3, 4]).quorum_threshold(), 3);
    }

    #[test]
    fn has_quorum_is_strict_majority() {
        let set = ReplicaSet::new([1, 2, 3]);
        assert!(!set.has_quorum(1));
        assert!(set.has_quorum(2));
        assert!(set.has_quorum(3));
        // Empty set never reaches quorum.
        assert!(!ReplicaSet::new([]).has_quorum(0));
        assert!(!ReplicaSet::new([]).has_quorum(5));
    }

    #[test]
    fn witness_counts_only_distinct_members() {
        let set = ReplicaSet::new([1, 2, 3]);
        let mut w = QuorumWitness::new(&set);
        assert!(w.record_ack(1));
        assert!(!w.record_ack(1), "duplicate ack does not count");
        assert!(!w.record_ack(99), "non-member ack does not count");
        assert_eq!(w.ack_count(), 1);
        assert!(!w.has_quorum());
        assert!(w.record_ack(2));
        assert!(w.has_quorum(), "2/3 distinct members is a majority");
    }

    #[test]
    fn minority_side_of_a_split_never_reaches_quorum() {
        // 5-replica island split 2 | 3. The minority (2) side can
        // collect at most its 2 acks → no quorum → no Active → no
        // double-run. The majority (3) side reaches quorum.
        let set = ReplicaSet::new([1, 2, 3, 4, 5]);

        let mut minority = QuorumWitness::new(&set);
        minority.record_ack(1);
        minority.record_ack(2);
        assert_eq!(minority.ack_count(), 2);
        assert!(
            !minority.has_quorum(),
            "minority side must NOT be able to commit Active",
        );

        let mut majority = QuorumWitness::new(&set);
        majority.record_ack(3);
        majority.record_ack(4);
        majority.record_ack(5);
        assert!(majority.has_quorum(), "majority side commits");
    }

    #[test]
    fn fence_rejects_a_stale_ex_leaders_active() {
        // Replicas witness epoch 5 (the current leadership term). A
        // stale ex-leader emits Active at its old epoch 4 → fenced.
        // The current leader's epoch-5 Active is accepted; a later
        // term (6) is accepted and advances the fence.
        let mut ledger = FenceLedger::new();
        let island: IslandId = 0xA0;

        assert!(ledger.accept_active(island, 5), "current term accepted");
        assert_eq!(ledger.highest_witnessed(island), 5);

        assert!(
            !ledger.accept_active(island, 4),
            "stale ex-leader (lower epoch) must be fenced",
        );
        assert_eq!(ledger.highest_witnessed(island), 5, "fence unchanged");

        assert!(
            ledger.accept_active(island, 5),
            "an equal-epoch retry from the live leader still passes",
        );
        assert!(ledger.accept_active(island, 6), "a newer term advances the fence");
        assert_eq!(ledger.highest_witnessed(island), 6);
        assert!(!ledger.accept_active(island, 5), "now epoch 5 is stale too");
    }

    #[test]
    fn fence_is_per_island() {
        let mut ledger = FenceLedger::new();
        ledger.accept_active(0xA0, 7);
        // A different island starts fresh — epoch 1 is fine there.
        assert!(ledger.accept_active(0xB0, 1));
        assert!(!ledger.accept_active(0xA0, 6), "island A0 still fenced at 7");
    }

    /// The two predicates composed: a leadership change. Old leader
    /// L1 (epoch 5) is partitioned to the minority; new leader L2
    /// (epoch 6) commits on the majority; L1's late epoch-5 Active is
    /// both quorum-starved AND fenced — belt and suspenders.
    #[test]
    fn leadership_change_minority_leader_cannot_commit() {
        let set = ReplicaSet::new([1, 2, 3, 4, 5]);
        let island: IslandId = 0xC0;
        let mut ledger = FenceLedger::new();

        // L2 commits epoch 6 on the majority {3,4,5}.
        let mut l2 = QuorumWitness::new(&set);
        for r in [3, 4, 5] {
            l2.record_ack(r);
        }
        assert!(l2.has_quorum());
        assert!(ledger.accept_active(island, 6), "new leader commits");

        // L1, stranded with {1,2}, tries epoch 5: no quorum...
        let mut l1 = QuorumWitness::new(&set);
        for r in [1, 2] {
            l1.record_ack(r);
        }
        assert!(!l1.has_quorum(), "minority leader can't reach quorum");
        // ...and even if it somehow reached a replica, the fence
        // rejects its stale epoch.
        assert!(!ledger.accepts(island, 5), "and the fence rejects epoch 5");
    }
}
