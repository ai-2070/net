//! Projection 6 — sensed capability readiness → per-interest
//! candidate delta (SENSING_INTEREST_COALESCING_PLAN SI-6, §4.8/
//! §4.9).
//!
//! The sensing plane's LOCAL aggregate views join the gang
//! scheduler's candidate pruning **through the same projection seam
//! as local liveness** (Projection 4): a pure function over observed
//! state returning a sorted, replay-deterministic delta the wiring
//! applies at match time — no I/O, no fold mutation.
//!
//! Two disciplines carried over from Projection 4, plus one of its
//! own:
//! - **absence of evidence never prunes**: a provider with no
//!   observation — or a Ready proof outside THIS consumer's budget
//!   (a route change could make it viable) — stays `potential`,
//!   exactly as an unclassified peer is never dropped from matching;
//! - **prune, never mutate**: the delta is applied to the
//!   candidate-host set inside the match call, leaving the folds'
//!   CRDT-grade AP state byte-identical;
//! - **never a suspension** (§4.9): the capability entry's
//!   suspension flag stays reserved for *unconditional* loss. One
//!   conditional observation — one interest's NotReady — deprioritizes
//!   candidates for THAT interest's match only; the entry, and every
//!   other interest's matching, is untouched.
//!
//! Viability is [`classify_branch`] — the §3.5 rule
//! `project_aggregate` itself applies — so the scheduler's candidate
//! order can never drift from the aggregate the consumer projects.

use crate::adapter::net::behavior::fold::NodeId;
use crate::adapter::net::behavior::sensing::{
    classify_branch, BranchViability, BranchView, ConsumerLatencyBudget,
};

/// Per-interest sensed candidate delta (Projection 6). All lists are
/// deterministic: `viable` is ranked best-first by the consumer-local
/// economics (route + provider start, provider id tie-break — the
/// aggregate's own order); `potential` and `non_viable` are sorted by
/// id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SensedCandidates {
    /// Providers whose sensed projection is locally viable (Ready
    /// within the budget) — ranked best-first. The claim targets
    /// [`Self::selected_provider`].
    pub viable: Vec<NodeId>,
    /// Providers with no viability verdict (Unknown, or Ready
    /// outside the budget) — retained in matching, never pruned.
    pub potential: Vec<NodeId>,
    /// Providers sensed explicitly NotReady for THIS interest —
    /// pruned from THIS match only, exactly like a down host; never
    /// suspended.
    pub non_viable: Vec<NodeId>,
}

impl SensedCandidates {
    /// The provider a claim for this interest should target: the
    /// aggregate's best-ranked viable candidate (`None` when nothing
    /// is currently viable — the scheduler falls back to unranked
    /// matching over `potential`).
    pub fn selected_provider(&self) -> Option<NodeId> {
        self.viable.first().copied()
    }
}

/// Project one interest's sensed branch views into a
/// [`SensedCandidates`] delta (Projection 6). Pure: reads only the
/// given views (the §4.9 overlay joined with live route estimates),
/// returns a value, mutates nothing.
pub fn project_sensed_candidates(
    branches: &[BranchView],
    budget: &ConsumerLatencyBudget,
) -> SensedCandidates {
    let mut ranked: Vec<(std::time::Duration, NodeId)> = Vec::new();
    let mut delta = SensedCandidates::default();
    for branch in branches {
        match classify_branch(branch, budget) {
            BranchViability::Viable(cost) => ranked.push((cost, branch.provider)),
            BranchViability::Potential => delta.potential.push(branch.provider),
            BranchViability::NonViable => delta.non_viable.push(branch.provider),
        }
    }
    ranked.sort();
    delta.viable = ranked.into_iter().map(|(_, id)| id).collect();
    delta.potential.sort_unstable();
    delta.non_viable.sort_unstable();
    delta
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::adapter::net::behavior::sensing::ProjectedReadiness;

    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn branch(
        provider: NodeId,
        projection: ProjectedReadiness,
        route_ms: u64,
        start_ms: u64,
    ) -> BranchView {
        BranchView {
            provider,
            projection,
            estimated_start: Some(ms(start_ms)),
            route_estimate: ms(route_ms),
        }
    }

    #[test]
    fn classifies_and_ranks_by_the_aggregates_own_economics() {
        let budget = ConsumerLatencyBudget {
            end_to_end_within: Some(ms(500)),
        };
        let branches = [
            // Viable, cost 300.
            branch(5, ProjectedReadiness::Ready, 200, 100),
            // Viable, cost 150 — ranks FIRST despite the higher id.
            branch(9, ProjectedReadiness::Ready, 100, 50),
            // Ready but over budget: potential, never pruned.
            branch(2, ProjectedReadiness::Ready, 600, 100),
            // Unknown: potential.
            branch(7, ProjectedReadiness::Unknown, 10, 10),
            // Explicit NotReady: non-viable for THIS interest.
            branch(3, ProjectedReadiness::NotReady, 10, 10),
        ];
        let delta = project_sensed_candidates(&branches, &budget);
        assert_eq!(delta.viable, vec![9, 5], "ranked by route + start");
        assert_eq!(delta.potential, vec![2, 7], "sorted; absence never prunes");
        assert_eq!(delta.non_viable, vec![3]);
        assert_eq!(delta.selected_provider(), Some(9));
    }

    #[test]
    fn equal_costs_tie_break_on_provider_id_deterministically() {
        let budget = ConsumerLatencyBudget::default();
        let branches = [
            branch(8, ProjectedReadiness::Ready, 100, 0),
            branch(4, ProjectedReadiness::Ready, 100, 0),
        ];
        let delta = project_sensed_candidates(&branches, &budget);
        assert_eq!(delta.viable, vec![4, 8], "stable id tie-break");
    }

    #[test]
    fn empty_views_yield_an_empty_delta_and_no_selection() {
        let delta = project_sensed_candidates(&[], &ConsumerLatencyBudget::default());
        assert_eq!(delta, SensedCandidates::default());
        assert_eq!(delta.selected_provider(), None);
    }
}
