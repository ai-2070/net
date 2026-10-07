//! Background expiry sweeper.
//!
//! [`Fold<K>`](super::Fold) stamps an `expires_at: Instant` on
//! every applied entry; the background task spawned from
//! [`Fold::new`](super::Fold::new) walks the state on a
//! configurable cadence and evicts entries past that instant.
//!
//! The internal `sweep_expired` helper is a plain synchronous
//! drain of the fold's expiry wheel, so tests can drive expiry
//! deterministically without spinning a tokio runtime; the background
//! task is a thin loop on top of it. The task holds a [`Weak`] reference
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
use super::wheel::Drain;
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
pub(super) const SWEEP_CHUNK_SIZE: usize = 1024;

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
/// Driven by the fold's expiry wheel (CAPABILITY_FOLD_SCALE_PLAN.md,
/// Slice 8; see `wheel.rs`), not by a walk of the primary map:
///
/// 1. Under the state READ lock, ask the wheel whether anything is due.
///    It visits only the slots since the previous sweep. An idle sweep
///    stops here, so it never takes the write lock and never touches the
///    rest of the fold. Before Slice 8 it walked every entry (5.7 ms at
///    1M, twice a second, to find nothing).
/// 2. Otherwise, in chunks of at most [`SWEEP_CHUNK_SIZE`] entries, each
///    under its own state + index write-lock acquisition in the fixed
///    `state → index` order (matching the apply path): take the due keys
///    out of the wheel, and remove their entries, reverse-index keys and
///    index rows in the same critical section. Every lock release
///    therefore leaves one wheel node per entry, including between the
///    chunks of a multi-chunk sweep.
///
/// The wheel's due check is exact (`expires_at <= now`), so expiry is as
/// prompt as the full walk was. A refresh moves its entry's node under
/// the apply's write lock, so there is no stale candidate to re-check.
pub(super) fn sweep_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
) -> usize {
    let now = Instant::now();
    let (due, probed) = state_lock.read().probe_due(now);
    if !due {
        metrics.on_sweep_walk(probed as u64);
        return 0;
    }
    let mut visited = probed;
    let mut evicted = 0usize;
    let mut keys: Vec<K::Key> = Vec::new();
    loop {
        let chunk = evict_due_chunk(state_lock, index_lock, metrics, audit_sink, now, &mut keys);
        visited += chunk.drain.visited;
        evicted += chunk.evicted;
        if chunk.drain.exhausted {
            break;
        }
    }
    metrics.on_sweep_walk(visited as u64);
    evicted
}

/// One chunk of [`sweep_expired`], from [`evict_due_chunk`].
#[derive(Debug, Clone, Copy)]
pub(super) struct Chunk {
    /// What the wheel did: nodes visited, and whether it ran dry.
    pub(super) drain: Drain,
    /// Entries removed.
    pub(super) evicted: usize,
}

/// Under ONE state + index write-lock acquisition, take up to
/// [`SWEEP_CHUNK_SIZE`] keys due at `now` out of the wheel and remove
/// their entries, reverse-index keys and index rows. `keys` is scratch,
/// reused across chunks. On return the wheel holds exactly one node per
/// entry again, so the invariant holds at every lock release.
pub(super) fn evict_due_chunk<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
    now: Instant,
    keys: &mut Vec<K::Key>,
) -> Chunk {
    let mut state = state_lock.write();
    let mut index = index_lock.write();
    keys.clear();
    let drain = state.take_due(now, SWEEP_CHUNK_SIZE, keys);
    let mut evicted = 0usize;
    for key in keys.drain(..) {
        // The wheel holds exactly one node per entry, so a due key
        // always names a resident entry.
        let Some(old_entry) = state.entries.remove(&key) else {
            debug_assert!(false, "expiry wheel named a missing entry");
            continue;
        };
        state.detach_key(old_entry.node_id, &key);
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
        evicted += 1;
    }
    debug_assert_eq!(state.scheduled_len(), state.entries.len());
    Chunk { drain, evicted }
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
