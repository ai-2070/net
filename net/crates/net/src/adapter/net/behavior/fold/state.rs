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
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use smallvec::SmallVec;

use super::wheel::{Drain, ExpiryWheel};
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

/// Fast multiplicative mixer for hash keys built entirely out of
/// `u64`s that are already well-distributed — fold ids
/// ([`NodeId`], [`IslandId`](super::IslandId)) and the capability
/// index's `(u64, u64)` keys.
///
/// Those ids are derived from already-hashed identity bytes, so
/// collision resistance exists at construction and SipHash's DoS
/// resistance adds nothing — it just charges ~15-25 ns of mixing per
/// probe (PERF_AUDIT §4.6, PERF_AUDIT_2026_07_31_GANG_SCHEDULER §7).
/// What makes that worth removing is the probe *count*: the gang
/// matcher's `HostedByAny` scan probes once per topology entry, not
/// once per candidate host.
///
/// # What a key type must satisfy to use this
///
/// **Well-distributed in its LOW bits specifically** — not merely
/// collision-free. There is no finalizer: for the single-write case
/// (which is every real use here), [`Hasher::finish`](std::hash::Hasher::finish) returns
/// `v.wrapping_mul(FX_SEED)`, and multiplication only propagates
/// entropy *upward* — bit `k` of the product depends only on bits
/// `0..=k` of the input. `hashbrown` derives the bucket index from
/// the low bits of the hash, so low-bit quality of the *input* is
/// what carries the whole table.
///
/// That holds for the ids here because they are already digests
/// (`NodeId` from identity bytes, `IslandId` = `hash(host, domain)`),
/// where every bit is equidistributed. It would NOT hold for a
/// counter, a left-shifted composite, a pointer, or anything with
/// structural zeroes low down — those want a finalizer or a
/// different hasher. This is the same trade `rustc-hash` makes, and
/// the same caveat applies.
///
/// **One implementation.** [`BuildU64Hasher`] is its alias. The
/// capability index's `(u64, u64)` alias went with Slice 7, when its
/// buckets became bitmaps over entry slots
/// (CAPABILITY_FOLD_SCALE_PLAN.md). Per-site rationale belongs on the
/// alias.
#[derive(Default, Clone)]
pub struct FxU64Hasher(u64);

impl std::hash::Hasher for FxU64Hasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        // FxHash-style step: rotate, xor, multiply by a large odd
        // constant. Well-distributed for already-hashed input.
        const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(FX_SEED);
    }

    /// Defensive byte fallback — `Hash for u64` calls `write_u64`
    /// directly, but routing an unexpected key type through here
    /// must still mix rather than silently collapse.
    ///
    /// Two properties a caller arriving here should know, both
    /// harmless for the `u64`-keyed sets this serves and neither
    /// reachable from them:
    ///
    /// - An **empty slice** mixes nothing — `chunks(8)` yields no
    ///   chunks, so the state stays at its `Default` of 0.
    /// - `write(&[0u8])` produces exactly the state `write_u64(0)`
    ///   does, because the short chunk is zero-padded. So this
    ///   fallback does not domain-separate by length, and a key type
    ///   that mixes byte writes with `u64` writes could collide
    ///   across the two.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(buf));
        }
    }
}

/// [`BuildHasher`](std::hash::BuildHasher) for [`FxU64Hasher`] over
/// single-`u64` fold ids.
pub type BuildU64Hasher = std::hash::BuildHasherDefault<FxU64Hasher>;

/// A set of [`NodeId`]s hashed with [`FxU64Hasher`] — the
/// candidate-host set the gang matcher builds and then probes once per
/// topology entry.
pub type NodeIdSet = HashSet<NodeId, BuildU64Hasher>;

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
    /// Handle of this entry's node in the fold's expiry wheel, keyed on
    /// `expires_at`: the entry's one active expiry placement. Set when
    /// the entry is installed; `wheel::NIL` before that.
    pub(super) expiry_node: u32,
}

