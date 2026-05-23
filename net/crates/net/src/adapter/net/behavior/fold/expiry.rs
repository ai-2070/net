//! Background expiry sweeper.
//!
//! [`Fold<K>`](super::Fold) stamps an `expires_at: Instant` on
//! every applied entry; the background task spawned from
//! [`Fold::new`](super::Fold::new) walks the state on a
//! configurable cadence and evicts entries past that instant.
//!
//! [`sweep_expired`] is a plain synchronous walk-and-remove so
//! tests can drive expiry deterministically without spinning a
//! tokio runtime; the background task is a thin loop on top of
//! it. The task holds a [`Weak`] reference to the fold's inner
//! state so it exits naturally when the last [`Fold<K>`] drops.
//!
//! [`DEFAULT_SWEEP_INTERVAL`] (500 ms) is a compromise between
//! "TTL boundary observable within a human-perceivable window"
//! and "no per-second wakes when the fold is idle."
//! [`super::Fold::with_sweep_interval`] overrides per fold.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use super::audit::FoldAuditSink;
use super::state::{EntryTransition, FoldIndex, FoldState};
use super::FoldKind;
use super::FoldMetrics;

/// Default cadence the background sweeper wakes at. See module
/// doc for the trade-off rationale; tests and workloads that need
/// tighter expiry tracking override per-fold via
/// [`super::Fold::with_sweep_interval`].
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_millis(500);

/// Maximum number of entries evicted per write-lock acquisition.
/// Larger batches amortize the lock-acquire cost; smaller batches
/// shorten the worst-case window during which applies + queries
/// block. 1024 is a compromise that on a 100k-entry fold caps
/// each write-lock hold to sub-millisecond while still finishing
/// a full sweep in ~100 read-then-write cycles.
const SWEEP_CHUNK_SIZE: usize = 1024;

/// Synchronous core of the expiry sweep. Walks the primary store
/// in [`SWEEP_CHUNK_SIZE`]-bounded batches, evicting entries
/// whose `expires_at <= now` from `entries` + `by_node`, calling
/// `K::Index::on_remove` for each, bumping
/// [`FoldMetrics::expiries`], and surfacing an
/// `EntryTransition::Expired` to the audit sink.
///
/// Returns the total number of entries evicted. Used by both the
/// background task (one call per wake) and tests (called
/// directly via [`super::Fold::sweep_expired_now`]).
///
/// Locking: each chunk acquires the state + index write locks in
/// the fixed `state → index` order (matching the apply path),
/// then releases them before the next chunk's read-lock pass.
/// Concurrent applies / queries see a series of short pauses
/// rather than one full-state stall. Between the read-lock pass
/// that picks the candidates and the write-lock pass that
/// removes them, a concurrent apply may refresh an entry's TTL —
/// the write-lock pass re-checks `expires_at <= now` per key and
/// skips refreshed entries.
pub(super) fn sweep_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
) -> usize {
    let now = Instant::now();
    let mut total_evicted = 0usize;
    loop {
        // Phase 1: read-lock, collect a bounded batch of
        // candidates whose expires_at is past `now`. Read lock is
        // released at end of this scope before we take write
        // locks below.
        let candidates: Vec<K::Key> = {
            let state = state_lock.read();
            state
                .entries
                .iter()
                .filter(|(_, e)| e.expires_at <= now)
                .map(|(k, _)| k.clone())
                .take(SWEEP_CHUNK_SIZE)
                .collect()
        };
        if candidates.is_empty() {
            return total_evicted;
        }

        // Phase 2: write-lock, re-check + mutate. Re-check is
        // load-bearing: between the read-lock release and write-
        // lock acquire, a concurrent apply may have refreshed
        // the entry's TTL (or evict_node may have removed it).
        let mut state = state_lock.write();
        let mut index = index_lock.write();
        for key in candidates {
            let still_expired = state
                .entries
                .get(&key)
                .map(|e| e.expires_at <= now)
                .unwrap_or(false);
            if !still_expired {
                continue;
            }
            let Some(old_entry) = state.entries.remove(&key) else {
                continue;
            };
            if let Some(keys) = state.by_node.get_mut(&old_entry.node_id) {
                keys.remove(&key);
                if keys.is_empty() {
                    state.by_node.remove(&old_entry.node_id);
                }
            }
            index.on_remove(&key, &old_entry.payload);
            if let Some(sink) = audit_sink {
                let transition = EntryTransition::Expired {
                    key: &key,
                    old: &old_entry,
                };
                if let Some(event) = K::audit_event(transition) {
                    sink.record(event);
                }
            }
            metrics.on_expire();
            total_evicted += 1;
        }
        // Locks drop here. The next loop iteration re-reads to
        // pick up the next chunk; if the read pass finds nothing,
        // we return.
    }
}

/// Spawn the per-fold background sweep task onto the ambient
/// tokio runtime. Returns the `JoinHandle` so [`super::Fold`]
/// can abort it on drop.
///
/// The task holds `Weak` references to the state / index /
/// metrics so a dropped fold doesn't keep its own task alive:
/// each iteration upgrades the weaks; if any `upgrade()` returns
/// `None`, the task exits.
pub(super) fn spawn_expiry_task<K: FoldKind>(
    state: Weak<RwLock<FoldState<K>>>,
    index: Weak<RwLock<K::Index>>,
    metrics: Weak<FoldMetrics>,
    audit_sink: Weak<parking_lot::RwLock<Option<Arc<dyn FoldAuditSink>>>>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick — `interval` fires at
        // `t=0` by default, which would run a pointless sweep on
        // a freshly-constructed empty fold.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let (Some(state), Some(index), Some(metrics)) =
                (state.upgrade(), index.upgrade(), metrics.upgrade())
            else {
                // Owner dropped — exit cleanly.
                break;
            };
            // Audit-sink upgrade is optional: if the sink slot
            // itself has been dropped, we still sweep, we just
            // don't emit audit events. The sink slot dropping
            // before the state implies a malformed construction
            // path; defensively drop to None rather than
            // panicking on a partial-shutdown invariant break.
            let sink_holder = audit_sink.upgrade();
            let sink_guard = sink_holder.as_ref().map(|h| h.read());
            let sink_ref = sink_guard.as_ref().and_then(|g| g.as_ref());
            sweep_expired::<K>(&state, &index, &metrics, sink_ref);
        }
    })
}
