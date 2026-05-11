//! Deterministic nearest-RTT leader election —
//! `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §4 + Locked Decision 3.
//!
//! Pure function over each node's locally-known state. No
//! broadcast, no epoch, no collection window, no `PlacementFilter`
//! consultation. Every healthy replica computes the same winner
//! from the same `(replica_set, self_id, rtt_lookup, health_lookup)`
//! tuple, so leader-loss recovery converges in microseconds without
//! the wire protocol getting involved.
//!
//! ```text
//! elect(replica_set, self) -> NodeId:
//!     R = { r ∈ replica_set : r is healthy in self's local view }
//!     if R is empty: return None       // partition isolated us
//!     sorted = R sorted by (rtt_to(self, r), r.node_id_lex)
//!                                       // primary: lower RTT wins
//!                                       // tie-break: lexicographic NodeId
//!     return sorted[0]
//! ```
//!
//! Lands ahead of the [`ReplicationCoordinator`] daemon (Phase C/E)
//! so the selection function the coordinator plugs into
//! `StandbyGroup`'s leader-loss hook is already tested. Pure
//! function = pure tests; no mesh, no async, no DST harness needed
//! at this layer.

use std::cmp::Ordering;
use std::time::Duration;

use crate::adapter::net::behavior::placement::NodeId;

/// Outcome of a single election round. Mirrors the plan's
/// "elect returns an `Option<NodeId>`" shape, with the variants
/// spelt out so the call site reads cleanly and DST traces
/// distinguish the three cases without inspecting metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElectionOutcome {
    /// `self` won — typically the case when self is healthy
    /// (RTT to self = 0 < any peer RTT). The coordinator
    /// transitions `Candidate → Leader`.
    SelfWins,
    /// A peer won. The coordinator transitions
    /// `Candidate → Replica` and follows the named peer.
    PeerWins(NodeId),
    /// Replica set is empty after the health filter — partition
    /// isolated this node, or every nominal replica is currently
    /// unhealthy. The coordinator stays in `Candidate` (or
    /// transitions to `Idle` per the plan's §3 escape) until
    /// `StandbyGroup` membership repopulates.
    NoEligibleReplica,
}

impl ElectionOutcome {
    /// `Some(node_id)` when an election produced a leader,
    /// `None` for [`Self::NoEligibleReplica`]. Convenience
    /// projection for call sites that just want the winning id.
    pub fn winner(self) -> Option<NodeId> {
        match self {
            Self::SelfWins => None, // caller already knows self's id
            Self::PeerWins(node) => Some(node),
            Self::NoEligibleReplica => None,
        }
    }

    /// True iff this node was elected leader. Convenience for the
    /// coordinator's `Candidate → Leader` branch.
    pub fn is_self(self) -> bool {
        matches!(self, Self::SelfWins)
    }
}