/// In-memory store backing a single [`Fold<K>`](super::Fold).
///
/// Public fields are read by [`FoldKind::query`] (and by tests),
/// but mutation flows exclusively through
/// [`super::Fold::apply`] / [`super::Fold::evict_node`] /
/// [`super::Fold::restore`] / the expiry sweep /
/// `Fold::update_payloads` so the [`super::FoldMetrics`]
/// counters, the `by_node` reverse index and the publisher revisions
/// stay coherent with `entries`. The fields are `pub` for reading
/// only: a [`super::Fold`] hands out shared borrows of its state and
/// nothing else, so no caller outside those paths can reach them
/// mutably.
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
    pub entries: HashMap<K::Key, FoldEntry<K>, K::KeyHasher>,
    /// Reverse index: `node_id → keys it owns`. Populated on
    /// every accepted apply; consulted on
    /// [`super::Fold::evict_node`] to drop every entry attached
    /// to a node in O(keys_for_that_node) instead of O(entries).
    /// At 50K-100K node scale (the plan's targeted operating
    /// range), the average node owns a handful of keys; the
    /// reverse index is the difference between "evict in
    /// microseconds" and "evict in seconds."
    ///
    /// Each record also carries the publisher's mutation revision
    /// ([`NodeRecord::rev`]). Every write to a publisher's entries
    /// goes through the `FoldState` methods that advance it
    /// (`attach_key`, `detach_key`, `remove_node`, `update_payloads`),
    /// and the record's fields are private, so no mutation path can
    /// change a publisher's entries without moving its revision.
    pub by_node: HashMap<NodeId, NodeRecord<K::Key>, K::KeyHasher>,
    /// Last publisher revision handed out. Monotonic for the life
    /// of the state, and deliberately NOT reset by a restore: a
    /// revision issued before a restore must never be reissued
    /// after it, or a cache keyed on `(node, rev)` could validate
    /// pre-restore contents against post-restore state.
    last_rev: u64,
    /// Every entry's deadline, ordered by time, so the expiry sweep
    /// visits only what is due (CAPABILITY_FOLD_SCALE_PLAN.md, Slice 8).
    /// Holds exactly one node per entry: `wheel.len() == entries.len()`
    /// whenever the state lock is released.
    wheel: ExpiryWheel<K::Key>,
}

/// Above this many keys, a [`NodeRecord`] keeps a key → position map
/// beside its key list, so membership checks and removals stay O(1)
/// for publishers that own many keys. A capability publisher owns one
/// key per class, but the routing, island and reservation folds key on
/// the payload, so one gateway can own thousands (one per destination
/// or resource). Below the threshold a linear scan of the inline list
/// is cheaper than hashing, and the record carries no map.
pub(super) const NODE_KEYS_INDEX_THRESHOLD: usize = 8;

/// Value a [`NodeRecord`]'s revision cell takes when the record is
/// dropped. Revisions come from a counter that starts at 1 and never
/// approaches this value, so a retired cell can never match a revision
/// a cache stored.
const REV_RETIRED: u64 = u64::MAX;

/// The keys one publisher owns, without duplicates.
///
/// Order is insertion order until a removal: a small list removes in
/// place, an indexed one swap-removes. Nothing reads the order as
/// meaningful (the reverse index was a `HashSet` before it was a list).
#[derive(Debug)]
struct NodeKeys<Q> {
    list: SmallVec<[Q; 1]>,
    /// `key → index into list`. Built when `list` grows past
    /// [`NODE_KEYS_INDEX_THRESHOLD`] and dropped when it shrinks to
    /// half of it; the hysteresis keeps a publisher hovering at the
    /// threshold from rebuilding the map on every insert/remove pair.
    ///
    /// Boxed on purpose: every publisher's record carries this field,
    /// almost always as `None`, and the box keeps that at one pointer
    /// instead of an inline `HashMap` (48 bytes). Only records past the
    /// threshold pay the extra allocation.
    #[allow(clippy::box_collection)]
    positions: Option<Box<HashMap<Q, usize>>>,
}

impl<Q: Hash + Eq + Clone> NodeKeys<Q> {
    fn new() -> Self {
        Self {
            list: SmallVec::new(),
            positions: None,
        }
    }

    fn contains(&self, key: &Q) -> bool {
        match &self.positions {
            Some(positions) => positions.contains_key(key),
            None => self.list.contains(key),
        }
    }

