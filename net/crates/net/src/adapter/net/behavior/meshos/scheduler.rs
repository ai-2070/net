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

    /// Live (un-cached) score for `(chain, node)` — what `score` would
    /// return straight from the live placement index, bypassing any
    /// snapshot a wrapping scorer serves cached values from. Defaults to
    /// `score`; [`SnapshotScorer`] overrides it to hit the live scorer so
    /// the decision arm can re-confirm a candidate victim that was
    /// *selected* from a possibly-stale snapshot before acting on it (see
    /// `diff_scheduler`'s sub-floor path). Plain scorers need not implement
    /// it — for them the live and cached scores are identical.
    fn live_score(&self, chain: ChainId, node: NodeId) -> Option<f32> {
        self.score(chain, node)
    }

    /// Pick the best alternative node for `chain`, excluding
    /// the nodes already holding it (`exclude`). Returns
    /// `Some((node_id, score))` for the best candidate or `None`
    /// when no candidate is meaningfully better than the
    /// current holders. The scheduler compares the returned
    /// score against the worst current holder's score plus
    /// hysteresis before committing.
    fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)>;

    /// A cheap, monotonic per-node fingerprint of the capability /
    /// inventory state that placement scoring reads — e.g. the
    /// capability-fold `generation` counter for `node`. Bumped
    /// whenever anything that could change the node's score changes.
    ///
    /// [`LocalScheduler`] folds the fingerprints of a chain's holders
    /// together to decide whether the chain's scoring inputs moved
    /// since the last sample (dirty-gating, Phase 2 of
    /// `MESH_SCHEDULER_IMPL_PLAN.md`). Returning `None` (the default)
    /// means "I can't cheaply fingerprint this" → the chain is always
    /// treated as dirty, reproducing un-gated every-tick sampling.
    /// Slower drift not captured here (RTT, etc.) is caught by the
    /// scheduler's coarse backstop cadence.
    fn node_fingerprint(&self, node: NodeId) -> Option<u64> {
        let _ = node;
        None
    }

    /// Estimated cost of migrating `chain` to `target`, if the scorer can
    /// estimate it (it has the artifact's state size + the path bandwidth /
    /// RTT). `None` (the default) disables cost-aware gating — the hysteresis
    /// gap alone decides. When `Some`, the decision arm converts it via
    /// [`SchedulerConfig::cost_model`] and requires `score_gain - cost > 0`
    /// before emitting an eviction (Phase 3, `MESH_SCHEDULER_IMPL_PLAN.md`).
    fn migration_cost(&self, chain: ChainId, target: NodeId) -> Option<MigrationCost> {
        let _ = (chain, target);
        None
    }
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

    /// Coarse backstop cadence for the loop-side sampler (Phase 2,
    /// `MESH_SCHEDULER_IMPL_PLAN.md`). A led chain is re-sampled
    /// when its dirty fingerprint moves OR at least this long has
    /// elapsed since its last sample — whichever comes first. The
    /// backstop guarantees eventual re-evaluation even when the
    /// dirty signal misses a slow sub-fingerprint drift. Default
    /// 30 s, per the design doc's Open Question #1.
    pub decision_interval: Duration,

    /// Converts a [`MigrationCost`] estimate into a score-equivalent the
    /// decision arm subtracts from the score gain (Phase 3,
    /// `MESH_SCHEDULER_IMPL_PLAN.md`). Only applies when the scorer
    /// returns a cost via [`PlacementScorer::migration_cost`].
    pub cost_model: MigrationCostModel,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            score_floor: 0.5,
            hysteresis_gap: 0.2,
            cooldown: Duration::from_secs(5 * 60),
            decision_interval: Duration::from_secs(30),
            cost_model: MigrationCostModel::default(),
        }
    }
}

