//! Per-fold metric counters.
//!
//! The apply / query / evict / snapshot paths bump these
//! synchronously; the Prometheus / Deck adapters sample them
//! through [`FoldStats`].
//!
//! All counters are lock-free atomics so the apply hot path
//! never contends with metrics readers.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Serializable per-fold snapshot of the live [`FoldMetrics`]
/// counters plus static identity (`kind` u16 + channel prefix).
/// The operator-facing surface (`net fold list`, the Deck FOLDS
/// panel, the Prometheus exporter) consumes this shape when it
/// wants a coherent picture of one fold.
///
/// Sampled via [`super::Fold::stats`] for the fold-side view, or
/// aggregated across the registry via
/// [`super::FoldRegistry::stats`] for the multi-fold view.
///
/// All counters are `u64` — the atomics behind them are also
/// `u64`, so no narrowing happens at snapshot time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldStats {
    /// [`super::FoldKind::KIND_ID`] of the fold this snapshot
    /// describes. Surfaces in JSON output so operators
    /// piping `--output json | jq` can route on a stable
    /// identifier rather than the channel-prefix string.
    pub kind: u16,
    /// [`super::FoldKind::CHANNEL_PREFIX`] — operator-friendly
    /// human label for the fold (e.g. `"fold:cap:"`,
    /// `"fold:route:"`, `"fold:res:"`). Owned `String` rather
    /// than `&'static str` so the shape round-trips through
    /// serde without lifetime gymnastics — operators piping
    /// `--output json | jq` deserialize into the same type
    /// the CLI emits.
    pub channel_prefix: String,
    /// Current entry count.
    pub entries: u64,
    /// Apply count: outcome = inserted.
    pub applies_inserted: u64,
    /// Apply count: outcome = replaced an older entry.
    pub applies_replaced: u64,
    /// Apply count: outcome = rejected (existing entry won
    /// merge, generation out of order, etc.).
    pub applies_rejected: u64,
    /// Sum of inserted + replaced + rejected. Useful as the
    /// denominator for outcome ratios.
    pub applies_total: u64,
    /// TTL-driven expiries since fold construction. Bumped by
    /// the background sweeper.
    pub expiries: u64,
    /// Operator / SWIM-driven evictions since fold construction.
    pub evictions: u64,
    /// Query count.
    pub queries: u64,
    /// Snapshots taken via [`super::Fold::snapshot`].
    pub snapshots_taken: u64,
    /// Snapshots restored via [`super::Fold::restore`].
    pub snapshots_restored: u64,
    /// Expiry sweeps' candidate walks since fold construction: one per
    /// sweep. `#[serde(default)]` so JSON from before the field existed
    /// still deserializes.
    #[serde(default)]
    pub sweep_walks: u64,
    /// Entries those walks examined, live and expired alike: the
    /// sweep's entry-visit cost. Divided by `sweep_walks` it reads as
    /// the average fold size per sweep; growing faster than that
    /// means the sweep is re-walking entries.
    #[serde(default)]
    pub sweep_yielded: u64,
    /// Distinct values in the fold's admission dictionary (canonical
    /// tags, for the capability fold; 0 for folds without one).
    #[serde(default)]
    pub interned: u64,
    /// UTF-8 bytes of those values: what the admission budget counts.
    #[serde(default)]
    pub interned_bytes: u64,
    /// Estimated dictionary overhead beyond `interned_bytes` (table,
    /// allocation headers). Reported separately; not budget-counted.
    #[serde(default)]
    pub interned_overhead_bytes: u64,
    /// Announcements refused whole for a payload limit (tag count or
    /// length). Also counted in `applies_rejected`.
    #[serde(default)]
    pub limit_rejections: u64,
    /// Announcements refused whole for the admission budget. Also
    /// counted in `applies_rejected`.
    #[serde(default)]
    pub budget_rejections: u64,
    /// Whether an [`super::FoldAuditSink`] is currently installed
    /// on the fold. Diagnostic — operators trying to figure
    /// out why their audit trail is empty want a quick
    /// "nothing's listening" signal.
    pub has_audit_sink: bool,
}

