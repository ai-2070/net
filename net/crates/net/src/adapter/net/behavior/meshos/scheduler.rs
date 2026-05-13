//! Phase D-1 — continuous-rebalance scoring loop. The body of
//! [`MESH_SCHEDULER_PLAN.md`] folded into the MeshOS reconcile
//! arm: a per-tick check that compares each chain's current
//! holders' placement scores against the configured threshold,
//! and (when a better alternative exists + cooldown elapsed)
//! emits a `RequestEviction` for the worst under-scorer.
//!
//! [`MESH_SCHEDULER_PLAN.md`]: ../../../../../../docs/plans/MESH_SCHEDULER_PLAN.md
//!
//! The substrate's `PlacementFilter` and capability index are
//! exposed via the [`PlacementScorer`] trait — production
//! consumers wire a `PlacementFilter`-backed impl; tests mock
//! it. The scheduler is leader-only by construction: per-chain
//! `Request*` actions only emit when this node is the elected
//! leader (same gate Phase C respects).
//!
//! # Why two stages
//!
//! Phase D-1's reconcile emits **eviction first**. Once the
//! eviction commits and the holder count drops below the
//! desired count, Phase C's existing diff emits
//! `RequestPlacement` to refill. This leaves a brief
//! under-replication window — acceptable as a first cut;
//! production-grade orchestration can layer on a new
//! atomic-swap action variant if + when the migration
//! orchestrator demands it.
//!
//! # Cooldown
//!
//! `MeshOsState::last_rebalance[chain]` records the timestamp
//! of the most recent eviction emission for the chain.
//! Subsequent rebalance evaluations within
//! `SchedulerConfig::cooldown` skip the chain — avoids
//! flapping where the scheduler emits A→B then immediately
//! B→A as scoring drifts back.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use super::event::{ChainId, NodeId};

/// Pluggable score source — the scheduler queries it once per
/// holder per tick to decide whether placement should rotate.
///
/// Implementations bridge to the substrate's `PlacementFilter`
/// (production) or to a mock (tests). The trait is intentionally
/// minimal — score-via-Option lets impls disclaim opinion on
/// chains they don't know about, and `best_alternative` lets
/// the scorer carry its own ranking logic (RTT-weighted, scope-
/// aware, etc.) without leaking the candidate iteration to the
/// scheduler.
pub trait PlacementScorer: Send + Sync + 'static {
    /// Score this node's hold of `chain`, in `[0.0, 1.0]`.
    /// Returns `None` when the scorer has no opinion (chain
    /// unknown, placement evaluation failed, etc.). The
    /// scheduler treats `None` as "skip this holder."
    fn score(&self, chain: ChainId, node: NodeId) -> Option<f32>;

    /// Pick the best alternative node for `chain`, excluding
    /// the nodes already holding it (`exclude`). Returns
    /// `Some((node_id, score))` for the best candidate or `None`
    /// when no candidate is meaningfully better than the
    /// current holders. The scheduler compares the returned
    /// score against the worst current holder's score plus
    /// hysteresis before committing.
    fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)>;
}

/// Tunables for [`super::reconcile::reconcile`]'s scheduler arm.
/// `Default::default()` reproduces the plan's numbers: score
/// floor 0.5, hysteresis gap 0.2, 5 min cooldown.
///
/// `#[non_exhaustive]`: future scoring knobs ride in without
/// breaking downstream callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SchedulerConfig {
    /// A holder's `score()` below this threshold flags the
    /// chain as a rebalance candidate. Default `0.5`.
    pub score_floor: f32,

    /// The best alternative's score must exceed the worst
    /// current holder's score by at least this much to commit
    /// a rebalance. Default `0.2`. Prevents flap when small
    /// score fluctuations cause continuous migration.
    pub hysteresis_gap: f32,

    /// After emitting a rebalance for a chain, subsequent
    /// evaluations within this window skip the chain. Default
    /// 5 min — matches the `MESH_SCHEDULER_PLAN.md` value.
    pub cooldown: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            score_floor: 0.5,
            hysteresis_gap: 0.2,
            cooldown: Duration::from_secs(5 * 60),
        }
    }
}

