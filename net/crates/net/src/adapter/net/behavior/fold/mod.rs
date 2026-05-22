//! Multi-fold framework.
//!
//! A generic state-aggregation runtime parameterized by the
//! [`FoldKind`] trait. One implementation handles apply, query,
//! snapshot, audit emission, TTL expiry, and node eviction for
//! every concrete fold. The three built-in folds —
//! [`CapabilityFold`], [`RoutingFold`], [`ReservationFold`] —
//! plug in by implementing the trait.
//!
//! See `docs/plans/SCALING_MULTIFOLD_PLAN.md` for the design
//! rationale.

use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub mod audit;
pub mod capability;
pub mod capability_bridge;
pub mod dispatch;
pub mod expiry;
pub mod metrics;
pub mod reservation;
pub mod routing;
pub mod snapshot;
pub mod state;
pub mod wire;

#[cfg(test)]
mod tests;

pub use wire::{EnvelopeMeta, SignedAnnouncement, WireError};
pub use audit::{FoldAuditSink, NoopSink, RingFoldAuditSink, VecFoldAuditSink};
pub use capability::{
    reflex_addr_for, CapabilityFilter, CapabilityFold, CapabilityIndexInner, CapabilityMatch,
    CapabilityMembership, CapabilityQuery, HardwareSummary, NodeState,
};
pub use dispatch::{
    DispatchError, FoldChannelRouter, FoldDispatch, FoldDispatchAdapter, FoldRegistry,
    SUBPROTOCOL_FOLD,
};
pub use expiry::DEFAULT_SWEEP_INTERVAL;
pub use metrics::{FoldMetrics, FoldStats};
pub use reservation::{
    JobId, ReservationAnnouncement, ReservationFold, ReservationQuery, ReservationRow,
    ReservationState, ResourceId,
};
pub use routing::{RouteAnnouncement, RouteRow, RoutingFold, RoutingQuery};
pub use snapshot::{FoldSnapshot, FoldSnapshotEntry};
pub use state::{
    ApplyOutcome, EntryTransition, FoldEntry, FoldError, FoldIndex, FoldState, MergeAction,
    NoIndex, NodeId,
};

/// One typed fold definition. Each concrete fold (capability,
/// routing, reservation, future kinds) is a unit type that
/// implements this trait; the runtime materializes a [`Fold<K>`]
/// per impl and routes incoming announcements through the
/// associated payload type.
///
/// Every method has a sensible default for folds that don't
/// override it; the only required choices are the associated
/// types and [`Self::key_for`] / [`Self::build_index`] /
/// [`Self::query`], which are inherently fold-specific.
///
/// See the plan's "The `FoldKind` trait" section for the design
/// rationale on each field.
pub trait FoldKind: Send + Sync + Sized + 'static {
    /// Stable u16 identifier on the wire. Reserved ranges (per
    /// the plan): `0x0000..=0x00FF` for built-in folds
    /// (capability=1, routing=2, reservation=3); `0x0100..=0xFFFF`
    /// for future / custom folds.
    const KIND_ID: u16;

    /// Channel-name prefix for this fold's announcements. The
    /// per-class channel is `format!("{}{}", CHANNEL_PREFIX,
    /// class)`. Subnet scope is NOT encoded in the channel name
    /// — the substrate's existing `NetHeader.subnet_id` plus
    /// `ChannelConfig::visibility` handle scoping at the gateway
    /// layer.
    const CHANNEL_PREFIX: &'static str;

    /// Default TTL for entries in this fold. Per-announcement
    /// `ttl_secs` overrides this when present.
    const DEFAULT_TTL: Duration;

    /// Indexing key. Must be hashable + cloneable +
    /// serializable so the snapshot envelope can round-trip
    /// keys without an additional codec. `Debug` is required so
    /// the runtime's diagnostic output ([`FoldState`] /
    /// [`FoldSnapshot`] Debug impls, audit event `key_repr`)
    /// compiles without per-fold escape hatches.
    type Key: Hash + Eq + Clone + std::fmt::Debug + Send + Sync + Serialize + DeserializeOwned;

    /// Domain-specific payload carried in announcements. See
    /// [`Self::Key`] for the `Debug` rationale.
    type Payload: Clone + std::fmt::Debug + Send + Sync + Serialize + DeserializeOwned;

    /// Query type the caller passes to [`Fold::query`].
    type Query: Send + Sync;

    /// Query result type the [`Self::query`] impl returns.
    type Result: Send + Sync;

    /// Secondary index — [`NoIndex`] is the default for folds
    /// that don't maintain anything beyond the primary store.
    type Index: FoldIndex<Self>;

    /// Derive the indexing key from an announcement. The
    /// publisher's `node_id` is passed separately so folds
    /// keyed solely on the payload (like [`RoutingFold`]'s
    /// `destination`) don't have to peel it back out.
    fn key_for(node_id: NodeId, payload: &Self::Payload) -> Self::Key;

    /// Construct a fresh secondary index. Called once at
    /// [`Fold::new`] and again on [`Fold::restore`] before
    /// re-populating from a snapshot.
    fn build_index() -> Self::Index;

    /// How the runtime should treat an incoming announcement vs
    /// the existing entry at its key. Default: last-write-wins
    /// by generation, which is what capability + reservation
    /// want; routing overrides to prefer the lower metric.
    fn merge(
        existing: Option<&FoldEntry<Self>>,
        incoming: &SignedAnnouncement<Self::Payload>,
    ) -> MergeAction {
        match existing {
            None => MergeAction::Insert,
            Some(e) if incoming.generation > e.generation => MergeAction::Replace,
            _ => MergeAction::Reject,
        }
    }

    /// Evaluate a [`Self::Query`] against the current
    /// [`FoldState`] + [`Self::Index`]. Read-only.
    fn query(state: &FoldState<Self>, index: &Self::Index, query: Self::Query) -> Self::Result;

    /// Optional audit emission. Returned events flow to the
    /// installed [`FoldAuditSink`]. Default: emit nothing.
    fn audit_event(transition: EntryTransition<'_, Self>) -> Option<AuditEvent> {
        let _ = transition;
        None
    }
}