/// Metric counters for one [`super::Fold`] instance. Counters
/// are independent atomics — readers (Prometheus scrape, the
/// Deck FOLDS panel, the metrics CLI) take a per-counter
/// snapshot via Relaxed loads.
///
/// Field naming matches the Prometheus metric names listed in
/// the plan's "Metrics" section: `fold_entries_total{kind}`,
/// `fold_applies_total{kind,outcome}`, etc. The `{kind}` label
/// is supplied by the [`FoldKind`](super::FoldKind) impl;
/// `{outcome}` is folded into separate counters here
/// (`applies_inserted` / `applies_replaced` / `applies_rejected`)
/// so the apply hot path is one atomic add against a fixed
/// address per outcome rather than a HashMap lookup keyed on a
/// label tuple.
#[derive(Debug, Default)]
pub struct FoldMetrics {
    /// Current entry count. Updated synchronously on every
    /// [`super::Fold::apply`] / [`super::Fold::evict_node`] /
    /// [`super::Fold::restore`] commit so the gauge is exact at
    /// every observation. Backed by an atomic so the metrics
    /// reader never has to acquire the state lock.
    entries: AtomicU64,
    /// Apply count by outcome: inserted.
    applies_inserted: AtomicU64,
    /// Apply count by outcome: replaced an older entry.
    applies_replaced: AtomicU64,
    /// Apply count by outcome: rejected (existing entry won
    /// the merge contest, generation was out of order, etc.).
    applies_rejected: AtomicU64,
    /// Entries removed because the TTL sweeper found
    /// `expires_at < now`.
    expiries: AtomicU64,
    /// Entries removed via [`super::Fold::evict_node`].
    /// Operator action / SWIM-declared-dead path bumps this.
    evictions: AtomicU64,
    /// Total queries served.
    queries: AtomicU64,
    /// Snapshots produced via [`super::Fold::snapshot`].
    snapshots_taken: AtomicU64,
    /// Snapshots applied via [`super::Fold::restore`].
    snapshots_restored: AtomicU64,
    /// Read-locked candidate walks the expiry sweep has run: one
    /// per sweep.
    sweep_walks: AtomicU64,
    /// Entries the expiry sweep's candidate walks have yielded,
    /// live and expired alike: the sweep's entry-visit cost. One walk
    /// yields every entry once.
    sweep_yielded: AtomicU64,
    /// Announcements refused for a payload limit.
    limit_rejections: AtomicU64,
    /// Announcements refused for the admission budget.
    budget_rejections: AtomicU64,
    /// The index's admission storage counters, published under the
    /// index lock after every operation that changes them, so
    /// [`super::Fold::stats`] reads them without taking any fold lock.
    admission_interned: AtomicU64,
    admission_interned_bytes: AtomicU64,
    admission_overhead_bytes: AtomicU64,
}