/// Estimated cost of migrating an artifact, in the dimensions the design doc
/// (`MESH_SCHEDULER_PLAN.md` §2) locks. Produced by
/// [`PlacementScorer::migration_cost`]; consumed by [`MigrationCostModel`].
///
/// Note `Default` is hand-written (not derived) so `reliability_factor`
/// defaults to the **neutral `1.0`**, not `0.0`. A derived `0.0` would zero out
/// the whole cost in [`MigrationCostModel::score_equivalent`] (it is a
/// multiplier), silently disabling the Phase 3 net-benefit gate for any caller
/// that builds a cost via `..Default::default()` without setting the weight.
#[derive(Clone, Debug, PartialEq)]
pub struct MigrationCost {
    /// State-transfer time: `bytes_to_transfer / bandwidth` + serialization.
    pub state_transfer: Duration,
    /// Disruption: estimated time the artifact is unavailable mid-migration.
    pub disruption: Duration,
    /// Bytes moved during transfer (relevant under network saturation;
    /// carried for richer models, not used by the default conversion).
    pub bandwidth_bytes: u64,
    /// Importance weight — higher means a more valuable artifact, charged a
    /// proportionally higher cost so it needs a bigger score gain to move.
    /// Defaults to the neutral `1.0`; a non-positive or non-finite value is
    /// clamped to `1.0` by `score_equivalent` so it can never zero a real cost.
    pub reliability_factor: f32,
}

impl Default for MigrationCost {
    fn default() -> Self {
        Self {
            state_transfer: Duration::ZERO,
            disruption: Duration::ZERO,
            bandwidth_bytes: 0,
            // Neutral weight — see the type doc; a `0.0` here would make the
            // gate a no-op under the common `..Default::default()` idiom.
            reliability_factor: 1.0,
        }
    }
}

/// Converts a [`MigrationCost`] into a score-equivalent in `[0, ∞)` so the
/// decision arm can require `score_gain - cost_score > 0` before migrating.
///
/// The default is a deliberately conservative placeholder: the design doc's
/// activation gate says the cost model is uncalibrated until production
/// telemetry exists, so this ships the *mechanism* with a safe default and
/// expects operators to tune `cost_per_sec` against observed migration costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MigrationCostModel {
    /// Score units charged per second of `state_transfer + disruption`,
    /// scaled by `reliability_factor`. Default `0.1`.
    pub cost_per_sec: f32,
}

impl Default for MigrationCostModel {
    fn default() -> Self {
        Self { cost_per_sec: 0.1 }
    }
}

