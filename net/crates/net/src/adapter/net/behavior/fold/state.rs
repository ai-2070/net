//! Generic in-memory state for a [`Fold<K>`](super::Fold).
//!
//! The fold runtime is parameterized by a single `FoldKind` trait
//! implementor (capability / routing / reservation / ...); this
//! module hosts the runtime-shared data structures: the per-key
//! entry record, the key→entry primary store, the node_id→keys
//! reverse index used by [`super::Fold::evict_node`], the merge
//! action enum that `FoldKind::merge` returns, the transition
//! enum that drives audit emission, and the [`FoldIndex`] trait
//! domain-specific secondary indices implement.
//!
//! Nothing in this module knows anything about wire format,
//! signature verification, channels, or audit chains — those
//! belong to the dispatch layer and the runtime layer
//! ([`super`]).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::wire::SignedAnnouncement;
use super::FoldKind;

/// Publisher's routing-layer identity, matching
/// [`behavior::placement::NodeId`](super::super::placement::NodeId).
/// The fold layer indexes by this `u64` rather than the 32-byte
/// cryptographic node identity because every query surface
/// (capability, routing, reservation) addresses nodes by their
/// routing id, and the wire envelope already commits a separate
/// [`SignedAnnouncement::signature`] to the publisher's
/// cryptographic identity.
pub type NodeId = u64;

/// One entry in a fold: the payload most recently accepted for
/// its key, plus the bookkeeping the runtime needs to expire,
/// merge, and audit further announcements.
///
/// `K::Payload` is owned, not borrowed — folds are eventually
/// consistent state caches, not view layers over a foreign
/// authority.
#[derive(Debug, Clone)]
pub struct FoldEntry<K: FoldKind> {
    /// Domain-specific payload accepted at this key.
    pub payload: K::Payload,
    /// Publisher of the announcement that produced this entry.
    /// Used to populate `state.by_node` for
    /// [`super::Fold::evict_node`] and to gate owner-only
    /// transitions in folds that enforce per-publisher state
    /// machines (e.g. [`super::ReservationFold`]).
    pub node_id: NodeId,
    /// Monotonic counter per `(node_id, kind, class)`, copied
    /// from the announcement. The default [`FoldKind::merge`]
    /// rejects any incoming announcement whose generation is
    /// `<=` the stored generation — this is the wire-level
    /// anti-reorder mechanism.
    pub generation: u64,
    /// Wall-clock instant at which the runtime accepted the
    /// announcement that produced this entry. Used by metrics +
    /// snapshot diagnostics; NOT used for expiry (see
    /// `expires_at`).
    pub received_at: Instant,
    /// Wall-clock instant at which this entry becomes stale.
    /// Computed at apply time as
    /// `received_at + ann.ttl_secs.unwrap_or(K::DEFAULT_TTL)`.
    /// The background expiry sweeper removes entries past this
    /// time.
    pub expires_at: Instant,
}

/// In-memory store backing a single [`Fold<K>`](super::Fold).
///
/// Public fields are read by [`FoldKind::query`] (and by tests),
/// but mutation flows exclusively through
/// [`super::Fold::apply`] / [`super::Fold::evict_node`] /
/// [`super::Fold::restore`] so the [`super::FoldMetrics`] counters
/// and `by_node` reverse index stay coherent with `entries`.
///
/// The container is held inside an `RwLock` on the
/// [`Fold<K>`](super::Fold) struct; this type is purely the data
/// shape, not the synchronization primitive.
#[derive(Debug)]
pub struct FoldState<K: FoldKind> {
    /// Primary store: `K::Key → FoldEntry<K>`. The
    /// [`FoldKind::key_for`] function is the only sanctioned
    /// way to derive keys from announcements; the apply path
    /// uses it to look up + replace existing entries.
    pub entries: HashMap<K::Key, FoldEntry<K>>,
    /// Reverse index: `node_id → keys it owns`. Populated on
    /// every accepted apply; consulted on
    /// [`super::Fold::evict_node`] to drop every entry attached
    /// to a node in O(keys_for_that_node) instead of O(entries).
    /// At 50K-100K node scale (the plan's targeted operating
    /// range), the average node owns a handful of keys; the
    /// reverse index is the difference between "evict in
    /// microseconds" and "evict in seconds."
    pub by_node: HashMap<NodeId, HashSet<K::Key>>,
}

impl<K: FoldKind> FoldState<K> {
    /// Build an empty state.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            by_node: HashMap::new(),
        }
    }

    /// Total entry count. Cheap O(1) read off the primary store.
    /// Mirrors what the [`super::FoldMetrics::entries`] gauge
    /// reports; tests and the [`super::Fold::snapshot`] header
    /// read it without acquiring the metrics layer.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the state is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up the entry for `key`. Borrowed access; the caller
    /// already holds the state guard via
    /// [`FoldKind::query`]'s `state: &FoldState<Self>` parameter.
    pub fn get(&self, key: &K::Key) -> Option<&FoldEntry<K>> {
        self.entries.get(key)
    }
}

impl<K: FoldKind> Default for FoldState<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// Verdict from [`FoldKind::merge`] for a new announcement
/// against the current state at its key. The runtime translates
/// the verdict into a concrete state mutation in
/// [`super::Fold::apply`].
///
/// Carries the announcement payload by reference on the runtime
/// side (the apply path passes `&SignedAnnouncement` into
/// `merge`); this enum is the *decision* shape, so it doesn't
/// embed the payload again — the runtime already has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeAction {
    /// No existing entry at this key. Runtime inserts the
    /// announcement's payload as a fresh [`FoldEntry`].
    Insert,
    /// Existing entry is older / out-ranked. Runtime evicts the
    /// old entry (updating `by_node` for both old and new
    /// owners) and inserts the new payload.
    Replace,
    /// Existing entry wins. Runtime drops the announcement and
    /// bumps the rejected-applies metric.
    Reject,
}

