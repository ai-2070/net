//! Background expiry sweeper.
//!
//! [`Fold<K>`](super::Fold) stamps an `expires_at: Instant` on
//! every applied entry; the background task spawned from
//! [`Fold::new`](super::Fold::new) walks the state on a
//! configurable cadence and evicts entries past that instant.
//!
//! The internal `sweep_expired` helper is a plain synchronous
//! walk-and-remove so tests can drive expiry deterministically
//! without spinning a tokio runtime; the background task is a
//! thin loop on top of it. The task holds a [`Weak`] reference
//! to the fold's inner state so it exits naturally when the last
//! [`Fold<K>`](super::Fold) drops.
//!
//! [`DEFAULT_SWEEP_INTERVAL`] (500 ms) is a compromise between
//! "TTL boundary observable within a human-perceivable window"
//! and "no per-second wakes when the fold is idle."
//! [`super::Fold::with_sweep_interval`] overrides per fold.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::watch;

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
/// shorten the window during which applies + queries block.
///
/// This bounds the entry count per hold, not the hold's duration:
/// each eviction runs the fold's index removal and drops the
/// entry's payload, so a hold costs `SWEEP_CHUNK_SIZE` times the
/// per-entry removal cost of the fold's payload. For the
/// capability fold that cost is several microseconds per entry
/// (CAPABILITY_FOLD_SCALE_PLAN.md, Slice 0), which puts a full
/// chunk in the milliseconds, not below one. No hold-duration
/// guarantee is made until chunk holds are measured directly.
const SWEEP_CHUNK_SIZE: usize = 1024;

/// Synchronous core of the expiry sweep: evicts every entry whose
/// `expires_at <= now` from `entries` + `by_node`, calling
/// `K::Index::on_remove` for each, bumping
/// [`FoldMetrics::expiries`], and surfacing an
/// `EntryTransition::Expired` to the audit sink.
///
/// Returns the total number of entries evicted. Used by both the
/// background task (one call per wake) and tests (called
/// directly via [`super::Fold::sweep_expired_now`]).
///
/// Two phases (CAPABILITY_FOLD_SCALE_PLAN.md, Slice 2):
///
/// 1. [`collect_expired`]: ONE walk of the primary store under the
///    state read lock, collecting every expired key. Each entry is
///    yielded once per sweep. The previous design restarted the walk
///    from the first bucket for every chunk, so a sweep over E expired
///    entries among N re-yielded the live prefix about E/1024 times
///    (~(N − E)·E/2048 yields; 45.7M at N = 1M, E = 100k).
/// 2. [`evict_chunk`]: the collected keys in [`SWEEP_CHUNK_SIZE`]
///    slices, each under its own state + index write-lock acquisition
///    in the fixed `state → index` order (matching the apply path),
///    released between slices.
///
/// The cost of phase 1 is one read-lock hold for the whole walk
/// instead of one per chunk. The read lock is fair, so a writer
/// queued during the walk also holds back readers that arrive after
/// it.
///
/// Between the walk and a key's eviction, a concurrent apply may
/// refresh the entry's TTL, or `evict_node` may remove it. Phase 2
/// re-checks `expires_at <= now` per key under the write lock and
/// skips refreshed or vanished entries.
pub(super) fn sweep_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
) -> usize {
    let now = Instant::now();
    let candidates = collect_expired(state_lock, metrics, now);
    candidates
        .chunks(SWEEP_CHUNK_SIZE)
        .map(|chunk| evict_chunk(state_lock, index_lock, metrics, audit_sink, chunk, now))
        .sum()
}

/// Phase 1 of [`sweep_expired`]: walk the primary store once under
/// the state read lock and return every key whose entry has
/// `expires_at <= now`. Records one walk and the number of entries it
/// yielded on `metrics`.
pub(super) fn collect_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    metrics: &FoldMetrics,
    now: Instant,
) -> Vec<K::Key> {
    let state = state_lock.read();
    // Count what the walk actually yields, before the expiry filter.
    // Recording `entries.len()` instead would report the walk's
    // expected cost, and a regression that visited entries twice
    // would still report the right number.
    let mut yielded = 0u64;
    let candidates: Vec<K::Key> = state
        .entries
        .iter()
        .inspect(|_| yielded += 1)
        .filter(|(_, e)| e.expires_at <= now)
        .map(|(k, _)| k.clone())
        .collect();
    metrics.on_sweep_walk(yielded);
    candidates
}

/// Phase 2 of [`sweep_expired`]: under one state + index write-lock
/// acquisition, evict each key of `chunk` whose entry is still
/// present and still expired as of `now`. Returns the number evicted.
///
/// The re-check is load-bearing: the keys were collected under an
/// earlier read lock, and a concurrent apply may have refreshed an
/// entry's TTL (or `evict_node` removed it) since.
pub(super) fn evict_chunk<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
    chunk: &[K::Key],
    now: Instant,
) -> usize {
    let mut state = state_lock.write();
    let mut index = index_lock.write();
    let mut evicted = 0usize;
    for key in chunk {
        let still_expired = state.entries.get(key).is_some_and(|e| e.expires_at <= now);
        if !still_expired {
            continue;
        }
        let Some(old_entry) = state.entries.remove(key) else {
            continue;
        };
        state.detach_key(old_entry.node_id, key);
        index.on_remove(key, &old_entry.payload);
        if let Some(sink) = audit_sink {
            let transition = EntryTransition::Expired {
                key,
                old: &old_entry,
            };
            if let Some(event) = K::audit_event(transition) {
                sink.record(event);
            }
        }
        metrics.on_expire();
        evicted += 1;
    }
    evicted
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
    change_tx: Weak<watch::Sender<u64>>,
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
            let reaped = sweep_expired::<K>(&state, &index, &metrics, sink_ref);
            // Wake fold-change subscribers if this sweep actually
            // removed anything — a TTL-expired peer's tools should
            // surface as a `Removed` on any `watch_*` consumer
            // without waiting for the debounce-ceiling fallback.
            if reaped > 0 {
                if let Some(tx) = change_tx.upgrade() {
                    tx.send_modify(|g| *g = g.wrapping_add(1));
                }
            }
        }
    })
}