/// Tag identifying which transition produced an [`AuditEvent`].
/// The five canonical variants cover the apply / sweep / evict
/// transitions the runtime emits; folds that want to surface
/// additional events (e.g. a `ReservationFold` takeover from an
/// expired holder) emit them via [`AuditKind::Custom`] without
/// having to widen the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    /// First-time install of a key.
    Created,
    /// Existing entry replaced under merge rules.
    Replaced,
    /// Incoming announcement rejected (stale generation, illegal
    /// transition, etc.).
    Rejected,
    /// Entry removed via [`Fold::evict_node`] (SWIM / operator).
    Evicted,
    /// Entry removed because `expires_at` lapsed.
    Expired,
    /// Fold-specific transition outside the runtime's canonical
    /// set. The `&'static str` is the fold's chosen tag (e.g.
    /// `"reservation_takeover"`).
    Custom(&'static str),
}

/// Audit-event payload emitted by [`FoldKind::audit_event`] for
/// each transition the fold wants surfaced. The runtime collects
/// them into the per-apply outcome and forwards to the installed
/// [`FoldAuditSink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// Which transition produced this event. See [`AuditKind`].
    pub kind: AuditKind,
    /// Debug-shaped key representation. The runtime can't
    /// `Debug`-format `K::Key` without an extra bound; impls
    /// that want richer audit detail materialize the string
    /// themselves.
    pub key_repr: String,
    /// Optional operator-readable detail string (e.g. the
    /// `reason` passed to [`Fold::evict_node`], or
    /// `"generation 7 → 8"` for a replace).
    pub detail: Option<String>,
}

/// Runtime instance of a single fold.
///
/// Holds the primary state ([`FoldState`]) and the secondary
/// index ([`FoldKind::Index`]) behind separate `RwLock`s so
/// query traffic can run in parallel with index reads. The apply
/// path takes both write locks in the fixed order `state →
/// index`; query takes both read locks in the same order.
/// Mixing those orders would deadlock; the runtime never does.
///
/// An optional [`FoldAuditSink`] receives transition events
/// emitted by [`FoldKind::audit_event`], and a background tokio
/// task wakes every [`DEFAULT_SWEEP_INTERVAL`] (or the per-fold
/// override) to evict entries past their TTL. The task holds
/// `Weak` references to the shared state, so dropping the fold
/// ends the task naturally — no explicit shutdown signal needed.
pub struct Fold<K: FoldKind> {
    state: Arc<RwLock<FoldState<K>>>,
    index: Arc<RwLock<K::Index>>,
    metrics: Arc<FoldMetrics>,
    /// Optional audit-event destination installed via
    /// [`Self::set_audit_sink`]. Default `None`; `K::audit_event`
    /// invocations are then no-ops at the call site.
    audit_sink: Arc<RwLock<Option<Arc<dyn FoldAuditSink>>>>,
    /// Background-sweeper join handle. Aborted in [`Self::drop`]
    /// so tests that construct + drop many folds rapidly don't
    /// accumulate stale tasks (the weakly-referenced state
    /// inside the task would let it exit on its own, but only
    /// after one sweep-interval tick).
    sweep_handle: Option<tokio::task::JoinHandle<()>>,
}

