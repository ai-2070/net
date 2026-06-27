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

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
///
/// `Send + Sync` (not `+ 'static`): the registry stores scorers as
/// `Arc<dyn PlacementScorer>`, which already pins `'static` at the
/// storage site. Leaving `'static` off the trait itself lets a
/// borrowing adapter like [`SnapshotScorer`] implement it and be
/// passed to `reconcile` as a short-lived `&dyn PlacementScorer`.
pub trait PlacementScorer: Send + Sync {
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

// =============================================================================
// Phase 1 (MESH_SCHEDULER_IMPL_PLAN.md) — sampling/decision split.
//
// The Phase D-1 arm (`diff_scheduler`) used to call the live
// `PlacementScorer::score` for every holder of every led chain on every
// reconcile tick. That per-holder sampling is the O(N) polling cost the impl
// plan targets. Phase 1 moves the *sampling* loop-side into `LocalScheduler`
// (which records `ScoreHistory` and produces a `ScoreSnapshot`) and hands the
// decision pass a `SnapshotScorer` that answers `score()` from the snapshot
// and delegates the rare `best_alternative()` to the live index. The pure
// `reconcile` / `diff_scheduler` signatures are untouched — the snapshot rides
// in behind the existing `PlacementScorer` trait object.
// =============================================================================

/// Cached per-`(chain, holder)` placement scores sampled loop-side by
/// [`LocalScheduler`]. Decouples score *sampling* (gated by cadence +
/// dirty-bits in Phase 2) from the *decision* in `diff_scheduler`, which reads
/// scores through a [`SnapshotScorer`] instead of evaluating the live
/// `PlacementFilter` per holder per tick.
#[derive(Clone, Default, Debug)]
pub struct ScoreSnapshot {
    scores: HashMap<(ChainId, NodeId), f32>,
}

impl ScoreSnapshot {
    /// Empty snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Score for `(chain, node)` if it was sampled, else `None`. `None` makes
    /// the decision arm treat the holder as "no opinion → skip," exactly as a
    /// live scorer returning `None` would.
    pub fn get(&self, chain: ChainId, node: NodeId) -> Option<f32> {
        self.scores.get(&(chain, node)).copied()
    }

    /// Record a sampled score.
    pub fn insert(&mut self, chain: ChainId, node: NodeId, score: f32) {
        self.scores.insert((chain, node), score);
    }

    /// Number of `(chain, holder)` entries sampled.
    pub fn len(&self) -> usize {
        self.scores.len()
    }

    /// `true` when nothing was sampled.
    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// A [`PlacementScorer`] that answers `score()` from a precomputed
/// [`ScoreSnapshot`] and delegates `best_alternative()` to a live scorer. The
/// loop wraps the per-tick snapshot + the installed scorer in one of these and
/// hands it to `reconcile`, so the decision arm reads cached scores (cheap)
/// while the rare candidate search still hits the live capability index.
pub struct SnapshotScorer<'a> {
    snapshot: &'a ScoreSnapshot,
    live: &'a dyn PlacementScorer,
}

impl<'a> SnapshotScorer<'a> {
    /// Wrap a sampled snapshot + the live scorer it was sampled from.
    pub fn new(snapshot: &'a ScoreSnapshot, live: &'a dyn PlacementScorer) -> Self {
        Self { snapshot, live }
    }
}

impl PlacementScorer for SnapshotScorer<'_> {
    fn score(&self, chain: ChainId, node: NodeId) -> Option<f32> {
        self.snapshot.get(chain, node)
    }

    fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)> {
        self.live.best_alternative(chain, exclude)
    }
}

/// Score-drift classification for a tracked chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Trend {
    /// Score is holding steady (within `TREND_EPS` of its running mean).
    #[default]
    Stable,
    /// Latest sample fell meaningfully below the running mean.
    Degrading,
    /// Latest sample rose meaningfully above the running mean.
    Improving,
}

/// Bounded score time-series for one tracked chain. Tracks the
/// decision-relevant signal — the *worst* holder's score each sample — plus an
/// incremental EWMA for cheap trend detection.
///
/// Bounded ring (NOT the design doc's full 60-minute window): trend detection
/// needs only a short tail + a running mean, so this is ~1 KB/chain rather than
/// the design doc's ~115 KB/artifact estimate.
#[derive(Clone, Debug)]
pub struct ScoreHistory {
    recent: VecDeque<(Instant, f32)>,
    current: f32,
    ewma: f32,
    trend: Trend,
}

/// Rolling-window cap. ~a few minutes at heartbeat cadence.
const HISTORY_CAP: usize = 64;
/// EWMA smoothing factor for the running mean / trend.
const EWMA_ALPHA: f32 = 0.3;
/// Dead-band around the running mean below which a sample reads as `Stable`.
const TREND_EPS: f32 = 0.02;