impl MigrationCostModel {
    /// Score-equivalent cost. Monotonic in `state_transfer`, `disruption`,
    /// and `reliability_factor` — the property the design doc's test
    /// strategy pins.
    ///
    /// `reliability_factor` is a multiplier, so a non-positive or non-finite
    /// value is clamped to the neutral `1.0` rather than passed through: a `0.0`
    /// (or NaN) weight would otherwise annihilate the whole cost and silently
    /// disable the gate. A real migration always carries its transfer +
    /// disruption time cost regardless of how its importance is (mis)configured.
    pub fn score_equivalent(&self, cost: &MigrationCost) -> f32 {
        let secs = cost.state_transfer.as_secs_f32() + cost.disruption.as_secs_f32();
        let weight = if cost.reliability_factor.is_finite() && cost.reliability_factor > 0.0 {
            cost.reliability_factor
        } else {
            1.0
        };
        self.cost_per_sec * secs * weight
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

/// Cached placement scores sampled loop-side by [`LocalScheduler`]. Decouples
/// score *sampling* (gated by cadence + dirty-bits in Phase 2) from the
/// *decision* in `diff_scheduler`, which reads scores through a
/// [`SnapshotScorer`] instead of evaluating the live `PlacementFilter` per
/// holder per tick.
///
/// Stored chain → (holder → score) so the gating loop can replace or drop a
/// whole chain's scores in O(1) without scanning the other chains' entries —
/// non-dirty chains cost zero map work per tick.
#[derive(Clone, Default, Debug)]
pub struct ScoreSnapshot {
    scores: HashMap<ChainId, HashMap<NodeId, f32>>,
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
        self.scores.get(&chain).and_then(|m| m.get(&node)).copied()
    }

    /// Record a sampled score.
    pub fn insert(&mut self, chain: ChainId, node: NodeId, score: f32) {
        self.scores.entry(chain).or_default().insert(node, score);
    }

    /// Number of `(chain, holder)` entries sampled.
    pub fn len(&self) -> usize {
        self.scores.values().map(HashMap::len).sum()
    }

    /// `true` when nothing was sampled.
    pub fn is_empty(&self) -> bool {
        self.scores.values().all(HashMap::is_empty)
    }

    /// Replace all of `chain`'s holder scores in one shot (used by the gating
    /// loop when a dirty chain is re-sampled). Dropping the old inner map
    /// clears any holders the chain no longer has.
    fn set_chain(&mut self, chain: ChainId, holders: HashMap<NodeId, f32>) {
        self.scores.insert(chain, holders);
    }

    /// Drop the scores of any chain for which `keep` returns `false`.
    fn retain_chains(&mut self, keep: impl Fn(ChainId) -> bool) {
        self.scores.retain(|&chain, _| keep(chain));
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

    fn live_score(&self, chain: ChainId, node: NodeId) -> Option<f32> {
        // Bypass the snapshot — the decision arm uses this to re-confirm a
        // victim it selected from possibly-stale cached scores.
        self.live.score(chain, node)
    }

    fn best_alternative(&self, chain: ChainId, exclude: &[NodeId]) -> Option<(NodeId, f32)> {
        self.live.best_alternative(chain, exclude)
    }

    fn node_fingerprint(&self, node: NodeId) -> Option<u64> {
        self.live.node_fingerprint(node)
    }

    fn migration_cost(&self, chain: ChainId, target: NodeId) -> Option<MigrationCost> {
        self.live.migration_cost(chain, target)
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

    /// Number of samples retained (≤ `HISTORY_CAP`).
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
///
/// Phase 2 (`MESH_SCHEDULER_IMPL_PLAN.md`) gates re-sampling by two
/// complementary levers, each closing a distinct failure mode:
///
/// - **Dirty-bit** — a chain is re-scored only when the fingerprint of its
///   holders' scoring inputs ([`PlacementScorer::node_fingerprint`]) moves.
///   Kills the O(N) steady-state polling wall: stable chains cost one fold of
///   cheap counter compares and zero scoring.
/// - **Coarse backstop** ([`SchedulerConfig::decision_interval`]) — every led
///   chain is re-scored at least once per interval regardless of its
///   fingerprint. Catches slow sub-fingerprint drift (and any dirty-tracking
///   gap) that the dirty-bit alone would leave silently stale forever.
///
/// Unsampled chains keep their prior scores in the retained snapshot, so the
/// decision arm always has full coverage of the `(led-chain, holder)` pairs it
/// queries.
#[derive(Default)]
pub struct LocalScheduler {
    /// Running snapshot — updated in place; dirty chains replaced, clean
    /// chains retained, dropped chains GC'd. Returned by `sample`.
    current: ScoreSnapshot,
    /// Per-chain score history (worst-holder series + trend).
    history: HashMap<ChainId, ScoreHistory>,
    /// Last dirty fingerprint observed per chain (absent → never sampled, or
    /// the scorer couldn't fingerprint it).
    last_fingerprint: HashMap<ChainId, u64>,
    /// Last sample timestamp per chain — drives the backstop cadence.
    last_sampled: HashMap<ChainId, Instant>,
}

impl LocalScheduler {
    /// Empty sidecar — nothing tracked until the first `sample`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sample placement scores for the chains this node leads and return the
    /// snapshot the decision arm reads via [`SnapshotScorer`].
    ///
    /// A led chain is re-scored when it is new, when its dirty fingerprint
    /// moved, or when `decision_interval` has elapsed since its last sample
    /// (the coarse backstop); otherwise its prior scores are retained
    /// untouched. With a scorer that doesn't implement `node_fingerprint`
    /// (fingerprint `None`), every chain is always dirty — behavior-identical
    /// to Phase 1's sample-everything pass.
    pub fn sample(
        &mut self,
        replicas: &HashMap<ChainId, BTreeSet<NodeId>>,
        replica_leader: &HashMap<ChainId, NodeId>,
        this_node: NodeId,
        scorer: &dyn PlacementScorer,
        now: Instant,
        decision_interval: Duration,
    ) -> &ScoreSnapshot {
        let mut led: HashSet<ChainId> = HashSet::new();
        for (&chain, holders) in replicas {
            if replica_leader.get(&chain).copied() != Some(this_node) {
                continue;
            }
            if holders.is_empty() {
                continue;
            }
            led.insert(chain);

            let fingerprint = Self::chain_fingerprint(scorer, holders);
            let backstop_due = self
                .last_sampled
                .get(&chain)
                .is_none_or(|t| now.saturating_duration_since(*t) >= decision_interval);
            let dirty = match fingerprint {
                // Un-fingerprintable → always re-sample (un-gated).
                None => true,
                Some(fp) => self.last_fingerprint.get(&chain).copied() != Some(fp),
            };
            if !(dirty || backstop_due) {
                // Clean and not yet due — retain the prior scores untouched.
                continue;
            }

            // Re-score every holder; track the worst (lowest) for history.
            let mut holder_scores: HashMap<NodeId, f32> = HashMap::new();
            let mut worst: Option<f32> = None;
            for &h in holders {
                if let Some(s) = scorer.score(chain, h) {
                    // A NaN score is not a usable opinion — skip it so it can't
                    // poison the snapshot or the worst-holder history series
                    // (the decision arm skips NaN too).
                    if s.is_nan() {
                        continue;
                    }
                    holder_scores.insert(h, s);
                    worst = Some(worst.map_or(s, |w| w.min(s)));
                }
            }
            self.current.set_chain(chain, holder_scores);
            if let Some(w) = worst {
                self.history
                    .entry(chain)
                    .and_modify(|h| h.record(now, w))
                    .or_insert_with(|| ScoreHistory::new(now, w));
            }
            match fingerprint {
                Some(fp) => {
                    self.last_fingerprint.insert(chain, fp);
                }
                None => {
                    self.last_fingerprint.remove(&chain);
                }
            }
            self.last_sampled.insert(chain, now);
        }

        // Drop all per-chain state for chains we no longer lead so the sidecar
        // stays bounded by the led-chain count, not the all-time chain count.
        // This GC is only needed when a previously-tracked chain fell out of
        // leadership. Every led chain ends with a `last_sampled` entry (it was
        // either sampled this tick or sampled on an earlier tick — a chain with
        // no entry is always backstop-due, so it gets sampled), and a chain we
        // no longer lead is skipped before `led.insert`, leaving its entry
        // behind. So `last_sampled` ⊇ `led` always, and a size mismatch is
        // exactly the "membership shrank" signal — in the steady state
        // (unchanged leadership) this skips four full map scans per tick.
        if self.last_sampled.len() != led.len() {
            self.current.retain_chains(|c| led.contains(&c));
            self.history.retain(|c, _| led.contains(c));
            self.last_fingerprint.retain(|c, _| led.contains(c));
            self.last_sampled.retain(|c, _| led.contains(c));
        }
        &self.current
    }

    /// Fold a chain's holders into one dirty fingerprint: the holder set
    /// (their `NodeId`s) plus each holder's [`PlacementScorer::node_fingerprint`].
    /// A holder added/removed, or any holder's inputs moving, changes the
    /// result. `None` if any holder is un-fingerprintable (→ always dirty).
    fn chain_fingerprint(scorer: &dyn PlacementScorer, holders: &BTreeSet<NodeId>) -> Option<u64> {
        // FNV-1a fold (shared helper) over each holder's id and its
        // node_fingerprint. The fold is order-sensitive, so it relies on
        // `holders` being a `BTreeSet` (sorted iteration) to stay
        // deterministic across calls.
        use super::super::hash::{fnv1a_step, FNV1A_OFFSET};
        let mut acc = FNV1A_OFFSET;
        for &h in holders {
            let fp = scorer.node_fingerprint(h)?;
            acc = fnv1a_step(acc, h);
            acc = fnv1a_step(acc, fp);
        }
        Some(acc)
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

/// Test-only fixed scorer: a per-`(chain, node)` score table plus a per-chain
/// best-alternative table; returns `None` for unrecorded entries. Defined at
/// module level (under `#[cfg(test)]`) so both this module's tests and the
/// `reconcile` tests share one definition instead of each keeping a copy.
#[cfg(test)]
pub(crate) struct FixedScorer {
    pub scores: HashMap<(ChainId, NodeId), f32>,
    pub alternatives: HashMap<ChainId, (NodeId, f32)>,
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        assert_eq!(cfg.decision_interval, Duration::from_secs(30));
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
        assert_eq!(
            ss.score(1, 100),
            Some(0.42),
            "score comes from the snapshot"
        );
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
        let snap = ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            Instant::now(),
            Duration::from_secs(30),
        );
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
        let interval = Duration::from_secs(30);
        ls.sample(&replicas, &leader, this, &scorer, Instant::now(), interval);
        assert_eq!(ls.tracked_len(), 2);
        // Next tick: we lose leadership of chain 2.
        leader.insert(2, 999);
        ls.sample(&replicas, &leader, this, &scorer, Instant::now(), interval);
        assert_eq!(ls.tracked_len(), 1);
        assert!(ls.history(2).is_none(), "dropped chain is GC'd");
        assert!(ls.history(1).is_some());
    }

    #[test]
    fn losing_leadership_gcs_snapshot_too_not_just_history() {
        // The membership-delta gate must GC *every* sidecar map (the snapshot
        // included), not just history, when a chain drops out of leadership.
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
        let interval = Duration::from_secs(30);
        let t0 = Instant::now();
        let snap = ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        assert_eq!(snap.get(2, 100), Some(0.5));

        // Drop leadership of chain 2 → its snapshot entry must be GC'd, while
        // the still-led chain 1 is retained.
        leader.insert(2, 999);
        let snap = ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            t0 + Duration::from_secs(1),
            interval,
        );
        assert_eq!(snap.get(2, 100), None, "dropped chain's snapshot entry is GC'd");
        assert_eq!(snap.get(1, 100), Some(0.4), "still-led chain is retained");
        assert!(ls.history(2).is_none());
    }

    // ----- Phase 2: dirty-gate + coarse backstop -----

    /// Test scorer that counts `score()` calls and exposes a controllable
    /// per-node fingerprint, so the gating tests can prove exactly when a
    /// chain is (and isn't) re-scored.
    struct CountingScorer {
        score: f32,
        fp_present: std::sync::atomic::AtomicBool,
        fp_bits: std::sync::atomic::AtomicU64,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingScorer {
        fn new(score: f32, fp: Option<u64>) -> Self {
            use std::sync::atomic::*;
            Self {
                score,
                fp_present: AtomicBool::new(fp.is_some()),
                fp_bits: AtomicU64::new(fp.unwrap_or(0)),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
        fn set_fp(&self, fp: u64) {
            self.fp_bits.store(fp, std::sync::atomic::Ordering::Relaxed);
        }
    }

    impl PlacementScorer for CountingScorer {
        fn score(&self, _chain: ChainId, _node: NodeId) -> Option<f32> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(self.score)
        }
        fn best_alternative(&self, _chain: ChainId, _exclude: &[NodeId]) -> Option<(NodeId, f32)> {
            None
        }
        fn node_fingerprint(&self, _node: NodeId) -> Option<u64> {
            use std::sync::atomic::Ordering::Relaxed;
            if self.fp_present.load(Relaxed) {
                Some(self.fp_bits.load(Relaxed))
            } else {
                None
            }
        }
    }

    fn one_led_chain() -> (
        HashMap<ChainId, BTreeSet<NodeId>>,
        HashMap<ChainId, NodeId>,
        NodeId,
    ) {
        let this: NodeId = 100;
        let mut replicas: HashMap<ChainId, BTreeSet<NodeId>> = HashMap::new();
        replicas.insert(1, BTreeSet::from([100]));
        let mut leader: HashMap<ChainId, NodeId> = HashMap::new();
        leader.insert(1, this);
        (replicas, leader, this)
    }

    #[test]
    fn dirty_gate_skips_rescore_when_fingerprint_stable() {
        let (replicas, leader, this) = one_led_chain();
        let scorer = CountingScorer::new(0.9, Some(7));
        let mut ls = LocalScheduler::new();
        let t0 = Instant::now();
        let interval = Duration::from_secs(30);

        // First sample: new chain → scored.
        ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        assert_eq!(scorer.calls(), 1);

        // Second sample, same fingerprint, within the backstop → NOT scored,
        // but the prior score is retained in the returned snapshot.
        let snap = ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            t0 + Duration::from_secs(1),
            interval,
        );
        assert_eq!(snap.get(1, 100), Some(0.9), "clean chain retains its score");
        assert_eq!(
            scorer.calls(),
            1,
            "stable fingerprint within backstop → no rescore"
        );
    }

    #[test]
    fn dirty_gate_rescore_when_fingerprint_moves() {
        let (replicas, leader, this) = one_led_chain();
        let scorer = CountingScorer::new(0.9, Some(7));
        let mut ls = LocalScheduler::new();
        let t0 = Instant::now();
        let interval = Duration::from_secs(30);

        ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        assert_eq!(scorer.calls(), 1);
        // Inputs move → fingerprint changes → re-scored even within backstop.
        scorer.set_fp(8);
        ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            t0 + Duration::from_secs(1),
            interval,
        );
        assert_eq!(scorer.calls(), 2, "fingerprint change forces a rescore");
    }

    #[test]
    fn coarse_backstop_rescore_even_when_fingerprint_stable() {
        let (replicas, leader, this) = one_led_chain();
        let scorer = CountingScorer::new(0.9, Some(7));
        let mut ls = LocalScheduler::new();
        let t0 = Instant::now();
        let interval = Duration::from_secs(30);

        ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        assert_eq!(scorer.calls(), 1);
        // Stable fingerprint, but the decision_interval has elapsed → the
        // backstop forces a rescore. This is the lever the dirty-bit alone
        // can't provide: it catches drift the fingerprint misses.
        ls.sample(&replicas, &leader, this, &scorer, t0 + interval, interval);
        assert_eq!(
            scorer.calls(),
            2,
            "backstop forces a rescore past the interval"
        );
    }

    #[test]
    fn dirty_gate_rescore_when_holder_set_changes() {
        let (mut replicas, leader, this) = one_led_chain();
        let scorer = CountingScorer::new(0.9, Some(7)); // constant per-node fp
        let mut ls = LocalScheduler::new();
        let t0 = Instant::now();
        let interval = Duration::from_secs(30);

        ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        assert_eq!(scorer.calls(), 1, "1 holder scored");
        // Same fingerprint per node, but a holder is added — the chain
        // fingerprint folds the holder set, so it moves and forces a rescore.
        replicas.insert(1, BTreeSet::from([100, 200]));
        ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            t0 + Duration::from_secs(1),
            interval,
        );
        assert_eq!(scorer.calls(), 3, "holder-set change rescored both holders");
    }

    #[test]
    fn no_fingerprint_means_always_dirty() {
        // A scorer that can't fingerprint (default trait behavior) is always
        // re-scored — behavior-identical to Phase 1's sample-everything pass.
        let (replicas, leader, this) = one_led_chain();
        let scorer = CountingScorer::new(0.9, None);
        let mut ls = LocalScheduler::new();
        let t0 = Instant::now();
        let interval = Duration::from_secs(30);

        ls.sample(&replicas, &leader, this, &scorer, t0, interval);
        ls.sample(
            &replicas,
            &leader,
            this,
            &scorer,
            t0 + Duration::from_secs(1),
            interval,
        );
        assert_eq!(scorer.calls(), 2, "no fingerprint → re-scored every tick");
    }

    // ----- Phase 3: migration cost model -----

    #[test]
    fn migration_cost_model_is_monotonic() {
        let model = MigrationCostModel::default();
        let base = MigrationCost {
            state_transfer: Duration::from_secs(1),
            disruption: Duration::from_secs(1),
            bandwidth_bytes: 0,
            reliability_factor: 1.0,
        };
        let base_score = model.score_equivalent(&base);
        assert!(base_score > 0.0);

        // Monotonic in state_transfer.
        let more_transfer = MigrationCost {
            state_transfer: Duration::from_secs(2),
            ..base.clone()
        };
        assert!(model.score_equivalent(&more_transfer) > base_score);

        // Monotonic in disruption.
        let more_disruption = MigrationCost {
            disruption: Duration::from_secs(2),
            ..base.clone()
        };
        assert!(model.score_equivalent(&more_disruption) > base_score);

        // Monotonic in reliability_factor (importance).
        let more_important = MigrationCost {
            reliability_factor: 2.0,
            ..base.clone()
        };
        assert!(model.score_equivalent(&more_important) > base_score);

        // A zero-cost migration is free.
        let zero = MigrationCost::default();
        assert_eq!(model.score_equivalent(&zero), 0.0);
    }

    #[test]
    fn migration_cost_default_weight_does_not_zero_the_gate() {
        // Regression for the reliability_factor footgun: a cost built via
        // `..Default::default()` (no explicit weight) must still charge for its
        // transfer/disruption time. A derived `0.0` default would make the
        // multiplier annihilate the cost and disable the Phase 3 gate.
        let model = MigrationCostModel::default();
        let defaulted = MigrationCost {
            state_transfer: Duration::from_secs(10),
            disruption: Duration::from_secs(10),
            ..Default::default()
        };
        assert!(
            model.score_equivalent(&defaulted) > 0.0,
            "default-weight cost must be non-zero or the net-benefit gate is a no-op",
        );

        // An explicit non-positive / non-finite weight is clamped to the
        // neutral 1.0, never zeroing a real time cost.
        let neutral = model.score_equivalent(&defaulted);
        for bad in [0.0f32, -1.0, f32::NAN] {
            let weighted = MigrationCost {
                reliability_factor: bad,
                ..defaulted.clone()
            };
            assert_eq!(
                model.score_equivalent(&weighted),
                neutral,
                "non-positive / NaN weight ({bad}) must clamp to the neutral 1.0",
            );
        }
    }
}