impl FoldMetrics {
    /// Construct a fresh counter set with every counter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bump the inserted-apply counter and increment the entry
    /// gauge. Called by [`super::Fold::apply`] on
    /// [`super::state::MergeAction::Insert`].
    #[inline]
    pub(super) fn on_insert(&self) {
        self.applies_inserted.fetch_add(1, Ordering::Relaxed);
        self.entries.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the replaced-apply counter. The entry gauge is
    /// unchanged because replace is "drop one, add one." Called
    /// by [`super::Fold::apply`] on
    /// [`super::state::MergeAction::Replace`].
    #[inline]
    pub(super) fn on_replace(&self) {
        self.applies_replaced.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the rejected-apply counter. The entry gauge is
    /// unchanged. Called by [`super::Fold::apply`] on
    /// [`super::state::MergeAction::Reject`] AND on the early
    /// rejections (invalid generation, etc.) that don't reach
    /// the merge step.
    #[inline]
    pub(super) fn on_reject(&self) {
        self.applies_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the entry gauge and bump the evictions counter.
    /// Called by [`super::Fold::evict_node`] once per entry
    /// removed.
    #[inline]
    pub(super) fn on_evict(&self) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        self.entries.fetch_sub(1, Ordering::Relaxed);
    }

    /// Decrement the entry gauge and bump the expiries counter.
    /// Called by [`super::expiry::sweep_expired`] once per
    /// expired entry. Distinct from [`Self::on_evict`] because
    /// expiries are TTL-driven and evictions are operator /
    /// SWIM-driven — operators tuning TTL want to see the two
    /// counters separately.
    #[inline]
    pub(super) fn on_expire(&self) {
        self.expiries.fetch_add(1, Ordering::Relaxed);
        self.entries.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record one expiry-sweep candidate walk that yielded
    /// `yielded` entries. Called once per sweep, by the sweep's
    /// collection phase, so the atomics are touched once per walk,
    /// not once per entry.
    #[inline]
    pub(super) fn on_sweep_walk(&self, yielded: u64) {
        self.sweep_walks.fetch_add(1, Ordering::Relaxed);
        self.sweep_yielded.fetch_add(yielded, Ordering::Relaxed);
    }

    /// Count an announcement refused for a payload limit.
    #[inline]
    pub(super) fn on_limit_reject(&self) {
        self.limit_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Count an announcement refused for the admission budget.
    #[inline]
    pub(super) fn on_budget_reject(&self) {
        self.budget_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Publish the index's admission counters. Called with the index
    /// lock held, after the operation that changed them.
    #[inline]
    pub(super) fn set_admission(&self, stats: super::state::AdmissionStats) {
        self.admission_interned
            .store(stats.interned, Ordering::Relaxed);
        self.admission_interned_bytes
            .store(stats.interned_bytes, Ordering::Relaxed);
        self.admission_overhead_bytes
            .store(stats.overhead_bytes, Ordering::Relaxed);
    }

    /// The admission counters last published under the index lock.
    pub fn admission(&self) -> super::state::AdmissionStats {
        super::state::AdmissionStats {
            interned: self.admission_interned.load(Ordering::Relaxed),
            interned_bytes: self.admission_interned_bytes.load(Ordering::Relaxed),
            overhead_bytes: self.admission_overhead_bytes.load(Ordering::Relaxed),
        }
    }

    /// Bump the query counter. Called by
    /// [`super::Fold::query`].
    #[inline]
    pub(super) fn on_query(&self) {
        self.queries.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the snapshots-taken counter. Called by
    /// [`super::Fold::snapshot`].
    #[inline]
    pub(super) fn on_snapshot_taken(&self) {
        self.snapshots_taken.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the snapshots-restored counter AND set the entry
    /// gauge to the post-restore entry count. Called by
    /// [`super::Fold::restore`] after the state mutation
    /// commits.
    #[inline]
    pub(super) fn on_snapshot_restored(&self, new_entry_count: u64) {
        self.snapshots_restored.fetch_add(1, Ordering::Relaxed);
        self.entries.store(new_entry_count, Ordering::Relaxed);
    }

    /// Current entry count. Cheap atomic load.
    pub fn entries(&self) -> u64 {
        self.entries.load(Ordering::Relaxed)
    }

    /// Inserted applies since start.
    pub fn applies_inserted(&self) -> u64 {
        self.applies_inserted.load(Ordering::Relaxed)
    }

    /// Replaced applies since start.
    pub fn applies_replaced(&self) -> u64 {
        self.applies_replaced.load(Ordering::Relaxed)
    }

    /// Rejected applies since start.
    pub fn applies_rejected(&self) -> u64 {
        self.applies_rejected.load(Ordering::Relaxed)
    }

    /// Sum of inserted + replaced + rejected. Useful as the
    /// denominator for outcome ratios.
    pub fn applies_total(&self) -> u64 {
        self.applies_inserted() + self.applies_replaced() + self.applies_rejected()
    }

    /// TTL-driven expiries since start.
    pub fn expiries(&self) -> u64 {
        self.expiries.load(Ordering::Relaxed)
    }

    /// Operator / SWIM-driven evictions since start.
    pub fn evictions(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
    }

    /// Query count since start.
    pub fn queries(&self) -> u64 {
        self.queries.load(Ordering::Relaxed)
    }

    /// Snapshot-taken count since start.
    pub fn snapshots_taken(&self) -> u64 {
        self.snapshots_taken.load(Ordering::Relaxed)
    }

    /// Snapshot-restored count since start.
    pub fn snapshots_restored(&self) -> u64 {
        self.snapshots_restored.load(Ordering::Relaxed)
    }

    /// Announcements refused for a payload limit since start.
    pub fn limit_rejections(&self) -> u64 {
        self.limit_rejections.load(Ordering::Relaxed)
    }

    /// Announcements refused for the admission budget since start.
    pub fn budget_rejections(&self) -> u64 {
        self.budget_rejections.load(Ordering::Relaxed)
    }

    /// Expiry-sweep candidate walks since start.
    pub fn sweep_walks(&self) -> u64 {
        self.sweep_walks.load(Ordering::Relaxed)
    }

    /// Entries yielded by expiry-sweep candidate walks since start,
    /// live and expired alike.
    pub fn sweep_yielded(&self) -> u64 {
        self.sweep_yielded.load(Ordering::Relaxed)
    }
}