impl ScoreHistory {
    fn new(now: Instant, score: f32) -> Self {
        let mut recent = VecDeque::with_capacity(HISTORY_CAP);
        recent.push_back((now, score));
        Self {
            recent,
            current: score,
            ewma: score,
            trend: Trend::Stable,
        }
    }

    fn record(&mut self, now: Instant, score: f32) {
        // Classify against the mean *before* folding in this sample so a
        // single new value can register as a departure from the trend.
        let prev_ewma = self.ewma;
        self.trend = if score < prev_ewma - TREND_EPS {
            Trend::Degrading
        } else if score > prev_ewma + TREND_EPS {
            Trend::Improving
        } else {
            Trend::Stable
        };
        self.ewma = EWMA_ALPHA * score + (1.0 - EWMA_ALPHA) * self.ewma;
        self.current = score;
        if self.recent.len() == HISTORY_CAP {
            self.recent.pop_front();
        }
        self.recent.push_back((now, score));
    }

    /// Latest sampled (worst-holder) score.
    pub fn current(&self) -> f32 {
        self.current
    }

    /// Current drift classification.
    pub fn trend(&self) -> Trend {
        self.trend
    }

    /// Number of samples retained (≤ [`HISTORY_CAP`]).
    pub fn len(&self) -> usize {
        self.recent.len()
    }

    /// `true` when no samples are retained — never the case for a live
    /// history (constructed with one), present for completeness/clippy.
    pub fn is_empty(&self) -> bool {
        self.recent.is_empty()
    }
}

/// Loop-owned drift-scorer sidecar. Holds the non-replicated, observational
/// state the design doc's `LocalScheduler` describes — score history per
/// tracked chain — and produces the per-tick [`ScoreSnapshot`] the reconcile
/// decision arm consumes.
///
/// Lives on the event loop, never in the fold: scores come from live
/// `PlacementFilter` evaluation (RTT / inventory / caps), not from committed
/// events, so they must not enter the replay-deterministic `MeshOsState`. All
/// timestamps are the loop's `last_tick` anchor, never wall-clock.
#[derive(Default)]
pub struct LocalScheduler {
    history: HashMap<ChainId, ScoreHistory>,
}

impl LocalScheduler {
    /// Empty sidecar — nothing tracked until the first `sample`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample placement scores for every chain this node leads and record
    /// per-chain history (keyed on the decision-relevant *worst* holder
    /// score). Returns the snapshot the decision arm reads via
    /// [`SnapshotScorer`].
    ///
    /// Phase 1 samples every led chain on every call, which is
    /// behavior-identical to the old inline scoring in `diff_scheduler` — the
    /// snapshot covers exactly the `(led-chain, holder)` pairs the decision
    /// arm queries. Phase 2 gates this by cadence + dirty-bits and retains the
    /// prior snapshot for unsampled chains.
    pub fn sample(
        &mut self,
        replicas: &HashMap<ChainId, BTreeSet<NodeId>>,
        replica_leader: &HashMap<ChainId, NodeId>,
        this_node: NodeId,
        scorer: &dyn PlacementScorer,
        now: Instant,
    ) -> ScoreSnapshot {
        let mut snapshot = ScoreSnapshot::new();
        let mut led: HashSet<ChainId> = HashSet::new();
        for (&chain, holders) in replicas {
            if replica_leader.get(&chain).copied() != Some(this_node) {
                continue;
            }
            led.insert(chain);
            // Sample every holder; track the worst (lowest) for history.
            let mut worst: Option<f32> = None;
            for &h in holders {
                if let Some(s) = scorer.score(chain, h) {
                    snapshot.insert(chain, h, s);
                    worst = Some(worst.map_or(s, |w| w.min(s)));
                }
            }
            if let Some(w) = worst {
                self.history
                    .entry(chain)
                    .and_modify(|h| h.record(now, w))
                    .or_insert_with(|| ScoreHistory::new(now, w));
            }
        }
        // Drop history for chains we no longer lead so the sidecar stays
        // bounded by the led-chain count, not the all-time chain count.
        self.history.retain(|c, _| led.contains(c));
        snapshot
    }

    /// Score history for `chain`, if tracked. Surfaced for observability /
    /// trend-driven policy (Phase 2+).
    pub fn history(&self, chain: ChainId) -> Option<&ScoreHistory> {
        self.history.get(&chain)
    }