    fn insert(&mut self, key: Q) {
        if self.contains(&key) {
            return;
        }
        if let Some(positions) = self.positions.as_mut() {
            positions.insert(key.clone(), self.list.len());
        }
        self.list.push(key);
        if self.positions.is_none() && self.list.len() > NODE_KEYS_INDEX_THRESHOLD {
            let positions = self
                .list
                .iter()
                .enumerate()
                .map(|(at, k)| (k.clone(), at))
                .collect();
            self.positions = Some(Box::new(positions));
        }
    }

    /// Remove `key`, returning whether it was present.
    fn remove(&mut self, key: &Q) -> bool {
        match self.positions.as_mut() {
            Some(positions) => {
                let Some(at) = positions.remove(key) else {
                    return false;
                };
                self.list.swap_remove(at);
                if let Some(moved) = self.list.get(at) {
                    if let Some(slot) = positions.get_mut(moved) {
                        *slot = at;
                    }
                }
                if self.list.len() <= NODE_KEYS_INDEX_THRESHOLD / 2 {
                    self.positions = None;
                }
            }
            None => {
                let Some(at) = self.list.iter().position(|k| k == key) else {
                    return false;
                };
                self.list.remove(at);
            }
        }
        true
    }
}

/// One publisher's slice of [`FoldState::by_node`].
///
/// The fields are private. The record's revision lives in a shared
/// atomic cell (see [`PublisherRevision`]), so a writable field would
/// let any holder of a shared borrow move it; the keys are private so
/// that membership changes only through the [`FoldState`] methods that
/// advance the revision with them.
#[derive(Debug)]
pub struct NodeRecord<Q> {
    keys: NodeKeys<Q>,
    /// See [`Self::rev`]. Shared with [`PublisherRevision`] handles so
    /// a cache can check validity without the fold's state lock. Set to
    /// [`REV_RETIRED`] when the record is dropped, by its `Drop`, so
    /// every way a record goes away retires it: removal of its last
    /// key, `evict_node`, a restore's clear, and the whole state being
    /// dropped with its fold.
    rev: Arc<AtomicU64>,
}

impl<Q> Drop for NodeRecord<Q> {
    fn drop(&mut self) {
        self.rev.store(REV_RETIRED, Ordering::Release);
    }
}

impl<Q: Hash + Eq + Clone> NodeRecord<Q> {
    fn new(rev: u64) -> Self {
        Self {
            keys: NodeKeys::new(),
            rev: Arc::new(AtomicU64::new(rev)),
        }
    }

    /// Every key this publisher currently owns, without duplicates.
    pub fn keys(&self) -> &[Q] {
        self.keys.list.as_slice()
    }

    /// The publisher's mutation revision: receiver-local, drawn from
    /// one fold-wide counter, and advanced by every change to the set
    /// of entries the publisher owns or to any of their payloads
    /// (insert or replace of any class, removal of any class, an
    /// in-place payload update). Never `0` while the record exists; an
    /// absent publisher reads as `0` through
    /// [`FoldState::publisher_rev`]. Because the counter is fold-wide
    /// and never reused, a publisher that leaves and returns gets a
    /// revision it has never had before.
    pub fn rev(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    fn set_rev(&self, rev: u64) {
        self.rev.store(rev, Ordering::Release);
    }
}

/// A lock-free handle on one publisher record's revision, from
/// [`FoldState::publisher_revision`].
///
/// [`Self::current`] reads the revision without the fold's state lock.
/// It returns the record's revision while the record exists and `None`
/// once the record has been dropped, even if the publisher has since
/// returned: a returning publisher gets a new record and a new cell. So
/// a value cached against `(handle, rev)` is valid exactly while
/// `handle.current() == Some(rev)`.
///
/// Writers store the new revision while they hold the state write
/// lock. A reader that sees the old revision is ordered before that
/// write commits, and a reader that observed the write complete sees
/// the new one.
#[derive(Debug, Clone)]
pub struct PublisherRevision(Arc<AtomicU64>);

impl PublisherRevision {
    /// The record's current revision, or `None` once it was dropped.
    pub fn current(&self) -> Option<u64> {
        let rev = self.0.load(Ordering::Acquire);
        (rev != REV_RETIRED).then_some(rev)
    }