/// Transition shape passed to [`FoldKind::audit_event`] when an
/// applied announcement produces an audit-worthy state change.
/// Per the plan's audit-integration section, the defaults emit
/// `FoldEntryCreated` / `FoldEntryReplaced` / `FoldEntryExpired`
/// / `FoldEntryEvicted` / `FoldEntryRejected`; fold authors
/// match on the variant they care about.
#[derive(Debug)]
pub enum EntryTransition<'a, K: FoldKind> {
    /// First-time insert at this key. `new` is the freshly-
    /// applied entry.
    Created {
        /// Key that received the new entry.
        key: &'a K::Key,
        /// The freshly-applied entry.
        new: &'a FoldEntry<K>,
    },
    /// Replacement at this key. Both `old` (about to be dropped)
    /// and `new` (about to be installed) are visible so audit
    /// records can carry generation deltas.
    Replaced {
        /// Key whose entry was replaced.
        key: &'a K::Key,
        /// Entry that was just evicted (the loser of the merge).
        old: &'a FoldEntry<K>,
        /// Entry that replaced it.
        new: &'a FoldEntry<K>,
    },
    /// Announcement was rejected per [`MergeAction::Reject`].
    /// `existing` is the entry that wins; `incoming` is the
    /// raw announcement that lost.
    Rejected {
        /// Key the rejected announcement targeted.
        key: &'a K::Key,
        /// Current entry at the key, if any — the merge winner.
        existing: Option<&'a FoldEntry<K>>,
        /// The losing announcement.
        incoming: &'a SignedAnnouncement<K::Payload>,
    },
    /// Entry was force-removed via [`super::Fold::evict_node`].
    /// `reason` is the operator-visible string for the audit
    /// record (e.g. "SWIM declared node dead").
    Evicted {
        /// Key whose entry was evicted.
        key: &'a K::Key,
        /// Entry that was removed.
        old: &'a FoldEntry<K>,
        /// Operator-supplied reason string for the audit log.
        reason: &'a str,
    },
    /// Entry was removed by the TTL sweeper because
    /// `expires_at < now`.
    Expired {
        /// Key whose entry expired.
        key: &'a K::Key,
        /// Entry that was removed.
        old: &'a FoldEntry<K>,
    },
}

/// Secondary index maintained alongside the primary
/// `key → entry` store. Domain-specific: capability uses a
/// tag-inverted lookup, reservation uses a "currently free" set,
/// routing uses no extra index (uses the primary store
/// directly).
///
/// The runtime calls `on_insert` / `on_remove` on every accepted
/// apply, before / after the primary-store mutation respectively
/// so the index sees the same `(key, payload)` shape the entry
/// is built from. [`FoldKind::query`] reads the index by
/// reference; it does NOT mutate.
pub trait FoldIndex<K: FoldKind>: Send + Sync {
    /// Called after an [`MergeAction::Insert`] or
    /// [`MergeAction::Replace`] commits to the primary store.
    /// For `Replace`, the previous payload was already passed
    /// to [`Self::on_remove`].
    fn on_insert(&mut self, key: &K::Key, payload: &K::Payload);

    /// Called before an [`MergeAction::Replace`] or an
    /// [`super::Fold::evict_node`] eviction drops the entry
    /// from the primary store, with the payload that's about
    /// to be removed.
    fn on_remove(&mut self, key: &K::Key, payload: &K::Payload);

    /// Drop every cached relation. Called by
    /// [`super::Fold::restore`] before re-populating from a
    /// snapshot.
    fn clear(&mut self);
}

/// Default no-op secondary index. Folds that don't need a
/// secondary lookup use this as their `K::Index` so the runtime
/// still has a uniformly-typed hook to call.
#[derive(Debug, Default)]
pub struct NoIndex;

impl<K: FoldKind> FoldIndex<K> for NoIndex {
    fn on_insert(&mut self, _key: &K::Key, _payload: &K::Payload) {}
    fn on_remove(&mut self, _key: &K::Key, _payload: &K::Payload) {}
    fn clear(&mut self) {}
}

/// Outcome of a single [`super::Fold::apply`] call. Mirrors
/// [`MergeAction`] but carries the entry that produced the
/// audit event (if any) so the runtime can hand it to
/// [`FoldKind::audit_event`] without re-locking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// New entry was created at the key.
    Inserted,
    /// Existing entry was replaced.
    Replaced,
    /// Existing entry wins; announcement dropped.
    Rejected,
}

/// Errors the runtime returns from the apply / snapshot path.
/// Dispatch-layer errors (bad signature, unknown kind) flow
/// through [`super::WireError`] / [`super::DispatchError`]
/// instead.
#[derive(Debug, thiserror::Error)]
pub enum FoldError {
    /// Apply rejected because the announcement's generation is
    /// `0`, which the wire format reserves as the "uninitialized"
    /// sentinel. A legitimate publisher always starts at `1`.
    #[error("invalid generation 0 from publisher {node_id}")]
    InvalidGeneration {
        /// Publisher whose announcement carried generation 0.
        node_id: NodeId,
    },
    /// Restore was called on a non-empty fold without the
    /// `force` flag. The runtime refuses to merge a snapshot
    /// over a live state — operators who really want this pass
    /// `force: true` to [`super::Fold::restore`].
    #[error("restore refused: fold is non-empty (len={current_len})")]
    RestoreOverLiveState {
        /// Current entry count of the live fold.
        current_len: usize,
    },
}