/// Shareable registry slot for the active scorer. Mirrors the
/// `ProbeRegistry` shape used by the locality / health probes —
/// `Arc<RwLock<Option<Arc<dyn PlacementScorer>>>>` clone-shared
/// between the loop and external installer paths so the
/// runtime can attach the scorer after `start`.
#[derive(Clone, Default)]
pub struct SchedulerRegistry {
    inner: Arc<RwLock<Option<Arc<dyn PlacementScorer>>>>,
}

impl SchedulerRegistry {
    /// Empty registry. Reconcile's scheduler arm is a no-op
    /// until a scorer is installed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install / replace the active scorer. Subsequent
    /// reconcile passes use the new scorer; in-flight passes
    /// see whichever scorer was active when they sampled.
    pub fn install(&self, scorer: Arc<dyn PlacementScorer>) -> Option<Arc<dyn PlacementScorer>> {
        let mut guard = self.inner.write();
        guard.replace(scorer)
    }

    /// Read the currently-installed scorer (`None` if absent).
    pub fn current(&self) -> Option<Arc<dyn PlacementScorer>> {
        self.inner.read().clone()
    }

    /// `true` when a scorer is installed.
    pub fn has_scorer(&self) -> bool {
        self.inner.read().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test-only fixed scorer: per-chain-per-node table of
    /// scores. Returns `None` for unrecorded entries.
    pub(crate) struct FixedScorer {
        pub scores: HashMap<(ChainId, NodeId), f32>,
        pub alternatives: HashMap<ChainId, (NodeId, f32)>,
    }

    impl PlacementScorer for FixedScorer {
        fn score(&self, chain: ChainId, node: NodeId) -> Option<f32> {
            self.scores.get(&(chain, node)).copied()
        }
        fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)> {
            let (n, s) = self.alternatives.get(&chain).copied()?;
            if exclude.contains(&n) {
                None
            } else {
                Some((n, s))
            }
        }
    }

    #[test]
    fn fixed_scorer_returns_table_entries() {
        let mut scorer = FixedScorer {
            scores: HashMap::new(),
            alternatives: HashMap::new(),
        };
        scorer.scores.insert((1, 100), 0.4);
        scorer.alternatives.insert(1, (200, 0.9));
        assert_eq!(scorer.score(1, 100), Some(0.4));
        assert_eq!(scorer.score(1, 999), None);
        assert_eq!(scorer.best_alternative(1, &[]), Some((200, 0.9)));
        assert_eq!(scorer.best_alternative(1, &[200]), None);
    }

    #[test]
    fn registry_install_replaces_and_returns_prior() {
        let reg = SchedulerRegistry::new();
        assert!(!reg.has_scorer());
        let s1 = Arc::new(FixedScorer {
            scores: HashMap::new(),
            alternatives: HashMap::new(),
        });
        let prior = reg.install(Arc::clone(&s1) as Arc<dyn PlacementScorer>);
        assert!(prior.is_none());
        assert!(reg.has_scorer());
        let s2 = Arc::new(FixedScorer {
            scores: HashMap::new(),
            alternatives: HashMap::new(),
        });
        let prior2 = reg.install(s2 as Arc<dyn PlacementScorer>);
        assert!(prior2.is_some());
    }

    #[test]
    fn scheduler_config_defaults_match_the_plan() {
        let cfg = SchedulerConfig::default();
        assert!((cfg.score_floor - 0.5).abs() < 1e-6);
        assert!((cfg.hysteresis_gap - 0.2).abs() < 1e-6);
        assert_eq!(cfg.cooldown, Duration::from_secs(5 * 60));
    }
}