impl<K: FoldKind> Fold<K> {
    /// Build a new fold instance with empty state and a fresh
    /// index, spawning the background expiry sweeper at the
    /// [`DEFAULT_SWEEP_INTERVAL`] cadence. For tighter cadences
    /// (tests, latency-sensitive folds) use
    /// [`Self::with_sweep_interval`].
    pub fn new() -> Self {
        Self::with_sweep_interval(DEFAULT_SWEEP_INTERVAL)
    }

    /// Build a new fold instance with a custom sweep cadence.
    /// `interval = Duration::ZERO` disables the background
    /// sweeper entirely — callers that want explicit control
    /// (e.g. tests that drive expiry via
    /// [`Self::sweep_expired_now`]) opt out this way.
    pub fn with_sweep_interval(interval: Duration) -> Self {
        let state = Arc::new(RwLock::new(FoldState::new()));
        let index = Arc::new(RwLock::new(K::build_index()));
        let metrics = Arc::new(FoldMetrics::new());
        let audit_sink: Arc<RwLock<Option<Arc<dyn FoldAuditSink>>>> = Arc::new(RwLock::new(None));

        // Spawn the background sweeper only on a non-zero interval
        // AND from inside a tokio runtime. Synchronous test paths
        // (without a runtime) drive expiry via `sweep_expired_now`;
        // the inbound dispatch path always has a runtime, so
        // production never hits the `None` branch.
        let sweep_handle = if interval.is_zero()
            || tokio::runtime::Handle::try_current().is_err()
        {
            None
        } else {
            Some(expiry::spawn_expiry_task::<K>(
                Arc::downgrade(&state),
                Arc::downgrade(&index),
                Arc::downgrade(&metrics),
                Arc::downgrade(&audit_sink),
                interval,
            ))
        };

        Self {
            state,
            index,
            metrics,
            audit_sink,
            sweep_handle,
        }
    }

    /// Apply a verified signed announcement to the fold.
    ///
    /// Rejects the wire-format `generation == 0` sentinel, then
    /// acquires `state` and `index` write locks (in that fixed
    /// order) and consults [`FoldKind::merge`] against the
    /// existing entry. Insert / Replace updates the primary
    /// store, `by_node` reverse index, secondary index, and
    /// metric counters; Reject bumps the rejected-applies
    /// counter and returns [`ApplyOutcome::Rejected`]. Any
    /// [`FoldKind::audit_event`] result is forwarded to the
    /// installed [`FoldAuditSink`].
    ///
    /// Signature verification is the dispatch layer's job; this
    /// method trusts the caller. Tests bypass dispatch with
    /// [`SignedAnnouncement::placeholder`]-stamped envelopes.
    pub fn apply(
        &self,
        ann: SignedAnnouncement<K::Payload>,
    ) -> Result<ApplyOutcome, FoldError> {
        if ann.generation == 0 {
            self.metrics.on_reject();
            return Err(FoldError::InvalidGeneration {
                node_id: ann.node_id,
            });
        }

        let key = K::key_for(ann.node_id, &ann.payload);

        let mut state = self.state.write();
        let mut index = self.index.write();

        let existing = state.entries.get(&key);
        let action = K::merge(existing, &ann);

        match action {
            MergeAction::Insert => {
                // No existing entry to evict; install fresh.
                let entry = build_entry::<K>(&ann);
                index.on_insert(&key, &entry.payload);
                state
                    .by_node
                    .entry(ann.node_id)
                    .or_default()
                    .insert(key.clone());
                let audit = K::audit_event(EntryTransition::Created {
                    key: &key,
                    new: &entry,
                });
                self.emit_audit(audit);
                state.entries.insert(key, entry);
                self.metrics.on_insert();
                Ok(ApplyOutcome::Inserted)
            }
            MergeAction::Replace => {
                // Drop the old entry's index + by_node attachments
                // before installing the new one. The `replace`
                // pattern is "remove then insert" rather than
                // "in-place mutate" so the index hooks see two
                // distinct events — keeps the index trait
                // contract simple.
                //
                // `merge` only returns `Replace` when `existing`
                // was `Some`, so `state.entries.remove(&key)` is
                // guaranteed to return `Some` here. The
                // let-else fallback to `Reject` keeps the runtime
                // sound (no `unwrap`) if a future `merge` impl
                // ever violates the contract — we'd undercount
                // replaces in metrics, never silently lose data.
                let Some(old_entry) = state.entries.remove(&key) else {
                    self.metrics.on_reject();
                    return Ok(ApplyOutcome::Rejected);
                };
                if let Some(keys) = state.by_node.get_mut(&old_entry.node_id) {
                    keys.remove(&key);
                    if keys.is_empty() {
                        state.by_node.remove(&old_entry.node_id);
                    }
                }
                index.on_remove(&key, &old_entry.payload);

                let new_entry = build_entry::<K>(&ann);
                index.on_insert(&key, &new_entry.payload);
                state
                    .by_node
                    .entry(ann.node_id)
                    .or_default()
                    .insert(key.clone());
                let audit = K::audit_event(EntryTransition::Replaced {
                    key: &key,
                    old: &old_entry,
                    new: &new_entry,
                });
                self.emit_audit(audit);
                state.entries.insert(key, new_entry);
                self.metrics.on_replace();
                Ok(ApplyOutcome::Replaced)
            }
            MergeAction::Reject => {
                let audit = K::audit_event(EntryTransition::Rejected {
                    key: &key,
                    existing,
                    incoming: &ann,
                });
                self.emit_audit(audit);
                self.metrics.on_reject();
                Ok(ApplyOutcome::Rejected)
            }
        }
    }

