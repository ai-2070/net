//! Multi-fold framework — Phase 1.
//!
//! A generic state-aggregation runtime parameterized by the
//! [`FoldKind`] trait. One implementation handles apply, query,
//! snapshot, and node-eviction for every concrete fold; the
//! concrete folds (capability, routing, reservation per the
//! plan's Phase 3 / 4 / 5) plug in by implementing the trait.
//!
//! Phase 1 scope (this commit):
//!
//! - [`FoldKind`] trait
//! - [`Fold<K>`] runtime struct
//! - [`Fold::apply`] / [`Fold::query`] / [`Fold::snapshot`] /
//!   [`Fold::restore`] / [`Fold::evict_node`]
//! - Per-fold metric counters via [`FoldMetrics`]
//! - The wire-envelope shape ([`SignedAnnouncement`]) and the
//!   in-memory state container ([`FoldState`]) that the apply
//!   path operates on
//!
//! Deferred:
//!
//! - **Phase 1B** — background expiry sweeper, audit emission
//!   wiring (`FoldKind::audit_event` is called but the audit
//!   sink plumbing isn't connected to the project's audit chain
//!   yet).
//! - **Phase 2** — wire codec, signature verification, channel
//!   dispatch via [`FoldRegistry`] (the registry itself lands in
//!   Phase 2).
//! - **Phase 3 / 4 / 5** — concrete `CapabilityFold` /
//!   `RoutingFold` / `ReservationFold` impls, which also delete
//!   the legacy `CapabilityIndex` / `RoutingTable` modules per
//!   the stripped plan.
//!
//! See `docs/plans/SCALING_MULTIFOLD_PLAN.md` for the full design.

use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub mod announcement;
pub mod dispatch;
pub mod metrics;
pub mod snapshot;
pub mod state;
pub mod wire;

#[cfg(test)]
mod tests;

pub use announcement::SignedAnnouncement;
pub use dispatch::{DispatchError, FoldDispatch, FoldDispatchAdapter, FoldRegistry};
pub use metrics::FoldMetrics;
pub use snapshot::{FoldSnapshot, FoldSnapshotEntry};
pub use state::{
    ApplyOutcome, EntryTransition, FoldEntry, FoldError, FoldIndex, FoldState, MergeAction,
    NoIndex, NodeId,
};
pub use wire::WireError;

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

    /// Optional audit emission. Phase 1 calls this but the
    /// returned `AuditEvent` isn't yet routed into the project's
    /// audit chain — Phase 1B wires the sink. Default: emit
    /// nothing.
    fn audit_event(transition: EntryTransition<'_, Self>) -> Option<AuditEvent> {
        let _ = transition;
        None
    }
}

/// Placeholder audit-event payload for Phase 1.
///
/// `FoldKind::audit_event` returns one of these per applied
/// transition the impl wants surfaced; the runtime collects them
/// into the per-apply outcome so Phase 1B / Phase 6 can route
/// them into the project's existing signed-audit chain without
/// a Phase 1 redesign. The shape is intentionally open: a single
/// `kind` string + a `key_repr` debug-shaped string lets folds
/// emit audit info before the canonical schema is locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    /// Short kind tag: `"created"`, `"replaced"`, `"rejected"`,
    /// `"evicted"`, `"expired"`. Folds may emit additional kinds
    /// (e.g. `"reservation_takeover"`).
    pub kind: &'static str,
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
pub struct Fold<K: FoldKind> {
    state: Arc<RwLock<FoldState<K>>>,
    index: Arc<RwLock<K::Index>>,
    metrics: Arc<FoldMetrics>,
}

impl<K: FoldKind> Fold<K> {
    /// Build a new fold instance with empty state and a fresh
    /// index. Phase 2 will plumb channel subscriptions and the
    /// audit sink into this constructor; Phase 1 keeps the
    /// shape minimal.
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(FoldState::new())),
            index: Arc::new(RwLock::new(K::build_index())),
            metrics: Arc::new(FoldMetrics::new()),
        }
    }

    /// Apply a signed announcement.
    ///
    /// The flow:
    /// 1. Reject `generation == 0` (wire-format sentinel).
    /// 2. Compute the key via [`FoldKind::key_for`].
    /// 3. Acquire `state` then `index` write locks in fixed
    ///    order.
    /// 4. Consult [`FoldKind::merge`] against the existing entry.
    /// 5. On Insert / Replace: update `entries`, `by_node`, the
    ///    secondary index, and the metric counters.
    /// 6. On Reject: bump the rejected-applies counter and
    ///    return [`ApplyOutcome::Rejected`].
    /// 7. Emit any [`FoldKind::audit_event`] result (Phase 1
    ///    discards; Phase 1B will route into the audit chain).
    ///
    /// Signature verification is the dispatch layer's job
    /// (Phase 2); this method assumes the caller already
    /// validated the envelope's `signature`. Tests pass
    /// [`SignedAnnouncement::placeholder`]-stamped envelopes
    /// through directly.
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
                state.entries.insert(key, entry);
                self.metrics.on_insert();
                drop(audit); // Phase 1B routes this into the audit chain.
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
                state.entries.insert(key, new_entry);
                self.metrics.on_replace();
                drop(audit);
                Ok(ApplyOutcome::Replaced)
            }
            MergeAction::Reject => {
                let audit = K::audit_event(EntryTransition::Rejected {
                    key: &key,
                    existing,
                    incoming: &ann,
                });
                self.metrics.on_reject();
                drop(audit);
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
                drop(audit);
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
        if snap.kind != K::KIND_ID {
            // Caller fed a snapshot from a different fold kind;
            // refuse explicitly. This is a configuration bug,
            // not a runtime error — handle by returning a
            // specific variant if Phase 2 wants finer error
            // shaping. For Phase 1 we surface the wrong-kind
            // case as a debug_assert (snapshots are produced by
            // this codebase and the test suite catches mis-
            // dispatch) and proceed; in production the dispatch
            // layer should never hand a foreign-kind snapshot
            // here.
            debug_assert!(
                snap.kind == K::KIND_ID,
                "FoldSnapshot::kind={} does not match K::KIND_ID={}",
                snap.kind,
                K::KIND_ID,
            );
        }

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

    /// Read-only access to the live state — held under the
    /// state lock for the closure's duration. Tests and
    /// diagnostics use this; production query paths should go
    /// through [`Self::query`] so [`FoldKind::query`] can use
    /// the secondary index.
    pub fn with_state<R>(&self, f: impl FnOnce(&FoldState<K>) -> R) -> R {
        let state = self.state.read();
        f(&state)
    }
}

impl<K: FoldKind> Default for Fold<K> {
    fn default() -> Self {
        Self::new()
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
