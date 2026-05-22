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

/// Synchronous core of the expiry sweep. Walks the primary
/// store, finds entries whose `expires_at < now`, removes them
/// from `entries` + `by_node`, calls
/// `K::Index::on_remove` for each, bumps the
/// [`FoldMetrics::expiries`] counter, and surfaces an
/// `EntryTransition::Expired` to the audit sink.
///
/// Returns the number of entries evicted. Used by both the
/// background task (one call per wake) and tests (called
/// directly via [`super::Fold::sweep_expired_now`]).
///
/// Locking: takes the state + index write locks in the fixed
/// `state → index` order that the apply path uses, so the
/// sweeper composes safely with concurrent applies + queries.
pub(super) fn sweep_expired<K: FoldKind>(
    state_lock: &RwLock<FoldState<K>>,
    index_lock: &RwLock<K::Index>,
    metrics: &FoldMetrics,
    audit_sink: Option<&Arc<dyn FoldAuditSink>>,
) -> usize {
    let now = Instant::now();
    let mut state = state_lock.write();
    let mut index = index_lock.write();

    // Two-pass: collect keys whose expires_at is past before
    // mutating. The mut-walk-and-remove pattern can't run inside
    // a single `retain` here because we also need to update the
    // `by_node` reverse index + the secondary index + emit audit
    // events per evicted entry — all of which need ownership of
    // the entry value, which HashMap::retain doesn't surrender.
    let expired_keys: Vec<K::Key> = state
        .entries
        .iter()
        .filter_map(|(k, e)| {
            if e.expires_at <= now {
                Some(k.clone())
            } else {
                None
            }
        })
        .collect();

    let evicted = expired_keys.len();
    for key in expired_keys {
        // `remove` after the prior `contains_key`-shaped filter
        // succeeds is infallible by construction; let-else
        // guards a future change that loses the invariant.
        let Some(old_entry) = state.entries.remove(&key) else {
            continue;
        };
        // by_node reverse index cleanup — mirrors evict_node.
        if let Some(keys) = state.by_node.get_mut(&old_entry.node_id) {
            keys.remove(&key);
            if keys.is_empty() {
                state.by_node.remove(&old_entry.node_id);
            }
        }
        // Secondary index hook before the entry is fully dropped.
        index.on_remove(&key, &old_entry.payload);
        // Audit emission while we still hold a borrow on the entry.
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