    /// Run a query against the fold's state + secondary index.
    /// Read-only; multiple queries can execute concurrently
    /// (both locks are read-acquired).
    pub fn query(&self, q: K::Query) -> K::Result {
        self.metrics.on_query();
        let state = self.state.read();
        let index = self.index.read();
        K::query(&state, &index, q)
    }

    /// Force-remove every entry owned by `node_id`. Called when
    /// SWIM (or an operator) declares the node dead so the
    /// fold's view of liveness matches the substrate's. O(keys
    /// owned by node) via the `by_node` reverse index.
    ///
    /// `reason` is passed to [`FoldKind::audit_event`] for each
    /// removed entry so the audit chain can record operator
    /// context.
    pub fn evict_node(&self, node_id: NodeId, reason: &str) {
        let mut state = self.state.write();
        let mut index = self.index.write();

        let Some(keys) = state.by_node.remove(&node_id) else {
            return;
        };
        for key in keys {
            if let Some(old_entry) = state.entries.remove(&key) {
                index.on_remove(&key, &old_entry.payload);
                let audit = K::audit_event(EntryTransition::Evicted {
                    key: &key,
                    old: &old_entry,
                    reason,
                });
                self.emit_audit(audit);
                self.metrics.on_evict();
            }
        }
    }

    /// Take an immutable point-in-time snapshot of the current
    /// state. Cheap on the live runtime: a read-lock walk +
    /// per-entry copy into the snapshot vec. Expired entries are
    /// dropped from the dump (see [`FoldSnapshot::from_state`]).
    pub fn snapshot(&self) -> FoldSnapshot<K> {
        let state = self.state.read();
        self.metrics.on_snapshot_taken();
        FoldSnapshot::from_state(&state)
    }

    /// Restore state from a snapshot. Refuses to merge over a
    /// non-empty fold unless `force` is set — the apply path is
    /// the legitimate way to add entries to a live fold;
    /// restore is for cold-start / forced-reset only.
    ///
    /// Re-anchors `received_at` / `expires_at` against the
    /// current `Instant::now()` so freshness semantics survive
    /// the dump → restore boundary (see
    /// [`FoldSnapshot::rehydrate_entry`]).
    pub fn restore(&self, snap: FoldSnapshot<K>, force: bool) -> Result<(), FoldError> {
        // Snapshot kind ↔ fold kind is a configuration invariant:
        // the dispatch layer routes by kind, so a foreign-kind
        // snapshot here means caller mis-wired the restore.
        debug_assert!(
            snap.kind == K::KIND_ID,
            "FoldSnapshot::kind={} does not match K::KIND_ID={}",
            snap.kind,
            K::KIND_ID,
        );

        let mut state = self.state.write();
        let mut index = self.index.write();

        if !force && !state.entries.is_empty() {
            return Err(FoldError::RestoreOverLiveState {
                current_len: state.entries.len(),
            });
        }

        state.entries.clear();
        state.by_node.clear();
        index.clear();

        let anchor = Instant::now();
        for snap_entry in &snap.entries {
            let entry = FoldSnapshot::<K>::rehydrate_entry(snap_entry, anchor);
            let key = snap_entry.key.clone();
            index.on_insert(&key, &entry.payload);
            state
                .by_node
                .entry(entry.node_id)
                .or_default()
                .insert(key.clone());
            state.entries.insert(key, entry);
        }

        let new_len = state.entries.len() as u64;
        self.metrics.on_snapshot_restored(new_len);
        Ok(())
    }