    /// Number of chains with tracked history.
    pub fn tracked_len(&self) -> usize {
        self.history.len()
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

    // ----- Phase 1: snapshot / history / sidecar -----

    #[test]
    fn score_snapshot_get_insert() {
        let mut s = ScoreSnapshot::new();
        assert!(s.is_empty());
        s.insert(7, 70, 0.5);
        assert_eq!(s.get(7, 70), Some(0.5));
        assert_eq!(s.get(7, 71), None);
        assert_eq!(s.len(), 1);
        assert!(!s.is_empty());
    }

    #[test]
    fn snapshot_scorer_reads_snapshot_and_delegates_alternative() {
        let mut snap = ScoreSnapshot::new();
        snap.insert(1, 100, 0.42);
        let live = FixedScorer {
            // Deliberately different from the snapshot so we can prove
            // `score()` reads the snapshot and not the live scorer.
            scores: [((1, 100), 0.99)].into_iter().collect(),
            alternatives: [(1, (200, 0.9))].into_iter().collect(),
        };
        let ss = SnapshotScorer::new(&snap, &live);
        assert_eq!(ss.score(1, 100), Some(0.42), "score comes from the snapshot");
        assert_eq!(ss.score(1, 999), None, "unsampled holder reads None");
        assert_eq!(
            ss.best_alternative(1, &[]),
            Some((200, 0.9)),
            "best_alternative delegates to the live scorer",
        );
        assert_eq!(ss.best_alternative(1, &[200]), None);
    }

    #[test]
    fn score_history_bounds_to_cap() {
        let now = Instant::now();
        let mut h = ScoreHistory::new(now, 0.5);
        for _ in 0..(HISTORY_CAP * 2) {
            h.record(now, 0.5);
        }
        assert_eq!(h.len(), HISTORY_CAP, "ring is bounded by HISTORY_CAP");
    }

    #[test]
    fn score_history_trend_transitions() {
        let now = Instant::now();
        let mut h = ScoreHistory::new(now, 0.8);
        h.record(now, 0.2); // sharp drop below the running mean
        assert_eq!(h.trend(), Trend::Degrading);
        for _ in 0..5 {
            h.record(now, 0.9); // climb above the mean
        }
        assert_eq!(h.trend(), Trend::Improving);
        for _ in 0..30 {
            h.record(now, 0.9); // settle; mean converges to 0.9
        }
        assert_eq!(h.trend(), Trend::Stable);
        assert!((h.current() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn local_scheduler_samples_only_led_chains_and_tracks_worst() {
        let this: NodeId = 100;
        let mut replicas: HashMap<ChainId, BTreeSet<NodeId>> = HashMap::new();
        replicas.insert(1, BTreeSet::from([100, 200]));
        replicas.insert(2, BTreeSet::from([100]));
        let mut leader: HashMap<ChainId, NodeId> = HashMap::new();
        leader.insert(1, this); // we lead chain 1
        leader.insert(2, 999); // someone else leads chain 2
        let scorer = FixedScorer {
            scores: [((1, 100), 0.4), ((1, 200), 0.6), ((2, 100), 0.9)]
                .into_iter()
                .collect(),
            alternatives: HashMap::new(),
        };
        let mut ls = LocalScheduler::new();
        let snap = ls.sample(&replicas, &leader, this, &scorer, Instant::now());
        // Led chain: every holder sampled.
        assert_eq!(snap.get(1, 100), Some(0.4));
        assert_eq!(snap.get(1, 200), Some(0.6));
        // Non-led chain: not sampled.
        assert_eq!(snap.get(2, 100), None);
        // History only for the led chain, keyed on the worst holder.
        assert_eq!(ls.tracked_len(), 1);
        assert!((ls.history(1).unwrap().current() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn local_scheduler_gcs_history_for_chains_no_longer_led() {
        let this: NodeId = 100;
        let scorer = FixedScorer {
            scores: [((1, 100), 0.4), ((2, 100), 0.5)].into_iter().collect(),
            alternatives: HashMap::new(),
        };
        let mut ls = LocalScheduler::new();
        let mut replicas: HashMap<ChainId, BTreeSet<NodeId>> = HashMap::new();
        replicas.insert(1, BTreeSet::from([100]));
        replicas.insert(2, BTreeSet::from([100]));
        let mut leader: HashMap<ChainId, NodeId> = HashMap::new();
        leader.insert(1, this);
        leader.insert(2, this);
        ls.sample(&replicas, &leader, this, &scorer, Instant::now());
        assert_eq!(ls.tracked_len(), 2);
        // Next tick: we lose leadership of chain 2.
        leader.insert(2, 999);
        ls.sample(&replicas, &leader, this, &scorer, Instant::now());
        assert_eq!(ls.tracked_len(), 1);
        assert!(ls.history(2).is_none(), "dropped chain is GC'd");
        assert!(ls.history(1).is_some());
    }
}