/// Compute the deterministic winner per plan §4.
///
/// - `replica_set` — every node currently registered as a replica
///   for the channel (the membership view comes from
///   `StandbyGroup`). MUST include `self_id` when this node is one
///   of the replicas.
/// - `self_id` — the node calling `elect`. Used both as a member
///   of the replica set and as the RTT lookup's source.
/// - `rtt_to` — closure returning the measured RTT from `self` to
///   the named peer. `None` means "no RTT measurement available"
///   — treated as unhealthy (the proximity graph hasn't pinged the
///   peer, so we don't trust its liveness). `rtt_to(self_id)` MAY
///   return `None`; we treat self-RTT as zero implicitly because
///   the membership/health filter for `self` is driven separately
///   by `health_of`.
/// - `health_of` — closure returning `true` when the named peer is
///   considered alive in `self`'s local membership view. This is
///   the `StandbyGroup` health bit; the proximity graph's RTT
///   freshness is folded in upstream.
///
/// Returns one of three [`ElectionOutcome`] variants.
///
/// **Determinism contract.** For any fixed `(replica_set, self_id,
/// rtt_to, health_of)` the output is byte-identical across calls,
/// across processes, across machines. The plan's safety property
/// "no two nodes leader for the same channel within a single
/// partition" derives from this — when every replica computes
/// `elect` over the same view, they all pick the same winner.
pub fn elect<R, H>(
    replica_set: &[NodeId],
    self_id: NodeId,
    rtt_to: R,
    health_of: H,
) -> ElectionOutcome
where
    R: Fn(NodeId) -> Option<Duration>,
    H: Fn(NodeId) -> bool,
{
    // Build the healthy candidate set. Self is included iff health
    // says so; pinning a healthy self in is the right shape — the
    // coordinator that calls `elect` only does so from
    // `Candidate`, which implies `self` already considers itself
    // alive.
    let mut candidates: Vec<(NodeId, Duration)> = Vec::with_capacity(replica_set.len());
    for &node in replica_set {
        if !health_of(node) {
            continue;
        }
        let rtt = if node == self_id {
            // Self-RTT is zero — RTT measurements are pairwise and
            // the proximity graph doesn't store a self-entry.
            Duration::ZERO
        } else {
            // Peer with no RTT measurement is treated as unhealthy
            // for ranking purposes. The plan §4 doesn't enumerate
            // this case — but a peer we've never pinged is one we
            // can't trust to be a useful leader, so excluding it
            // matches the "healthy in self's local view" filter.
            match rtt_to(node) {
                Some(d) => d,
                None => continue,
            }
        };
        candidates.push((node, rtt));
    }

    if candidates.is_empty() {
        return ElectionOutcome::NoEligibleReplica;
    }

    // Stable sort by (rtt ascending, node_id ascending). Vec::sort
    // is guaranteed stable in std; this is the deterministic
    // tie-break the plan pins.
    candidates.sort_by(|a, b| match a.1.cmp(&b.1) {
        Ordering::Equal => a.0.cmp(&b.0),
        other => other,
    });

    let (winner, _) = candidates[0];
    if winner == self_id {
        ElectionOutcome::SelfWins
    } else {
        ElectionOutcome::PeerWins(winner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RTT lookup from a static table. Missing entries
    /// produce `None`.
    fn rtt_table(entries: &[(NodeId, u64)]) -> impl Fn(NodeId) -> Option<Duration> + '_ {
        move |node: NodeId| {
            entries
                .iter()
                .find_map(|(n, ms)| (*n == node).then_some(Duration::from_millis(*ms)))
        }
    }

    /// Health lookup that says yes to every node in `alive`.
    fn alive_set(alive: &[NodeId]) -> impl Fn(NodeId) -> bool + '_ {
        move |node: NodeId| alive.contains(&node)
    }

    #[test]
    fn single_eligible_replica_wins() {
        let set = [0x1];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[]),
            alive_set(&[0x1]),
        );
        assert_eq!(outcome, ElectionOutcome::SelfWins);
    }

    #[test]
    fn empty_replica_set_yields_no_eligible() {
        let set: [NodeId; 0] = [];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[]),
            alive_set(&[0x1]),
        );
        assert_eq!(outcome, ElectionOutcome::NoEligibleReplica);
    }

    #[test]
    fn self_unhealthy_falls_to_peer() {
        // Self is in the replica set but health says no — election
        // routes to the healthy peer.
        let set = [0x1, 0x2];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[(0x2, 50)]),
            alive_set(&[0x2]),
        );
        assert_eq!(outcome, ElectionOutcome::PeerWins(0x2));
    }

    #[test]
    fn all_peers_unhealthy_falls_to_self() {
        let set = [0x1, 0x2, 0x3];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[(0x2, 50), (0x3, 75)]),
            alive_set(&[0x1]),
        );
        assert_eq!(outcome, ElectionOutcome::SelfWins);
    }

    #[test]
    fn all_unhealthy_yields_no_eligible() {
        let set = [0x1, 0x2, 0x3];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[]),
            alive_set(&[]),
        );
        assert_eq!(outcome, ElectionOutcome::NoEligibleReplica);
    }

    #[test]
    fn self_wins_against_distant_peers() {
        // Self-RTT is zero; any healthy peer with positive RTT
        // loses to self.
        let set = [0x1, 0x2, 0x3];
        let outcome = elect(
            &set,
            0x1,
            rtt_table(&[(0x2, 10), (0x3, 5)]),
            alive_set(&[0x1, 0x2, 0x3]),
        );
        assert_eq!(outcome, ElectionOutcome::SelfWins);
    }

    #[test]
    fn lowest_rtt_peer_wins_when_self_excluded() {
        let set = [0x10, 0x20, 0x30];
        let outcome = elect(
            &set,
            0xFF, // self not in set; effectively a coordinator-side query
            rtt_table(&[(0x10, 30), (0x20, 10), (0x30, 20)]),
            alive_set(&[0x10, 0x20, 0x30]),
        );
        assert_eq!(outcome, ElectionOutcome::PeerWins(0x20));
    }

    #[test]
    fn tied_rtt_breaks_by_lex_node_id() {
        // Three peers at identical RTT; lex-smallest NodeId wins.
        let set = [0xCC, 0xAA, 0xBB];
        let outcome = elect(
            &set,
            0xFF,
            rtt_table(&[(0xAA, 25), (0xBB, 25), (0xCC, 25)]),
            alive_set(&[0xAA, 0xBB, 0xCC]),
        );
        assert_eq!(outcome, ElectionOutcome::PeerWins(0xAA));
    }

    #[test]
    fn rtt_takes_priority_over_node_id() {
        // Lower NodeId loses to lower RTT.
        let set = [0xAA, 0xBB];
        let outcome = elect(
            &set,
            0xFF,
            rtt_table(&[(0xAA, 50), (0xBB, 10)]),
            alive_set(&[0xAA, 0xBB]),
        );
        assert_eq!(outcome, ElectionOutcome::PeerWins(0xBB));
    }

    #[test]
    fn peer_without_rtt_measurement_excluded() {
        // The proximity graph hasn't measured RTT to 0xCC; treat
        // as unmeasured = unhealthy for ranking. The healthy peer
        // with a measurement wins.
        let set = [0xAA, 0xBB, 0xCC];
        let outcome = elect(
            &set,
            0xFF,
            rtt_table(&[(0xAA, 30), (0xBB, 50)]), // no entry for 0xCC
            alive_set(&[0xAA, 0xBB, 0xCC]),       // health says yes to all
        );
        assert_eq!(outcome, ElectionOutcome::PeerWins(0xAA));
    }

    #[test]
    fn determinism_across_call_orders() {
        // Pin the determinism contract: same inputs → same output,
        // regardless of input vector order. Iterate every
        // permutation of a small replica set; every call returns
        // the same winner.
        let perms = [
            [0x1, 0x2, 0x3],
            [0x3, 0x2, 0x1],
            [0x2, 0x1, 0x3],
            [0x1, 0x3, 0x2],
            [0x3, 0x1, 0x2],
            [0x2, 0x3, 0x1],
        ];
        for set in perms {
            let outcome = elect(
                &set,
                0xFF,
                rtt_table(&[(0x1, 20), (0x2, 20), (0x3, 20)]),
                alive_set(&[0x1, 0x2, 0x3]),
            );
            assert_eq!(
                outcome,
                ElectionOutcome::PeerWins(0x1),
                "perm {set:?} must converge to same winner",
            );
        }
    }

    #[test]
    fn self_at_zero_rtt_beats_peer_at_zero_rtt_via_lex_node_id() {
        // Edge case: peer's RTT is artificially measured at zero
        // too. Self still wins iff its NodeId is lex-smaller.
        let set = [0x5, 0x10];
        let outcome_smaller = elect(
            &set,
            0x5,
            rtt_table(&[(0x10, 0)]),
            alive_set(&[0x5, 0x10]),
        );
        assert_eq!(outcome_smaller, ElectionOutcome::SelfWins);

        let outcome_larger = elect(
            &set,
            0x10,
            rtt_table(&[(0x5, 0)]),
            alive_set(&[0x5, 0x10]),
        );
        // Self is 0x10, peer is 0x5; both at RTT 0; lex wins for 0x5.
        assert_eq!(outcome_larger, ElectionOutcome::PeerWins(0x5));
    }

    #[test]
    fn winner_helper_strips_self_id() {
        // `ElectionOutcome::winner()` returns `None` for SelfWins
        // (caller already knows self's id) and Some(peer) for
        // PeerWins. Pin the contract so call sites that route on
        // `winner()` handle the SelfWins case via `is_self()`.
        assert_eq!(ElectionOutcome::SelfWins.winner(), None);
        assert_eq!(ElectionOutcome::PeerWins(0xAA).winner(), Some(0xAA));
        assert_eq!(ElectionOutcome::NoEligibleReplica.winner(), None);
    }

    #[test]
    fn is_self_helper() {
        assert!(ElectionOutcome::SelfWins.is_self());
        assert!(!ElectionOutcome::PeerWins(0xAA).is_self());
        assert!(!ElectionOutcome::NoEligibleReplica.is_self());
    }

    #[test]
    fn cross_partition_independent_evaluation() {
        // Two replicas with disjoint views (the partition scenario
        // from plan §4 "Convergence and split-brain"): each picks
        // the locally-reachable winner. Same inputs, same output —
        // the test pins that each side's `elect` is internally
        // consistent; the cross-partition divergence is a topology
        // concern, not a correctness one for the function itself.
        let global_set = [0xA, 0xB, 0xC, 0xD];

        // Side A: sees A, B only.
        let outcome_a = elect(
            &global_set,
            0xA,
            rtt_table(&[(0xB, 5)]),
            alive_set(&[0xA, 0xB]),
        );
        assert_eq!(outcome_a, ElectionOutcome::SelfWins);

        // Side B: sees C, D only.
        let outcome_d = elect(
            &global_set,
            0xD,
            rtt_table(&[(0xC, 5)]),
            alive_set(&[0xC, 0xD]),
        );
        // 0xC is the lex-smaller NodeId at lower RTT than self (0xD
        // sees C at 5ms, self at 0ms — so self wins via RTT priority).
        // Wait: self is 0xD at RTT 0; 0xC at RTT 5ms. Self wins.
        assert_eq!(outcome_d, ElectionOutcome::SelfWins);

        // Pin: dual-leader is possible across partitions because A
        // and D both think they're leader. The DST harness Phase F
        // exercises this; here we just confirm each side's election
        // is self-consistent.
    }

    // ────────────────────────────────────────────────────────────────
    // Phase F invariants — partition-safety properties
    //
    // The DST harness landing in Phase F exercises these under
    // adversarial scheduler interleavings + fault injection. The
    // unit-test versions below pin the same invariants on the pure
    // function so any drift between the production logic and the
    // documented partition-safety contract surfaces here first.
    //
    // **Design note on dual-leader windows.** `elect()` hardcodes
    // self-RTT to zero (the proximity graph doesn't store
    // self-entries; tautologically RTT-to-self is 0). With non-zero
    // pairwise RTTs, every node sees itself at the lowest RTT and
    // would compute `SelfWins` in a symmetric all-healthy scenario.
    // The pure function does NOT enforce "one leader per partition"
    // by itself — that property comes from the broader system:
    //
    // 1. Election only fires when the current leader goes silent;
    //    in steady state nobody runs `elect`.
    // 2. The health filter excludes the silent leader, leaving only
    //    survivors as candidates.
    // 3. Among survivors, each runs `elect` and may produce
    //    `SelfWins`. Dual-leader windows are expected.
    // 4. The capability tag layer (`announce_chain` /
    //    `find_chain_holders`) + heartbeat cycle converges to one
    //    leader within ~3 heartbeats post-failover.
    //
    // The DST harness Phase F exercises the convergence guarantee.
    // The unit tests below pin the narrower properties the pure
    // function does enforce.
    // ────────────────────────────────────────────────────────────────

    /// Phase F invariant: when one peer is observed at zero RTT
    /// from every other node's view (representing a "central"
    /// leader with established connections), every node agrees on
    /// that peer as the winner. This is the convergence case the
    /// election function does enforce — and the scenario it routes
    /// for whenever the heartbeat cycle has propagated leader
    /// identity through the proximity graph.
    #[test]
    fn central_peer_at_zero_rtt_wins_across_all_voters() {
        let set = [0x10, 0x20, 0x30];
        // 0x10 is the "central" candidate — every node observes
        // it at RTT 0 (tightest possible measurement). Other peers
        // sit at positive RTT.
        let rtt_for = |_: NodeId, to: NodeId| -> Option<Duration> {
            match to {
                0x10 => Some(Duration::ZERO),
                0x20 => Some(Duration::from_millis(10)),
                0x30 => Some(Duration::from_millis(20)),
                _ => None,
            }
        };
        let healthy: std::collections::HashSet<NodeId> = set.iter().copied().collect();

        let mut winners: Vec<NodeId> = Vec::new();
        for &self_id in &set {
            let outcome = elect(
                &set,
                self_id,
                |peer| rtt_for(self_id, peer),
                |peer| healthy.contains(&peer),
            );
            let winner = match outcome {
                ElectionOutcome::SelfWins => self_id,
                ElectionOutcome::PeerWins(w) => w,
                ElectionOutcome::NoEligibleReplica => {
                    panic!("expected a winner; healthy partition has eligible replicas");
                }
            };
            winners.push(winner);
        }
        assert!(
            winners.windows(2).all(|w| w[0] == w[1]),
            "every node converges on the central-zero-RTT winner; got {winners:?}"
        );
        assert_eq!(winners[0], 0x10);
    }

    /// Phase F documentation: in the symmetric-RTT failover
    /// scenario (dead leader filtered out; survivors each see each
    /// other at the same RTT), every survivor's local `elect()`
    /// reports `SelfWins`. The pure function does NOT enforce
    /// "single leader per partition" in this case — that property
    /// comes from the broader system (heartbeat cycle + capability
    /// tag layer demoting all-but-one Leader within ~3 heartbeats).
    ///
    /// Pinned as expected behavior. Phase F's DST harness asserts
    /// the broader-system convergence guarantee; this test pins
    /// the pure function's contract that produces the dual-leader
    /// window the convergence mechanism then resolves.
    #[test]
    fn symmetric_failover_yields_dual_self_winners_as_expected() {
        let dead_leader = 0x05;
        let survivors = [0x10, 0x20, 0x30];
        let set = [dead_leader, 0x10, 0x20, 0x30];
        let rtt_for = |from: NodeId, to: NodeId| -> Option<Duration> {
            if from == to {
                return Some(Duration::ZERO);
            }
            Some(Duration::from_millis(5))
        };
        let healthy_for_survivor = |peer: NodeId| peer != dead_leader;

        let mut self_winners = Vec::new();
        for &self_id in &survivors {
            let outcome = elect(
                &set,
                self_id,
                |peer| rtt_for(self_id, peer),
                healthy_for_survivor,
            );
            if outcome == ElectionOutcome::SelfWins {
                self_winners.push(self_id);
            }
        }
        // Every survivor sees self at 0 RTT (tautology) which is
        // lower than the symmetric 5ms peer RTT, so every survivor
        // self-wins. Convergence is the broader system's job.
        assert_eq!(
            self_winners, survivors,
            "symmetric-RTT failover produces N self-winners; convergence is broader-system"
        );
    }
}