    /// Read-only handle to the metric counters.
    pub fn metrics(&self) -> &FoldMetrics {
        &self.metrics
    }

    /// Materialize a [`metrics::FoldStats`] snapshot of this
    /// fold for the operator surface (CLI / Deck / Prometheus).
    /// One atomic load per counter + one read lock on the
    /// audit-sink slot; cheap enough to call per-tick.
    pub fn stats(&self) -> metrics::FoldStats {
        metrics::FoldStats {
            kind: K::KIND_ID,
            channel_prefix: K::CHANNEL_PREFIX.to_string(),
            entries: self.metrics.entries(),
            applies_inserted: self.metrics.applies_inserted(),
            applies_replaced: self.metrics.applies_replaced(),
            applies_rejected: self.metrics.applies_rejected(),
            applies_total: self.metrics.applies_total(),
            expiries: self.metrics.expiries(),
            evictions: self.metrics.evictions(),
            queries: self.metrics.queries(),
            snapshots_taken: self.metrics.snapshots_taken(),
            snapshots_restored: self.metrics.snapshots_restored(),
            has_audit_sink: self.has_audit_sink(),
        }
    }

    /// Read-only access to the live state — held under the
    /// state lock for the closure's duration. Tests and
    /// diagnostics use this; production query paths should go
    /// through [`Self::query`] so [`FoldKind::query`] can use
    /// the secondary index.
    pub fn with_state<R>(&self, f: impl FnOnce(&FoldState<K>) -> R) -> R {
        let state = self.state.read();
        f(&state)
    }

    /// Install (or uninstall) the audit sink. Idempotent;
    /// re-installing replaces the prior sink. After
    /// `set_audit_sink(Some(...))`, every
    /// [`FoldKind::audit_event`] that returns `Some` is
    /// forwarded to the sink's `record` method synchronously
    /// from the path that emitted the transition (apply,
    /// evict_node, sweep_expired). See [`FoldAuditSink`] for the
    /// contract.
    pub fn set_audit_sink(&self, sink: Option<Arc<dyn FoldAuditSink>>) {
        *self.audit_sink.write() = sink;
    }

    /// True iff an audit sink is currently installed.
    pub fn has_audit_sink(&self) -> bool {
        self.audit_sink.read().is_some()
    }

    /// Synchronous sweep: walk the primary store, evict entries
    /// past their `expires_at`, return the count removed.
    ///
    /// Drives the background sweeper too — the spawned tokio
    /// task calls into this primitive on every tick. Tests can
    /// invoke it directly to make expiry deterministic without
    /// relying on the runtime's scheduler.
    pub fn sweep_expired_now(&self) -> usize {
        let sink_holder = self.audit_sink.clone();
        let sink_guard = sink_holder.read();
        let sink_ref = sink_guard.as_ref();
        expiry::sweep_expired::<K>(&self.state, &self.index, &self.metrics, sink_ref)
    }

    /// Internal: forward an [`AuditEvent`] returned by
    /// [`FoldKind::audit_event`] to the installed sink (if any).
    /// `event = None` means the impl chose not to emit; in
    /// either case this is a single short read-lock acquisition
    /// on the sink slot.
    #[inline]
    fn emit_audit(&self, event: Option<AuditEvent>) {
        let Some(event) = event else {
            return;
        };
        if let Some(sink) = self.audit_sink.read().as_ref() {
            sink.record(event);
        }
    }
}

impl<K: FoldKind> Default for Fold<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: FoldKind> Drop for Fold<K> {
    fn drop(&mut self) {
        // Abort the background sweeper so dropped folds don't
        // keep tasks alive on the runtime — the task's `Weak`
        // upgrades would start failing on the next tick anyway,
        // but `abort` shortens the tail latency to "next
        // yield point" so test suites that churn through many
        // folds don't accumulate stale tasks waiting on
        // `tokio::time::interval::tick`.
        if let Some(handle) = self.sweep_handle.take() {
            handle.abort();
        }
    }
}

/// Build a fresh [`FoldEntry`] from an accepted announcement.
/// Computes `expires_at` from the announcement's `ttl_secs`
/// override (or [`FoldKind::DEFAULT_TTL`] when absent).
fn build_entry<K: FoldKind>(ann: &SignedAnnouncement<K::Payload>) -> FoldEntry<K> {
    let now = Instant::now();
    let ttl = ann
        .ttl_secs
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(K::DEFAULT_TTL);
    FoldEntry {
        payload: ann.payload.clone(),
        node_id: ann.node_id,
        generation: ann.generation,
        received_at: now,
        expires_at: now.checked_add(ttl).unwrap_or(now),
    }
}
