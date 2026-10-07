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

/// How many segments one expiry walk is split into. The walk releases
/// the state read lock between segments, so a writer queued behind the
/// sweep (and every reader queued behind that writer, the read lock
/// being fair) waits for at most one segment, not a walk of the whole
/// map. Segmenting also bounds the candidate list to one segment's keys.
///
/// The count is small and fixed because resuming is not free: segment
/// `k` re-skips the `k` before it, and a skip costs about a quarter of
/// examining an entry (1.5 ns against 5.7 ns at 1M; it scans control
/// bytes and touches no entry). Over `S` segments the skips total about
/// `N·(S − 1)/2`, so 4 segments add roughly 40% to a steady-state walk
/// and cut its longest hold to a quarter. Fixed 16k-entry segments made
/// the skips quadratic: a 1M steady sweep went from 5.7 ms to 50 ms.
const SWEEP_WALK_SEGMENTS: usize = 4;

/// Smallest segment the walk uses. A fold this size or smaller walks in
/// one hold, which is already short (about 0.4 ms at 64k entries).
const SWEEP_WALK_SEGMENT_MIN: usize = 64 * 1024;

/// Segment length for a fold holding about `entries` entries.
fn walk_segment(entries: u64) -> usize {
    let entries = usize::try_from(entries).unwrap_or(usize::MAX);
    entries
        .div_ceil(SWEEP_WALK_SEGMENTS)
        .max(SWEEP_WALK_SEGMENT_MIN)
}

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
/// One logical walk of the primary store per sweep
/// (CAPABILITY_FOLD_SCALE_PLAN.md, Slice 2), in at most
/// [`SWEEP_WALK_SEGMENTS`] segments (see there for the trade-off). For
/// each segment:
///
/// 1. [`collect_expired_segment`]: under one state read-lock hold,
///    skip the entries earlier segments examined and collect the
///    expired keys among the next segment's worth.
/// 2. [`evict_chunk`]: the collected keys in [`SWEEP_CHUNK_SIZE`]
///    slices, each under its own state + index write-lock acquisition
///    in the fixed `state → index` order (matching the apply path),
///    released between slices.
///
/// Each entry is examined once per sweep. The previous design
/// restarted the examining walk from the first bucket for every chunk,
/// so a sweep over E expired entries among N re-examined the live
/// prefix about E/1024 times (~(N − E)·E/2048 visits; 45.7M at N = 1M,
/// E = 100k). Resuming a segment re-skips the earlier ones; a skip only
/// scans the table's control bytes and touches no entry, and the
/// segment count is capped so the skips stay linear in N.
///
/// The resume point is a count of occupied slots, which is exact
/// against this sweep's own evictions (removal leaves every other slot
/// where it was, and the count is reduced by what was evicted) but only
/// approximate against concurrent writers between segments: an insert
/// before the resume point, or a resize, shifts it. The cost of that is
/// bounded and benign: an entry examined twice is evicted once (phase
/// 2 re-checks), and an entry skipped is evicted by the next sweep.
///
/// Between a segment's walk and a key's eviction, a concurrent apply
/// may refresh the entry's TTL, or `evict_node` may remove it. Phase 2
/// re-checks `expires_at <= now` per key under the write lock and
/// skips refreshed or vanished entries.
pub(super) fn sweep_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
) -> usize {
    sweep_expired_in_segments(
        state_lock,
        index_lock,
        metrics,
        audit_sink,
        walk_segment(metrics.entries()),
    )
}

/// [`sweep_expired`] with an explicit segment size, so tests can drive
/// a multi-segment walk over a small fold.
pub(super) fn sweep_expired_in_segments<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
    segment: usize,
) -> usize {
    let now = Instant::now();
    let mut resume = 0usize;
    let mut examined_total = 0u64;
    let mut evicted_total = 0usize;
    loop {
        let walk = collect_expired_segment(state_lock, resume, segment, now);
        examined_total += walk.examined as u64;
        let evicted: usize = walk
            .expired
            .chunks(SWEEP_CHUNK_SIZE)
            .map(|chunk| evict_chunk(state_lock, index_lock, metrics, audit_sink, chunk, now))
            .sum();
        evicted_total += evicted;
        if walk.exhausted {
            break;
        }
        // Every key this segment evicted sat before the resume point,
        // and each removal took one occupied slot out of that prefix.
        resume = (resume + walk.examined).saturating_sub(evicted);
    }
    metrics.on_sweep_walk(examined_total);
    evicted_total
}

/// One segment of the expiry walk, from [`collect_expired_segment`].
pub(super) struct WalkSegment<Q> {
    /// Keys whose entries had `expires_at <= now` when examined.
    pub(super) expired: Vec<Q>,
    /// Entries examined (live and expired alike), excluding skipped.
    pub(super) examined: usize,
    /// The walk reached the end of the table.
    pub(super) exhausted: bool,
}

/// Phase 1 of [`sweep_expired`]: under one state read-lock hold, skip
/// `skip` entries of the primary store, then examine up to `limit`
/// more and collect every key whose entry has `expires_at <= now`.
pub(super) fn collect_expired_segment<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    skip: usize,
    limit: usize,
    now: Instant,
) -> WalkSegment<K::Key> {
    let state = state_lock.read();
    let mut walk = state.entries.iter().skip(skip);
    let mut expired = Vec::new();
    // Count what the walk actually yields, before the expiry filter.
    // Recording a precomputed length instead would report the walk's
    // expected cost, and a regression that visited entries twice
    // would still report the right number.
    let mut examined = 0usize;
    for (key, entry) in walk.by_ref().take(limit) {
        examined += 1;
        if entry.expires_at <= now {
            expired.push(key.clone());
        }
    }
    let exhausted = examined < limit || walk.next().is_none();
    WalkSegment {
        expired,
        examined,
        exhausted,
    }
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
