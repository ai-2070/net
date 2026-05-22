//! Per-fold metric counters.
//!
//! Phase 1 ships the counters that the apply / query / evict /
//! snapshot paths bump synchronously. Histograms (apply duration,
//! query duration, subscription lag) are recorded as raw counts
//! here and surfaced by the Prometheus / Deck adapters when the
//! observability layer plumbs the rest of Phase 6's pipework
//! through.
//!
//! All counters are lock-free atomics so the apply hot path never
//! contends with metrics readers.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Serializable per-fold snapshot of the live [`FoldMetrics`]
/// counters + a small amount of static identity (`kind` u16 +
/// channel prefix). Per Phase 6a of the multifold plan, this
/// is the shape the operator-facing surface (the
/// `net fold list` / `net fold stats <kind>` CLI commands, the
/// Deck FOLDS panel, the Prometheus exporter) reads when it
/// wants a coherent picture of one fold.
///
/// Sampled via [`FoldMetrics::snapshot`] for the fold-side
/// view, or aggregated across the registry via
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
    /// `expires_at < now`. Phase 1B follow-up populates this.
    expiries: AtomicU64,
    /// Entries removed via [`super::Fold::evict_node`].
    /// Operator action / SWIM-declared-dead path bumps this.
    evictions: AtomicU64,
    /// Total queries served. Read-only counter; the per-query
    /// duration histogram is a Phase 1B follow-up.
    queries: AtomicU64,
    /// Snapshots produced via [`super::Fold::snapshot`].
    snapshots_taken: AtomicU64,
    /// Snapshots applied via [`super::Fold::restore`].
    snapshots_restored: AtomicU64,
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
    /// Called by the Phase 1B background sweep
    /// ([`super::expiry::sweep_expired`]) once per expired
    /// entry removed. Distinct from [`Self::on_evict`] because
    /// expiries are TTL-driven and evictions are operator /
    /// SWIM-driven — operators tuning TTL want to see the two
    /// counters separately.
    #[inline]
    pub(super) fn on_expire(&self) {
        self.expiries.fetch_add(1, Ordering::Relaxed);
        self.entries.fetch_sub(1, Ordering::Relaxed);
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

    /// TTL-driven expiries since start. Phase 1B populates this
    /// once the sweeper lands; Phase 1 always reports `0`.
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

    /// Materialize a [`FoldStats`] snapshot of every counter
    /// plus the supplied static identity (`kind` /
    /// `channel_prefix`) and runtime flag (`has_audit_sink`).
    /// Called by [`super::Fold::stats`] and by the
    /// per-fold [`super::FoldDispatch::stats`] adapter — the
    /// shape the operator surface consumes.
    pub fn snapshot(
        &self,
        kind: u16,
        channel_prefix: &'static str,
        has_audit_sink: bool,
    ) -> FoldStats {
        FoldStats {
            kind,
            channel_prefix: channel_prefix.to_string(),
            entries: self.entries(),
            applies_inserted: self.applies_inserted(),
            applies_replaced: self.applies_replaced(),
            applies_rejected: self.applies_rejected(),
            applies_total: self.applies_total(),
            expiries: self.expiries(),
            evictions: self.evictions(),
            queries: self.queries(),
            snapshots_taken: self.snapshots_taken(),
            snapshots_restored: self.snapshots_restored(),
            has_audit_sink,
        }
    }
}