    /// Whether both handles name the same record. Revision NUMBERS are
    /// only unique within one fold, so a validity check that may see
    /// handles from another fold (a cache reused across folds) must
    /// compare records, not numbers.
    pub fn same_record(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<K: FoldKind> FoldState<K> {
    /// Build an empty state.
    pub fn new() -> Self {
        Self {
            entries: HashMap::default(),
            by_node: HashMap::default(),
            last_rev: 0,
            wheel: ExpiryWheel::new(Instant::now()),
        }
    }

    /// Schedule `entry` (about to be installed at `key`) in the expiry
    /// wheel at its `expires_at`, recording the node on the entry.
    pub(super) fn schedule(&mut self, key: &K::Key, entry: &mut FoldEntry<K>) {
        entry.expiry_node = self.wheel.insert(key.clone(), entry.expires_at);
    }

    /// Whether a new entry can be scheduled, checked before an Insert
    /// mutates anything. The expiry wheel addresses at most `u32::MAX`
    /// nodes, which bounds every fold kind, not only the capability fold
    /// its slot table already bounds (PR #1210 review).
    pub(super) fn can_schedule(&self) -> bool {
        self.wheel.has_room()
    }

    /// Most entries the expiry wheel can schedule at once.
    pub(super) fn schedule_limit(&self) -> usize {
        self.wheel.limit()
    }

    /// Lower the expiry wheel's node limit, so a test can reach it.
    #[cfg(test)]
    pub(crate) fn set_schedule_limit(&mut self, limit: usize) {
        self.wheel.set_limit(limit);
    }

    /// Move an installed entry's expiry placement to `deadline`. Never
    /// allocates, so a warm refresh stays allocation-free.
    pub(super) fn reschedule(&mut self, node: u32, deadline: Instant) {
        self.wheel.reschedule(node, deadline);
    }

    /// Drop an entry's expiry placement as the entry leaves `entries`.
    pub(super) fn unschedule(&mut self, node: u32) {
        self.wheel.remove(node);
    }

    /// Whether any entry is due at `now`, and how many wheel nodes the
    /// check visited. Read-only, so the sweep can decide under the read
    /// lock that there is nothing to do.
    pub(super) fn probe_due(&self, now: Instant) -> (bool, usize) {
        self.wheel.probe_due(now)
    }

    /// Unschedule up to `max` keys due at `now` onto `out`. The caller
    /// removes the matching entries before releasing the write lock.
    pub(super) fn take_due(&mut self, now: Instant, max: usize, out: &mut Vec<K::Key>) -> Drain {
        self.wheel.take_due(now, max, out)
    }

    /// Scheduled keys: equal to [`Self::len`] whenever the state lock is
    /// released.
    pub(super) fn scheduled_len(&self) -> usize {
        self.wheel.len()
    }

    /// Full check of the expiry index against `entries`: one node per
    /// entry, each recording that entry's key and deadline, with
    /// consistent lists. O(entries); for tests.
    #[cfg(test)]
    pub(crate) fn assert_expiry_index(&self) {
        self.wheel.assert_consistent();
        assert_eq!(self.wheel.len(), self.entries.len(), "one node per entry");
        let mut nodes: HashMap<u32, (&K::Key, Instant)> = HashMap::new();
        for (at, key, deadline) in self.wheel.scheduled() {
            nodes.insert(at, (key, deadline));
        }
        for (key, entry) in &self.entries {
            let Some(&(node_key, deadline)) = nodes.get(&entry.expiry_node) else {
                panic!("entry {key:?} has no expiry node");
            };
            assert_eq!(node_key, key, "node key");
            assert_eq!(deadline, entry.expires_at, "node deadline for {key:?}");
        }
    }

    /// The ring position an entry's node is linked into, and the one its
    /// deadline maps to now. For placement witnesses.
    #[cfg(test)]
    pub(crate) fn expiry_position(&self, key: &K::Key) -> Option<(u32, u32)> {
        let entry = self.entries.get(key)?;
        let linked = self.wheel.position_of_node(entry.expiry_node)?;
        Some((linked, self.wheel.position_for(entry.expires_at)))
    }

    /// The keys `node` currently owns, if it owns any.
    pub fn keys_for(&self, node: NodeId) -> Option<&[K::Key]> {
        self.by_node.get(&node).map(NodeRecord::keys)
    }

    /// `node`'s current mutation revision, or `0` when it owns no
    /// entries. Two reads that return the same NONZERO value bracket
    /// no change to the publisher's entries: see [`NodeRecord::rev`].
    /// `0` is not a history fence: two reads of `0` can bracket the
    /// publisher arriving and leaving again. A value derived only from
    /// absence (such as the empty capability set) is still correct at
    /// the second read.
    pub fn publisher_rev(&self, node: NodeId) -> u64 {
        self.by_node.get(&node).map_or(0, NodeRecord::rev)
    }

    /// A lock-free handle on `node`'s revision, or `None` when it owns
    /// no entries. See [`PublisherRevision`].
    pub fn publisher_revision(&self, node: NodeId) -> Option<PublisherRevision> {
        self.by_node
            .get(&node)
            .map(|record| PublisherRevision(record.rev.clone()))
    }

    fn next_rev(&mut self) -> u64 {
        self.last_rev += 1;
        self.last_rev
    }

    /// Record that `node` owns `key` after an accepted write of that
    /// key's entry, and advance `node`'s revision. Called for every
    /// insert and replace, including a replace that leaves the key
    /// set unchanged: the payload changed, so the revision must. A key
    /// already listed is not listed twice, so this also repairs a
    /// record that had lost the key.
    pub(super) fn attach_key(&mut self, node: NodeId, key: K::Key) {
        let rev = self.next_rev();
        let record = self
            .by_node
            .entry(node)
            .or_insert_with(|| NodeRecord::new(rev));
        record.keys.insert(key);
        record.set_rev(rev);
    }

    /// Advance `node`'s revision without touching its keys. A no-op
    /// on the records when `node` owns nothing.
    fn touch_node(&mut self, node: NodeId) {
        let rev = self.next_rev();
        if let Some(record) = self.by_node.get(&node) {
            record.set_rev(rev);
        }
    }

    /// Record that `node` no longer owns `key`. Drops the record when
    /// it was the publisher's last key (the publisher then reads as
    /// absent, revision `0`); otherwise advances the revision, since
    /// one of its classes is gone. A no-op when `node` does not list
    /// `key`.
    pub(super) fn detach_key(&mut self, node: NodeId, key: &K::Key) {
        let Some(record) = self.by_node.get_mut(&node) else {
            return;
        };
        if !record.keys.remove(key) {
            return;
        }
        if record.keys.list.is_empty() {
            // Dropping the record retires its revision cell.
            self.by_node.remove(&node);
            return;
        }
        self.touch_node(node);
    }

    /// Drop `node`'s record, returning the keys it owned.
    pub(super) fn remove_node(&mut self, node: NodeId) -> Option<SmallVec<[K::Key; 1]>> {
        // The record retires its revision cell as it drops here.
        let mut record = self.by_node.remove(&node)?;
        Some(std::mem::take(&mut record.keys.list))
    }

    /// Empty the entries and the reverse index ahead of a restore,
    /// keeping the revision counter so restored publishers get
    /// revisions no earlier lookup has seen.
    pub(super) fn clear_for_restore(&mut self) {
        // Dropping the records retires their revision cells.
        self.wheel.clear(Instant::now());
        self.entries.clear();
        self.by_node.clear();
    }

    /// Rewrite the payloads of `node`'s entries in place: `update`
    /// runs on each and returns whether it changed that payload. When
    /// any changed, `node`'s revision advances, so a cache keyed on it
    /// misses. Returns how many payloads changed.
    ///
    /// The secondary index is NOT maintained, so `update` may only
    /// touch fields no index reads; see [`super::Fold::update_payloads`].
    pub(super) fn update_payloads(
        &mut self,
        node: NodeId,
        mut update: impl FnMut(&mut K::Payload) -> bool,
    ) -> usize {
        let Some(record) = self.by_node.get(&node) else {
            return 0;
        };
        let mut changed = 0;
        for key in record.keys() {
            if let Some(entry) = self.entries.get_mut(key) {
                if update(&mut entry.payload) {
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.touch_node(node);
        }
        changed
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

    /// Returns `true` when the two payloads index identically —
    /// i.e. [`Self::on_remove`] + [`Self::on_insert`] against
    /// these two payloads would net to a no-op on every
    /// dimension this index maintains.
    ///
    /// The runtime's `MergeAction::Replace` arm consults this
    /// before paying the index churn: when an announcement
    /// refreshes generation/TTL without changing the tags /
    /// region / state the index keys on (the steady-state
    /// republish case), the index dance is pure waste. Per
    /// PERF_AUDIT §4.5 — pre-fix the refresh always re-walked
    /// every tag bucket, re-derived synthetic indexes, and
    /// re-allocated the `entry().or_default()` HashSets even
    /// when nothing changed, all under the writer lock.
    ///
    /// Default `false` keeps the safe pre-fix behavior for
    /// indexes that don't implement a content-aware equality
    /// check.
    fn index_payload_equivalent(_old: &K::Payload, _new: &K::Payload) -> bool {
        false
    }

    /// Admit an accepted mutation's payload into any receiver-local
    /// storage the index shares with entries (the capability fold's tag
    /// dictionary). Called after merge decided Insert (`outgoing` is
    /// `None`) or Replace (`outgoing` is the payload being replaced),
    /// under the state + index write guards, before any other change.
    ///
    /// All-or-nothing: on `Err` nothing in the index and nothing in
    /// `incoming` may have changed, and the fold refuses the apply. On
    /// `Ok` the index may have rewritten `incoming` to share storage, and
    /// has released `outgoing`'s share. Default: admit everything.
    fn admit(
        &mut self,
        _incoming: &mut K::Payload,
        _outgoing: Option<&K::Payload>,
    ) -> Result<(), PayloadRejection> {
        Ok(())
    }

    /// Release a stored payload's share of admission storage as its
    /// entry leaves the fold by eviction, expiry or a restore unwind.
    /// (A Replace releases through [`Self::admit`].) Default: nothing.
    fn release(&mut self, _payload: &K::Payload) {}

    /// Whether a restore installing exactly `payloads` (the effective
    /// restored state) into an emptied index would be admitted. Called
    /// before the live fold is touched. Default: yes.
    fn preflight_restore(&self, _payloads: &[&K::Payload]) -> Result<(), PayloadRejection> {
        Ok(())
    }

    /// Most entries the index can hold at once. A restore stops building
    /// its rows past this, before cloning a snapshot it could never
    /// install. Default: unbounded.
    fn entry_capacity(&self) -> usize {
        usize::MAX
    }

    /// Admission storage counters. Default: zeros.
    fn admission_stats(&self) -> AdmissionStats {
        AdmissionStats::default()
    }
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
    /// The announcement's payload was refused whole: it broke a payload
    /// limit ([`FoldKind::validate`]) or the fold's admission budget
    /// ([`FoldIndex::admit`]). Nothing in the fold changed.
    #[error("payload from publisher {node_id} refused: {reason}")]
    PayloadRejected {
        /// Publisher of the refused announcement.
        node_id: NodeId,
        /// Why.
        reason: PayloadRejection,
    },
    /// A restore was refused before touching the live fold: a row of the
    /// effective restored state broke a payload limit, or the state as a
    /// whole does not fit the admission budget. The fold is unchanged.
    #[error("restore refused, fold unchanged: {reason}")]
    RestoreRefused {
        /// Why.
        reason: PayloadRejection,
    },
}

/// Why a payload was refused whole. Typed so a caller (and the fold's
/// counters) can tell a malformed advertisement from a full budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadRejection {
    /// More tags than the per-advertisement cap, duplicates counted.
    #[error("{count} tags, over the cap of {max}")]
    TooManyTags {
        /// Tags carried.
        count: usize,
        /// The cap.
        max: usize,
    },
    /// A tag longer than the per-tag cap, in UTF-8 bytes.
    #[error("tag {index} is {len} bytes, over the cap of {max}")]
    TagTooLong {
        /// Position of the first overlong tag.
        index: usize,
        /// Its length in UTF-8 bytes.
        len: usize,
        /// The cap.
        max: usize,
    },
    /// The fold has no slot left for a new entry: every one of `slots`
    /// positions is occupied, in the secondary index's slot table or the
    /// expiry wheel. Never wraps.
    #[error("index full: all {slots} entry slots are occupied")]
    IndexFull {
        /// The slot-space size.
        slots: usize,
    },
    /// Admitting the payload's new tags would exceed the fold's
    /// canonical-tag budget, after counting what a replacement frees.
    #[error(
        "tag budget: {new_tags} new tags / {new_bytes} bytes would exceed \
         {max_tags} tags / {max_bytes} bytes (live {live_tags} / {live_bytes})"
    )]
    TagBudget {
        /// New distinct tags the payload brings.
        new_tags: usize,
        /// Their UTF-8 bytes.
        new_bytes: usize,
        /// Distinct tags held when refused.
        live_tags: usize,
        /// Bytes held when refused.
        live_bytes: usize,
        /// The budget's tag ceiling.
        max_tags: usize,
        /// The budget's byte ceiling.
        max_bytes: usize,
    },
}

impl PayloadRejection {
    /// Whether this is a capacity refusal (the tag budget or the index's
    /// slot space) rather than a size-limit one.
    pub fn is_budget(&self) -> bool {
        matches!(self, Self::TagBudget { .. } | Self::IndexFull { .. })
    }
}

/// Counters an index's admission dictionary reports through
/// [`FoldIndex::admission_stats`]. All zero for an index without one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionStats {
    /// Distinct canonical values held (tags, for the capability fold).
    pub interned: u64,
    /// Bytes of those values: what the budget counts.
    pub interned_bytes: u64,
    /// Estimated overhead beyond those bytes, not counted by the budget.
    pub overhead_bytes: u64,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{NodeKeys, NODE_KEYS_INDEX_THRESHOLD};

    /// Every invariant `NodeKeys` keeps: no duplicates, the list and the
    /// model hold the same keys, and the position map (when present)
    /// names each key's actual index.
    fn check(keys: &NodeKeys<u64>, model: &HashSet<u64>) {
        let listed: HashSet<u64> = keys.list.iter().copied().collect();
        assert_eq!(listed.len(), keys.list.len(), "no duplicates");
        assert_eq!(&listed, model);
        if let Some(positions) = &keys.positions {
            assert_eq!(positions.len(), keys.list.len());
            for (at, key) in keys.list.iter().enumerate() {
                assert_eq!(positions.get(key), Some(&at), "position of {key}");
            }
        }
        for key in model {
            assert!(keys.contains(key));
        }
    }

    /// Random inserts and removals over a key space wide enough to cross
    /// the index threshold in both directions, checked against a
    /// `HashSet` model after every operation.
    #[test]
    fn node_keys_match_a_set_model_across_the_index_threshold() {
        let mut keys = NodeKeys::new();
        let mut model = HashSet::new();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut crossed_up = false;
        let mut crossed_down = false;
        for step in 0..20_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let key = seed % 40;
            // Bias toward inserts for the first half, removals after, so
            // the list grows past the threshold and drains below it.
            let insert = !(seed >> 32).is_multiple_of(4);
            let insert = if step < 10_000 { insert } else { !insert };
            let had_index = keys.positions.is_some();
            if insert {
                keys.insert(key);
                model.insert(key);
            } else {
                assert_eq!(keys.remove(&key), model.remove(&key));
            }
            crossed_up |= !had_index && keys.positions.is_some();
            crossed_down |= had_index && keys.positions.is_none();
            check(&keys, &model);
        }
        assert!(crossed_up && crossed_down, "both transitions exercised");
    }

    #[test]
    fn small_node_keys_carry_no_index() {
        let mut keys = NodeKeys::new();
        for key in 0..NODE_KEYS_INDEX_THRESHOLD as u64 {
            keys.insert(key);
        }
        assert!(keys.positions.is_none());
        keys.insert(NODE_KEYS_INDEX_THRESHOLD as u64);
        assert!(keys.positions.is_some(), "indexed past the threshold");
    }
}
