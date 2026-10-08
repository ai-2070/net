//! `CapabilityFold` — per-publisher capability membership.
//!
//! Each `(class_hash, publisher_node_id)` pair carries at most
//! one entry whose payload describes what the publisher claims
//! about its own membership in that capability class — tags,
//! hardware summary, current state, optional region + price
//! quote.
//!
//! Replaces the deleted `behavior::capability::CapabilityIndex` —
//! see `docs/internal/plans/MULTIFOLD_PHASE_3B_CUTOVER.md` for the
//! end-to-end cutover that landed.
//!
//! Tags ship as canonical `String`s — the same form the legacy
//! [`Tag`](super::super::tag::Tag) enum would emit when
//! displayed — to keep the wire envelope parseable by operator
//! tools regardless of the in-memory shape downstream.
//!
//! Key shape: `(class_hash, publisher_node_id)`. The publisher's
//! `node_id` IS the key component, so each publisher writes only
//! its own entries. Unlike [`RoutingFold`](super::routing) (where
//! multiple publishers compete for a shared destination key),
//! the security model here is trivial: signature verification at
//! dispatch time gates the publisher claim; the key shape gates
//! which entries that publisher may write.

#[cfg(test)]
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use roaring::RoaringBitmap;

use super::state::{AdmissionStats, FoldIndex, FoldState, NodeId, PayloadRejection};
use super::tag_dictionary::{validate_capability_tags, TagBudget, TagDictionary};
use super::tag_str::TagStr;
use super::FoldKind;

/// Coarse-grained node state for capability matching. The
/// scheduler / market matcher filters on this when picking
/// candidates: an `Idle` node is a candidate, a `Faulty` node
/// is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Node is idle and accepting work.
    Idle,
    /// Node is running work but might still accept more.
    Busy,
    /// Node has been reserved by a scheduler; not currently
    /// accepting placement decisions from other schedulers.
    Reserved,
    /// Node is known unhealthy. Don't place on it.
    Faulty,
}

/// Lightweight hardware-summary the scheduler reads when
/// filtering candidates by hardware shape. NOT a complete
/// hardware inventory — the legacy
/// [`HardwareCapabilities`](super::super::capability::HardwareCapabilities)
/// struct stays the source of truth; this is the small
/// always-shipped projection that callers want to filter on
/// without paying for the full announcement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct HardwareSummary {
    /// GPU vendor string (canonical lowercase: `"nvidia"`,
    /// `"amd"`, `"intel"`). `None` if the node has no GPU.
    pub gpu_vendor: Option<String>,
    /// GPU count.
    pub gpu_count: u8,
    /// System memory in gigabytes. `None` if unknown.
    pub memory_gb: Option<u32>,
    /// Total GPU video memory in gigabytes (sum across all
    /// installed GPUs). `None` if the node has no GPU or the
    /// publisher didn't fill it.
    pub vram_gb: Option<u32>,
}

/// Wire payload for one capability announcement. The publisher
/// declares its own membership in `class_hash`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityMembership {
    /// Capability class this announcement is about. Each
    /// announcement covers one (class, publisher) pair; a
    /// publisher in multiple classes emits one announcement
    /// per class.
    pub class_hash: u64,
    /// Canonical-form tag strings the publisher claims
    /// (e.g. `"hardware.gpu"`, `"hardware.gpu.vram_gb=80"`,
    /// `"causal:<hex>"`). See the module doc on tag
    /// representation.
    ///
    /// [`TagStr`] handles, which serialize exactly as `String`s (the wire
    /// form and signature transcript are unchanged). Once stored in a
    /// fold, each one shares its text with every other entry carrying
    /// the same tag. At most [`super::MAX_CAPABILITY_TAGS`] tags of at
    /// most [`super::MAX_CAPABILITY_TAG_LEN`] UTF-8 bytes each: a fold
    /// refuses a larger advertisement whole.
    pub tags: Vec<TagStr>,
    /// Optional hardware projection for fast filtering.
    pub hardware: Option<HardwareSummary>,
    /// Current state — the load-bearing filter for the
    /// scheduler's "find idle candidates" path.
    pub state: NodeState,
    /// Optional region string. Free-form; operator chooses
    /// the granularity (`"us-east"`, `"us-east.dc-1"`, etc.).
    pub region: Option<String>,
    /// Optional price-per-unit quote for compute-marketplace
    /// workloads. Units intentionally opaque (operator
    /// decides — could be µ$/sec, µ$/job, µ$/GPU-hour).
    pub price_quote: Option<u64>,
    /// Publisher's last-advertised public reflex `SocketAddr`.
    /// Used by NAT-traversal rendezvous (stage 3) to look up
    /// the punch target's public address. The publisher emits
    /// this whenever it observes its own public side via a
    /// reflex probe; receivers cache it across class entries
    /// (one publisher tends to publish the same reflex across
    /// every class it joins).
    pub reflex_addr: Option<std::net::SocketAddr>,
    /// Publisher's announced Noise static public key (plan §5
    /// Layer 1, Stage 4a). Projected exactly like `reflex_addr`:
    /// the publisher emits it only when it has RTC configured, and
    /// receivers read it to build a session without an
    /// out-of-band key handoff. `None` for every publisher that
    /// does not announce one, which is every pre-Stage-4 node.
    /// **Not serialized.** The fold envelope is a non-self-describing
    /// binary encoding, so an omitted-when-`None` field is a decode
    /// error on the far side rather than a default — and this
    /// projection does not need to travel: every node ingests the
    /// announcement itself and fills this locally.
    #[serde(skip)]
    pub noise_pubkey: Option<[u8; 32]>,
    /// Publisher's announced bootstrap listener URL
    /// (`rtc_bootstrap`, plan §11; Stage 4b). Projected exactly like
    /// `noise_pubkey`, and `#[serde(skip)]` for the same reason:
    /// every node ingests the announcement itself, so this
    /// projection never travels in the fold envelope.
    #[serde(skip)]
    pub rtc_bootstrap: Option<String>,
    /// Publisher's announced public RTC/STUN socket (`rtc_addr`,
    /// plan §11; Stage 4b). Same projection rules as
    /// `rtc_bootstrap`.
    #[serde(skip)]
    pub rtc_addr: Option<std::net::SocketAddr>,
    /// Publisher's announced STUN endpoint (`rtc_stun_addr`,
    /// Stage 6) — the endpoint a leaf's `iceServers` points at,
    /// deliberately distinct from `rtc_addr`, which cannot serve
    /// STUN for a connection it is itself the ICE peer of. Same
    /// projection rules as `rtc_bootstrap`.
    #[serde(skip)]
    pub rtc_stun_addr: Option<String>,
    /// v0.4 capability-auth allow-list — peer `node_id`s
    /// authorized to invoke any of this publisher's `tags`. Empty
    /// = unrestricted (permissive default). Union semantics with
    /// `allowed_subnets` and `allowed_groups`; the caller is
    /// admitted if it matches at least one populated axis.
    pub allowed_nodes: Vec<u64>,
    /// v0.4 capability-auth allow-list — caller subnets authorized
    /// to invoke this publisher's tags. Same union semantics as
    /// `allowed_nodes`.
    pub allowed_subnets: Vec<super::super::subnet::SubnetId>,
    /// v0.4 capability-auth allow-list — caller groups authorized
    /// to invoke this publisher's tags. Same union semantics as
    /// `allowed_nodes`.
    pub allowed_groups: Vec<super::super::group::GroupId>,
    /// Free-form per-publisher metadata. Carries the same opaque
    /// key/value pairs the legacy
    /// [`CapabilitySet::metadata`](super::super::capability::CapabilitySet)
    /// exposes; predicates that test `metadata_exists`/
    /// `metadata_equals` consult this map after `synthesize_capability_set`
    /// hydrates the synthesized set from the fold.
    pub metadata: BTreeMap<String, String>,
    /// OA-1 ownership projection — the publisher's verified owner,
    /// populated ONLY when BOTH the enclosing announcement
    /// signature AND the embedded `owner_cert` passed ingest
    /// verification (announcement signature, cert signature,
    /// window, `member == entity_id` binding, revocation floors).
    /// `None` for unowned publishers, for unsigned announcements
    /// (a valid replayed cert must not lend ownership to an
    /// unauthenticated capability statement — review-8 §1), and
    /// for announcements whose cert failed verification (the cert
    /// is dropped; the entry is kept).
    ///
    /// Unlike `allowed_*` / tag-derived axes this is not
    /// self-declared — it is proven belonging. It is also NOT
    /// execution authority: `may_execute` never consults it
    /// (`ORG_CAPABILITY_AUTH_PLAN.md`, authority-dark OA-1).
    ///
    /// `#[serde(skip)]` is load-bearing twice over: (1) the fold
    /// payload rides `SUBPROTOCOL_FOLD` as positional postcard, so
    /// a serialized field would break every mixed-fleet fold frame
    /// at upgrade time; (2) a wire-carried owner would be a
    /// SELF-DECLARED ownership claim — the projection must only
    /// ever be derived on the receiving node from a cert it
    /// verified itself, and fold state (including snapshots) is
    /// never admission evidence. Decode always yields `None`.
    #[serde(skip)]
    pub owner: Option<VerifiedOwner>,
}

/// An ingest-verified ownership projection: WHICH ENTITY published,
/// which org vouched for it, and at which certificate generation.
/// The generation is retained so a rising revocation floor can
/// retract exactly the projections that fell below it — no
/// re-announcement required, no still-valid (higher-generation)
/// projection over-cleared (review-8 §9).
///
/// The `member` is retained (review-10 P1-1) because a `NodeId` is
/// the low 8 bytes of an entity id and therefore NOT an identity: a
/// consumer that reads a projection under one snapshot and then
/// resolves `NodeId → EntityId` through the live session pin can
/// pair one publisher's verified owner with a DIFFERENT entity that
/// currently holds the pin. Carrying the verified publisher inside
/// the projection lets such a consumer compare against the exact
/// entity whose cert was checked, instead of trusting the node id to
/// name it.
///
/// Construction is `pub(crate)` and the fields are private
/// (review-9): the verification bridge
/// (`capability_bridge::verify_announced_owner_cert`) is the only
/// legitimate producer, so a caller outside this crate cannot
/// synthesize an unverified "verified" projection and feed it
/// through `translate_announcement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedOwner {
    /// The publisher whose owner cert verified at ingest, as raw
    /// bytes rather than an `EntityId` so this projection stays
    /// `Copy` — the ingest path passes it by value four times per
    /// announcement and `EntityId` is deliberately `Clone`-only.
    member: [u8; 32],
    /// The organization whose certificate verified at ingest.
    org: super::super::org::OrgId,
    /// The verified certificate's revocation generation.
    generation: u32,
}

impl VerifiedOwner {
    /// In-crate constructor — the verification bridge is the only
    /// legitimate producer; everything else consumes.
    pub(crate) fn new(
        member: &crate::adapter::net::identity::EntityId,
        org: super::super::org::OrgId,
        generation: u32,
    ) -> Self {
        Self {
            member: *member.as_bytes(),
            org,
            generation,
        }
    }

    /// The verified publishing entity.
    #[inline]
    pub fn member(&self) -> crate::adapter::net::identity::EntityId {
        crate::adapter::net::identity::EntityId::from_bytes(self.member)
    }

    /// The verified publishing entity, as raw bytes — the
    /// comparison form, free of the `EntityId` reconstruction.
    #[inline]
    pub fn member_bytes(&self) -> &[u8; 32] {
        &self.member
    }

    /// The vouching organization.
    #[inline]
    pub fn org(&self) -> super::super::org::OrgId {
        self.org
    }

    /// The verified certificate's revocation generation.
    #[inline]
    pub fn generation(&self) -> u32 {
        self.generation
    }
}

/// Query shapes the [`CapabilityFold`] answers.
///
/// `Composite` is the kitchen-sink form the scheduler uses;
/// individual single-axis variants exist so simpler callers
/// don't have to construct the full struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityQuery {
    /// Every member of a class regardless of state / tags.
    InClass(u64),
    /// Every entry carrying ALL of these tags. Set semantics —
    /// tags-all over an empty list matches everything.
    HasAllTags(Vec<String>),
    /// Every entry carrying AT LEAST ONE of these tags. Empty
    /// list matches nothing (vs `HasAllTags` empty matching
    /// everything — same asymmetric semantic the substrate
    /// uses for `require_any_tag` / `require_all_tags`).
    HasAnyTag(Vec<String>),
    /// Every entry currently in `state`.
    InState(NodeState),
    /// Every entry in `region` (exact string match).
    InRegion(String),
    /// Composite predicate — the scheduler's typical shape.
    /// Conjunctive AND across every populated field.
    Composite(CapabilityFilter),
}

/// Composite filter for [`CapabilityQuery::Composite`]. Every
/// `None` / empty field is "no constraint on this axis"; every
/// populated field tightens the candidate set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityFilter {
    /// Restrict to this class (None = any class).
    pub class: Option<u64>,
    /// Tags the entry MUST carry (intersection).
    pub tags_all: Vec<String>,
    /// Tags the entry must carry at least one of (union).
    /// Empty = no constraint.
    pub tags_any: Vec<String>,
    /// Conjunction of disjunctions: the entry must carry at least
    /// one tag from *every* group (AND across groups, OR within a
    /// group). Used for filter axes whose legacy semantics are
    /// "any of these must match" but which AND with the other
    /// axes — `require_models`, `require_tools`, `require_gpu`,
    /// `gpu_vendor` — encoded as the index-only synthetic tags
    /// `derive_synthetic_index_tags` produces. Empty = no
    /// constraint.
    pub tag_groups_all: Vec<Vec<String>>,
    /// State filter (None = any).
    pub state: Option<NodeState>,
    /// Region filter (None = any).
    pub region: Option<String>,
    /// Optional result cap. `0` = no cap.
    pub limit: usize,
}

impl CapabilityFilter {
    /// `true` when no field constrains the candidate set — every
    /// node in the fold is admissible. Per PERF_AUDIT §4.11 the
    /// bulk `find_nodes_matching` path short-circuits on this so
    /// the permissive case (e.g. `LegacyPlacement::permissive`)
    /// skips the full `HashSet<(class, NodeId)>` build + retain
    /// loop + sort + dedup that the general path runs.
    #[inline]
    pub fn is_permissive(&self) -> bool {
        self.class.is_none()
            && self.tags_all.is_empty()
            && self.tags_any.is_empty()
            && self.tag_groups_all.is_empty()
            && self.state.is_none()
            && self.region.is_none()
    }
}

/// One query result row.
pub type CapabilityMatch = ((u64, NodeId), CapabilityMembership);

/// Secondary index maintained alongside the primary
/// `(class, node) → CapabilityMembership` store. Three
/// inverted-index dimensions — by tag, by region, by state —
/// matching the plan's `CapabilityIndexInner` shape. Powers the
/// fast path for the most common query shapes (find-by-tag,
/// find-in-region, find-by-state) without scanning the full
/// store. `Composite` queries pick the most selective indexed
/// dimension and filter the others in-memory.
///
/// **Buckets are bitmaps over entry slots** (CAPABILITY_FOLD_SCALE_PLAN.md
/// Slice 7). Every indexed `(class, node)` entry holds one dense `u32`
/// slot (`SlotTable`); a bucket is the [`RoaringBitmap`] of the slots
/// carrying its tag / synthetic key / region / state. Identity is the
/// ENTRY, not the node: a tag in class A and a tag in class B of one node
/// sit in two slots, so a predicate never combines them. AND / OR are
/// bitmap operations; slots map back to keys through the table. The
/// outer maps keep a keyed hasher: tag and region strings are
/// publisher-chosen.
#[derive(Debug, Default)]
pub struct CapabilityIndexInner {
    /// tag → slots of the entries carrying that tag. Keyed by the
    /// canonical [`TagStr`], so a bucket shares its tag's one allocation
    /// with every entry instead of owning a copy; looked up by `&str`.
    by_tag: HashMap<TagStr, RoaringBitmap>,
    /// One dense slot per indexed entry. See [`SlotTable`].
    slots: SlotTable,
    /// One canonical [`TagStr`] per distinct published tag, with
    /// fold-owned use counts and the fold's tag budget
    /// (CAPABILITY_FOLD_SCALE_PLAN.md Slice 6). Synthetic keys are not in
    /// it: they live in `by_synthetic`, a separate namespace.
    dictionary: TagDictionary,
    /// Index-only synthetic tag (`model:`/`tool:`/`gpu:`) → set of
    /// (class, node) keys. Kept in a SEPARATE map from `by_tag` so a
    /// raw published tag string can never collide with a synthetic
    /// key: published tags are arbitrary strings (`Tag::Legacy`
    /// round-trips verbatim), so a publisher emitting a plain
    /// `"model:llama3"` tag must not be able to satisfy a
    /// `require_models` query it lacks the real bundle for. The bulk
    /// model/tool/gpu axes (`tag_groups_all`) resolve against this
    /// map only — see [`group_union`].
    by_synthetic: HashMap<String, RoaringBitmap>,
    /// region → slots of the entries in that region.
    by_region: HashMap<String, RoaringBitmap>,
    /// state → slots of the entries in that state.
    by_state: HashMap<NodeState, RoaringBitmap>,
    /// Reused buffer for building synthetic tag keys, so deriving them
    /// on insert and remove allocates nothing once it has grown.
    scratch: String,
}

/// Add `slot` to the bucket for `bucket`, allocating the bucket's
/// owned `String` only when the bucket is new.
fn bucket_insert(map: &mut HashMap<String, RoaringBitmap>, bucket: &str, slot: u32) {
    if let Some(bits) = map.get_mut(bucket) {
        bits.insert(slot);
    } else {
        map.entry(bucket.to_owned()).or_default().insert(slot);
    }
}

/// Remove `slot` from the bucket for `bucket`, dropping the bucket when
/// it empties.
fn bucket_remove(map: &mut HashMap<String, RoaringBitmap>, bucket: &str, slot: u32) {
    if let Some(bits) = map.get_mut(bucket) {
        bits.remove(slot);
        if bits.is_empty() {
            map.remove(bucket);
        }
    }
}

/// One dense `u32` slot per indexed `(class, node)` entry, the identity
/// the bucket bitmaps are over.
///
/// - A slot is acquired when an entry is indexed and released when it is
///   un-indexed; released slots go on a free list and are **reused before
///   the table grows**. So the slot space is bounded by the PEAK number
///   of live entries, not the current one: after 1,000 entries shrink to
///   100, the table can hold 1,000 positions with 900 free. Nothing is
///   compacted.
/// - A slot is released only after every bucket bit of its entry is
///   cleared (`on_remove` clears raw, synthetic, region and state bits
///   under the index write lock), so a reused slot inherits nothing.
/// - The slot space ends at [`SlotTable::limit`] (`u32::MAX` positions by
///   default): acquisition beyond it fails, it never wraps. `admit`
///   refuses an Insert when no slot is available.
#[derive(Debug)]
pub(crate) struct SlotTable {
    /// Keyed hasher: `class_hash` is publisher-chosen.
    slot_of: HashMap<(u64, NodeId), u32, foldhash::fast::RandomState>,
    key_of: Vec<Option<(u64, NodeId)>>,
    free: Vec<u32>,
    limit: usize,
}

impl Default for SlotTable {
    fn default() -> Self {
        Self {
            slot_of: HashMap::default(),
            key_of: Vec::new(),
            free: Vec::new(),
            limit: u32::MAX as usize,
        }
    }
}

impl SlotTable {
    /// Whether a new entry can get a slot.
    fn can_acquire(&self) -> bool {
        !self.free.is_empty() || self.key_of.len() < self.limit
    }

    /// The slot of `key`, assigning one (free list first) if it has none.
    /// `None` only when the slot space is exhausted.
    fn acquire(&mut self, key: (u64, NodeId)) -> Option<u32> {
        if let Some(&slot) = self.slot_of.get(&key) {
            return Some(slot);
        }
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                if self.key_of.len() >= self.limit {
                    return None;
                }
                let slot = u32::try_from(self.key_of.len()).ok()?;
                self.key_of.push(None);
                slot
            }
        };
        self.key_of[slot as usize] = Some(key);
        self.slot_of.insert(key, slot);
        Some(slot)
    }

    fn get(&self, key: &(u64, NodeId)) -> Option<u32> {
        self.slot_of.get(key).copied()
    }

    /// Release `key`'s slot to the free list.
    fn release(&mut self, key: &(u64, NodeId)) {
        if let Some(slot) = self.slot_of.remove(key) {
            self.key_of[slot as usize] = None;
            self.free.push(slot);
        }
    }

    /// The key holding `slot`, if occupied.
    fn key(&self, slot: u32) -> Option<(u64, NodeId)> {
        self.key_of.get(slot as usize).copied().flatten()
    }

    fn clear(&mut self) {
        self.slot_of.clear();
        self.key_of.clear();
        self.free.clear();
    }

    /// Every occupied slot, optionally only those of `class`.
    fn occupied(&self, class: Option<u64>) -> RoaringBitmap {
        let slots = self.key_of.iter().enumerate().filter_map(|(slot, key)| {
            key.filter(|k| class.is_none_or(|c| k.0 == c))
                .map(|_| slot as u32)
        });
        RoaringBitmap::from_sorted_iter(slots).unwrap_or_default()
    }

    fn occupied_len(&self) -> usize {
        self.slot_of.len()
    }

    /// Set the slot-space limit, for exhaustion tests.
    #[cfg(test)]
    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.limit = limit.min(u32::MAX as usize);
    }
}

impl CapabilityIndexInner {
    /// An empty index whose tag dictionary enforces `budget` instead of
    /// [`TagBudget::default`]. Pass to
    /// [`Fold::with_sweep_interval_and_index`](super::Fold::with_sweep_interval_and_index)
    /// for a deployment that needs more distinct tags.
    pub fn with_tag_budget(budget: TagBudget) -> Self {
        Self {
            dictionary: TagDictionary::with_budget(budget),
            ..Self::default()
        }
    }

    /// The tag dictionary, for tests.
    #[cfg(test)]
    pub(crate) fn dictionary(&self) -> &TagDictionary {
        &self.dictionary
    }

    /// Estimated heap bytes of this index, split so a change to one part
    /// cannot hide in another (CAPABILITY_FOLD_SCALE_PLAN.md Slice 7).
    /// Estimates from table capacities and set sizes, not a measurement:
    /// the reporter's counting allocator gives the measured totals.
    pub fn memory_breakdown(&self) -> IndexMemory {
        let mut mem = IndexMemory::default();
        let outer = |cap: usize, slot: usize| table_bytes(cap, slot) as u64;
        mem.bucket_maps = outer(
            self.by_tag.capacity(),
            std::mem::size_of::<(TagStr, IndexBucket)>(),
        ) + outer(
            self.by_synthetic.capacity(),
            std::mem::size_of::<(String, IndexBucket)>(),
        ) + outer(
            self.by_region.capacity(),
            std::mem::size_of::<(String, IndexBucket)>(),
        ) + outer(
            self.by_state.capacity(),
            std::mem::size_of::<(NodeState, IndexBucket)>(),
        );
        mem.bucket_key_strings = self
            .by_synthetic
            .keys()
            .chain(self.by_region.keys())
            .map(|k| k.capacity() as u64)
            .sum();
        let buckets = self
            .by_tag
            .values()
            .chain(self.by_synthetic.values())
            .chain(self.by_region.values())
            .chain(self.by_state.values());
        for bucket in buckets {
            mem.buckets += 1;
            mem.memberships += bucket_len(bucket) as u64;
            mem.bucket_sets += bucket_heap_bytes(bucket) as u64;
        }
        self.slot_memory(&mut mem);
        mem
    }
}

/// One inverted-index bucket: the slots carrying one tag / synthetic
/// key / region / state.
type IndexBucket = RoaringBitmap;

fn bucket_len(bucket: &IndexBucket) -> usize {
    bucket.len() as usize
}

/// A bitmap's container payload, by its serialized size, which tracks the
/// in-memory containers (2 bytes per member in an array container, 8 KiB
/// per bitmap container, runs for run containers) plus each container's
/// header. The `Vec` slack around the containers is not counted.
fn bucket_heap_bytes(bucket: &IndexBucket) -> usize {
    bucket.serialized_size()
}

impl CapabilityIndexInner {
    fn slot_memory(&self, mem: &mut IndexMemory) {
        let slots = &self.slots;
        mem.slot_table = (table_bytes(
            slots.slot_of.capacity(),
            std::mem::size_of::<((u64, NodeId), u32)>(),
        ) + slots.key_of.capacity()
            * std::mem::size_of::<Option<(u64, NodeId)>>()) as u64;
        mem.free_list = (slots.free.capacity() * std::mem::size_of::<u32>()) as u64;
        mem.occupied_slots = slots.occupied_len() as u64;
        mem.slot_capacity = slots.key_of.len() as u64;
        mem.free_slots = slots.free.len() as u64;
    }

    /// The slot table, for tests.
    #[cfg(test)]
    pub(crate) fn slots_mut(&mut self) -> &mut SlotTable {
        &mut self.slots
    }

    /// `key`'s slot, for tests.
    #[cfg(test)]
    pub(crate) fn slot_of(&self, key: &(u64, NodeId)) -> Option<u32> {
        self.slots.get(key)
    }

    /// The keys in `bits`, for tests and oracles.
    #[cfg(test)]
    fn keys_of(&self, bits: &RoaringBitmap) -> HashSet<(u64, NodeId)> {
        bits.iter()
            .filter_map(|slot| self.slots.key(slot))
            .collect()
    }
}

/// Estimated heap bytes of a hashbrown table of `capacity` with
/// `slot`-byte slots: power-of-two buckets at 7/8 load from 8 up, one
/// control byte per bucket plus a 16-byte trailing group.
fn table_bytes(capacity: usize, slot: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        (capacity + 1).next_power_of_two()
    } else {
        (capacity * 8 / 7).next_power_of_two()
    };
    (buckets * slot).next_multiple_of(16) + buckets + 16
}

/// [`CapabilityIndexInner::memory_breakdown`]'s result. Bucket bytes are
/// the inverted index itself; slot bytes are the bookkeeping that maps
/// bucket members to keys (zero while buckets hold keys directly).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexMemory {
    /// Buckets across the raw-tag, synthetic, region and state maps.
    pub buckets: u64,
    /// Bucket memberships: one per (bucket, entry) pair.
    pub memberships: u64,
    /// The four outer maps' tables.
    pub bucket_maps: u64,
    /// The bucket sets themselves.
    pub bucket_sets: u64,
    /// Owned key strings of the synthetic and region maps (raw-tag keys
    /// share the tag dictionary's allocation and are not counted).
    pub bucket_key_strings: u64,
    /// Slot table: key-to-slot map and slot-to-key vector.
    pub slot_table: u64,
    /// Free-slot list.
    pub free_list: u64,
    /// Occupied slots.
    pub occupied_slots: u64,
    /// Allocated slot positions (occupied plus free).
    pub slot_capacity: u64,
    /// Free slots awaiting reuse.
    pub free_slots: u64,
}

impl IndexMemory {
    /// Everything the inverted index costs: maps, sets and key strings.
    pub fn bucket_total(&self) -> u64 {
        self.bucket_maps + self.bucket_sets + self.bucket_key_strings
    }
}

/// [`bucket_insert`] for the raw-tag map: a new bucket takes a clone of
/// the canonical handle (a refcount bump, no copy of the text).
fn tag_bucket_insert(map: &mut HashMap<TagStr, RoaringBitmap>, tag: &TagStr, slot: u32) {
    if let Some(bits) = map.get_mut(tag.as_str()) {
        bits.insert(slot);
    } else {
        map.entry(tag.clone()).or_default().insert(slot);
    }
}

/// [`bucket_remove`] for the raw-tag map.
fn tag_bucket_remove(map: &mut HashMap<TagStr, RoaringBitmap>, tag: &str, slot: u32) {
    if let Some(bits) = map.get_mut(tag) {
        bits.remove(slot);
        if bits.is_empty() {
            map.remove(tag);
        }
    }
}

impl FoldIndex<CapabilityFold> for CapabilityIndexInner {
    fn on_insert(&mut self, key: &(u64, NodeId), payload: &CapabilityMembership) {
        // `admit` refused the Insert if no slot was available, and a
        // Replace re-indexes into the slot its `on_remove` just freed.
        let Some(slot) = self.slots.acquire(*key) else {
            debug_assert!(false, "no slot for an admitted entry");
            return;
        };
        for tag in &payload.tags {
            tag_bucket_insert(&mut self.by_tag, tag, slot);
        }
        // Index-only synthetic tags (model:/tool:/gpu:) live in
        // their own `by_synthetic` map so the model / tool / gpu
        // filter axes resolve without a per-query full scan and
        // without risking collision against a raw published tag of
        // the same string. Derived here at insert, never per query.
        let mut scratch = std::mem::take(&mut self.scratch);
        for_each_synthetic_index_tag(payload, &mut scratch, |tag| {
            bucket_insert(&mut self.by_synthetic, tag, slot);
        });
        self.scratch = scratch;
        if let Some(region) = &payload.region {
            bucket_insert(&mut self.by_region, region, slot);
        }
        self.by_state.entry(payload.state).or_default().insert(slot);
    }

    fn on_remove(&mut self, key: &(u64, NodeId), payload: &CapabilityMembership) {
        let Some(slot) = self.slots.get(key) else {
            return;
        };
        for tag in &payload.tags {
            tag_bucket_remove(&mut self.by_tag, tag, slot);
        }
        // Mirror the synthetic tags added in `on_insert`. Derived
        // from the same payload, so the set is identical.
        let mut scratch = std::mem::take(&mut self.scratch);
        for_each_synthetic_index_tag(payload, &mut scratch, |tag| {
            bucket_remove(&mut self.by_synthetic, tag, slot);
        });
        self.scratch = scratch;
        if let Some(region) = &payload.region {
            bucket_remove(&mut self.by_region, region, slot);
        }
        if let Some(bits) = self.by_state.get_mut(&payload.state) {
            bits.remove(slot);
            if bits.is_empty() {
                self.by_state.remove(&payload.state);
            }
        }
        // Every bit of this entry is cleared above, so the slot goes back
        // to the free list clean: its next holder inherits nothing.
        self.slots.release(key);
    }

    fn clear(&mut self) {
        self.dictionary.clear();
        self.slots.clear();
        self.by_tag.clear();
        self.by_synthetic.clear();
        self.by_region.clear();
        self.by_state.clear();
    }

    /// PERF_AUDIT §4.5 — `on_insert` keys this index on
    /// `(tags, derived synthetic tags, region, state)`. If the two
    /// payloads agree on every one of those, an on_remove +
    /// on_insert against them nets to a no-op on every bucket
    /// (`derive_synthetic_index_tags` is pure over the payload, so
    /// identical inputs produce identical synthetic outputs).
    ///
    /// The synthetic tags derive from TWO payload fields: the
    /// `software.model.*` / `software.tool.*` bundles inside
    /// `tags` (covered by the `tags` equality) AND the
    /// `gpu:present` / `gpu:vendor:<v>` projection of `hardware`
    /// — so `hardware` MUST be part of this comparison or a
    /// refresh that changes only the GPU shape would leave
    /// `by_synthetic` stale. Comparing the whole
    /// `HardwareSummary` is slightly conservative (a
    /// memory_gb/vram_gb-only delta forces a rebuild the index
    /// doesn't strictly need), but the steady-state refresh the
    /// audit targets keeps hardware identical, so the win is
    /// unaffected and the check stays future-proof against new
    /// hardware-derived synthetic tags. Allow-lists / metadata /
    /// price_quote / reflex_addr are NOT consulted by this index
    /// and may differ freely.
    fn index_payload_equivalent(old: &CapabilityMembership, new: &CapabilityMembership) -> bool {
        old.state == new.state
            && old.region == new.region
            && old.tags == new.tags
            && old.hardware == new.hardware
    }

    /// Canonicalize the incoming tags through the dictionary, net of the
    /// outgoing payload's release. See [`TagDictionary::admit`].
    fn admit(
        &mut self,
        incoming: &mut CapabilityMembership,
        outgoing: Option<&CapabilityMembership>,
    ) -> Result<(), PayloadRejection> {
        // An Insert needs a fresh slot; a Replace reuses its own. Checked
        // first, so a refusal leaves the dictionary untouched.
        if outgoing.is_none() && !self.slots.can_acquire() {
            return Err(PayloadRejection::IndexFull {
                slots: self.slots.limit,
            });
        }
        self.dictionary
            .admit(&mut incoming.tags, outgoing.map(|p| p.tags.as_slice()))
    }

    fn release(&mut self, payload: &CapabilityMembership) {
        self.dictionary.release(&payload.tags);
    }

    fn preflight_restore(
        &self,
        payloads: &[&CapabilityMembership],
    ) -> Result<(), PayloadRejection> {
        if payloads.len() > self.slots.limit {
            return Err(PayloadRejection::IndexFull {
                slots: self.slots.limit,
            });
        }
        self.dictionary
            .preflight(payloads.iter().map(|p| p.tags.as_slice()))
    }

    fn entry_capacity(&self) -> usize {
        self.slots.limit
    }

    fn admission_stats(&self) -> AdmissionStats {
        let stats = self.dictionary.stats();
        AdmissionStats {
            interned: stats.tags,
            interned_bytes: stats.bytes,
            overhead_bytes: stats.overhead_bytes,
        }
    }
}

/// Derive the index-only synthetic tags for a membership: the
/// `model:<id>` / `tool:<id>` / `gpu:present` / `gpu:vendor:<v>`
/// keys that let the secondary index resolve the filter axes the
/// plain-tag index doesn't natively carry.
///
/// These live ONLY in the index — they are never written into
/// `payload.tags`, so tag enumeration (`capability_tags_for`) is
/// unaffected. Models / tools are read from the canonical
/// `software.model.<i>.id=<v>` / `software.tool.<i>.tool_id=<v>`
/// bundles using the same `Tag::AxisValue` shape
/// `CapabilitySet::has_model` / `has_tool` match; GPU presence /
/// vendor come from the hardware projection, matching the legacy
/// `require_gpu` (`gpu_count > 0 || gpu_vendor.is_some()`) and
/// `gpu_vendor` predicates.
///
/// Must stay the exact inverse of the tags `translate_filter`
/// emits, or model / tool / gpu queries silently diverge between
/// the bulk index path and the single-target post-filter path —
/// `target_matches_filter_agrees_with_find_nodes_matching` guards
/// this.
#[cfg(test)]
fn derive_synthetic_index_tags(payload: &CapabilityMembership) -> Vec<String> {
    let mut out = Vec::new();
    let mut scratch = String::new();
    for_each_synthetic_index_tag(payload, &mut scratch, |tag| out.push(tag.to_owned()));
    out
}

/// Visit each index-only synthetic tag of `payload`, in a fixed order
/// (the test-only `derive_synthetic_index_tags` collects it), building
/// each key in
/// `scratch` instead of allocating. Tags are read with
/// [`axis_value_ref`](super::super::tag::axis_value_ref), the borrowing
/// form of `Tag::parse`, so the accepted grammar is `Tag::parse`'s
/// exactly: both `=` and `:` separators, reserved prefixes excluded.
/// `synthetic_derivation_matches_tag_parse` pins that equivalence.
fn for_each_synthetic_index_tag(
    payload: &CapabilityMembership,
    scratch: &mut String,
    mut visit: impl FnMut(&str),
) {
    use super::super::tag::{axis_value_ref, TaxonomyAxis};
    let mut emit = |prefix: &str, value: &str| {
        scratch.clear();
        scratch.push_str(prefix);
        scratch.push_str(value);
        visit(scratch);
    };
    for s in &payload.tags {
        let Some((TaxonomyAxis::Software, key, value)) = axis_value_ref(s) else {
            continue;
        };
        if let Some(rest) = key.strip_prefix("model.") {
            if matches!(rest.split_once('.'), Some((_, "id"))) {
                emit("model:", value);
            }
        } else if let Some(rest) = key.strip_prefix("tool.") {
            if matches!(rest.split_once('.'), Some((_, "tool_id"))) {
                emit("tool:", value);
            }
        }
    }
    if let Some(h) = &payload.hardware {
        if h.gpu_count > 0 || h.gpu_vendor.is_some() {
            emit("gpu:present", "");
        }
        if let Some(vendor) = &h.gpu_vendor {
            emit("gpu:vendor:", vendor);
        }
    }
}

/// The pre-Slice-3 derivation, through the owning `Tag::parse`: the
/// oracle `synthetic_derivation_matches_tag_parse` compares the
/// borrowing derivation against.
#[cfg(test)]
fn derive_synthetic_index_tags_via_parse(payload: &CapabilityMembership) -> Vec<String> {
    use super::super::tag::{Tag, TaxonomyAxis};
    let mut out = Vec::new();
    for s in &payload.tags {
        let Ok(Tag::AxisValue {
            axis: TaxonomyAxis::Software,
            key,
            value,
            ..
        }) = Tag::parse(s)
        else {
            continue;
        };
        if let Some(rest) = key.strip_prefix("model.") {
            if matches!(rest.split_once('.'), Some((_, "id"))) {
                out.push(format!("model:{value}"));
            }
        } else if let Some(rest) = key.strip_prefix("tool.") {
            if matches!(rest.split_once('.'), Some((_, "tool_id"))) {
                out.push(format!("tool:{value}"));
            }
        }
    }
    if let Some(h) = &payload.hardware {
        if h.gpu_count > 0 || h.gpu_vendor.is_some() {
            out.push("gpu:present".to_string());
        }
        if let Some(vendor) = &h.gpu_vendor {
            out.push(format!("gpu:vendor:{vendor}"));
        }
    }
    out
}

/// Marker type for the [`FoldKind`] impl.
#[derive(Debug)]
pub struct CapabilityFold;

impl FoldKind for CapabilityFold {
    /// Reserved built-in fold id `1` per the plan's
    /// "Reserved range" note in [`FoldKind::KIND_ID`].
    const KIND_ID: u16 = 1;
    const CHANNEL_PREFIX: &'static str = "fold:cap:";
    /// 60-second TTL matches the plan's recommendation: the
    /// background sweeper removes stale memberships that
    /// haven't been refreshed within a minute. Operator-tuned
    /// per-announcement TTLs override.
    const DEFAULT_TTL: Duration = Duration::from_secs(60);

    type Key = (u64, NodeId);
    type Payload = CapabilityMembership;
    type Query = CapabilityQuery;
    type Result = Vec<CapabilityMatch>;
    type Index = CapabilityIndexInner;
    /// A keyed fast hasher. The key's `class_hash` is publisher-declared,
    /// so an unkeyed mixer would let a publisher manufacture colliding
    /// keys and turn this map's lookups into probe-chain scans.
    /// `foldhash::fast::RandomState` seeds every map instance at random
    /// and uses a folded multiply, so collisions cannot be built without
    /// the receiver's seed, at near-Fx speed (it is hashbrown's default
    /// hasher). Pinned by `capability_fold_primary_map_hasher_is_keyed`.
    type KeyHasher = foldhash::fast::RandomState;

    /// The per-advertisement tag caps (CAPABILITY_FOLD_SCALE_PLAN.md
    /// Slice 6): at most [`super::MAX_CAPABILITY_TAGS`] tags, duplicates
    /// counted, of at most [`super::MAX_CAPABILITY_TAG_LEN`] UTF-8 bytes.
    fn validate(payload: &Self::Payload) -> Result<(), PayloadRejection> {
        validate_capability_tags(&payload.tags)
    }

    fn key_for(node_id: NodeId, payload: &Self::Payload) -> Self::Key {
        (payload.class_hash, node_id)
    }

    fn build_index() -> CapabilityIndexInner {
        CapabilityIndexInner::default()
    }

    fn query(
        state: &FoldState<Self>,
        index: &CapabilityIndexInner,
        query: CapabilityQuery,
    ) -> Vec<CapabilityMatch> {
        match query {
            CapabilityQuery::InClass(class) => state
                .entries
                .iter()
                .filter(|((c, _), _)| *c == class)
                .map(|(k, e)| (*k, e.payload.clone()))
                .collect(),
            CapabilityQuery::HasAllTags(tags) => {
                materialize(state, index, &resolve_keys_all_tags(index, &tags))
            }
            CapabilityQuery::HasAnyTag(tags) => {
                let mut any = RoaringBitmap::new();
                for tag in &tags {
                    if let Some(bits) = index.by_tag.get(tag.as_str()) {
                        any |= bits;
                    }
                }
                materialize(state, index, &any)
            }
            CapabilityQuery::InState(s) => match index.by_state.get(&s) {
                Some(bits) => materialize(state, index, bits),
                None => Vec::new(),
            },
            CapabilityQuery::InRegion(r) => match index.by_region.get(&r) {
                Some(bits) => materialize(state, index, bits),
                None => Vec::new(),
            },
            CapabilityQuery::Composite(filter) => composite_query(state, index, &filter),
        }
    }
}

/// The matches for the entries whose slots are in `bits`: each slot
/// mapped to its key, then its live entry (an index slot with no live
/// entry behind it is dropped), payload cloned.
fn materialize(
    state: &FoldState<CapabilityFold>,
    index: &CapabilityIndexInner,
    bits: &RoaringBitmap,
) -> Vec<CapabilityMatch> {
    bits.iter()
        .filter_map(|slot| index.slots.key(slot))
        .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
        .collect()
}

/// The slots of the entries that carry EVERY tag in `tags`: the
/// intersection of their raw-tag buckets, seeded from the smallest.
/// Empty `tags` returns every indexed entry (the `tags_all = []` "no
/// constraint" convention).
fn resolve_keys_all_tags(index: &CapabilityIndexInner, tags: &[String]) -> RoaringBitmap {
    if tags.is_empty() {
        return index.slots.occupied(None);
    }
    let mut buckets: Vec<&RoaringBitmap> = Vec::with_capacity(tags.len());
    for tag in tags {
        match index.by_tag.get(tag.as_str()) {
            Some(bits) => buckets.push(bits),
            None => return RoaringBitmap::new(),
        }
    }
    buckets.sort_by_key(|bits| bits.len());
    let mut out = buckets[0].clone();
    for bits in &buckets[1..] {
        out &= *bits;
        if out.is_empty() {
            break;
        }
    }
    out
}

/// Candidate set returned by [`resolve_candidate_keys`]: a bitmap of
/// entry slots, borrowed from the index when one bucket IS the answer
/// (a single-constraint filter: no copy at all) or owned when it had to
/// be combined, plus the slot table that maps slots back to keys.
///
/// Iteration order is slot order, which is arrival order, not `NodeId`
/// order. Every consumer that returns nodes sorts and deduplicates
/// explicitly (CAPABILITY_FOLD_SCALE_PLAN.md B2 decision 2).
pub(crate) struct CandidateKeys<'a> {
    bits: Bits<'a>,
    slots: &'a SlotTable,
}

enum Bits<'a> {
    Borrowed(&'a RoaringBitmap),
    Owned(RoaringBitmap),
}

impl Bits<'_> {
    fn get(&self) -> &RoaringBitmap {
        match self {
            Self::Borrowed(bits) => bits,
            Self::Owned(bits) => bits,
        }
    }

    fn into_owned(self) -> RoaringBitmap {
        match self {
            Self::Borrowed(bits) => bits.clone(),
            Self::Owned(bits) => bits,
        }
    }
}

impl<'a> CandidateKeys<'a> {
    fn new(index: &'a CapabilityIndexInner, bits: Bits<'a>) -> Self {
        Self {
            bits,
            slots: &index.slots,
        }
    }

    fn empty(index: &'a CapabilityIndexInner) -> Self {
        Self::new(index, Bits::Owned(RoaringBitmap::new()))
    }

    /// Number of candidate entries.
    pub(crate) fn len(&self) -> usize {
        self.bits.get().len() as usize
    }

    /// The candidate `(class, node)` keys, in slot order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = (u64, NodeId)> + '_ {
        self.bits
            .get()
            .iter()
            .filter_map(|slot| self.slots.key(slot))
    }

    /// The candidate keys as a set, for tests.
    #[cfg(test)]
    pub(crate) fn key_set(&self) -> HashSet<(u64, NodeId)> {
        self.keys().collect()
    }

    /// Whether the set is an index bucket borrowed as is, for tests.
    #[cfg(test)]
    pub(crate) fn is_borrowed(&self) -> bool {
        matches!(self.bits, Bits::Borrowed(_))
    }
}

/// Resolve the `(class, node)` keys a [`CapabilityFilter`] selects on
/// its *indexed* axes (tags, synthetic groups, state, region, class) as
/// a bitmap of entry slots.
///
/// - Single-constraint filters return the index bucket borrowed: the
///   bucket already IS the answer, so nothing is copied.
/// - Otherwise every indexed constraint is a bucket (borrowed) or a
///   union (an owned OR, for a multi-tag group or `tags_any`); the result
///   is their AND, seeded from the smallest. A class predicate (not
///   indexed) filters the result by the slot's key.
///
/// Does NOT clone any payload, and does NOT apply `filter.limit` or the
/// non-indexed predicates (hardware / model / tool). Callers that only
/// need keys, or that post-filter against borrowed payloads, use this
/// via [`Fold::with_state_and_index`]; [`composite_query`] layers payload
/// materialization and the limit on top.
pub(crate) fn resolve_candidate_keys<'a>(
    _state: &FoldState<CapabilityFold>,
    index: &'a CapabilityIndexInner,
    filter: &CapabilityFilter,
) -> CandidateKeys<'a> {
    // Single-constraint fast path (2026-06-11 service-discovery
    // follow-up): the index bucket already IS the final candidate set.
    // `tags_all` resolves against `by_tag` only (synthetic model / tool /
    // gpu axes ride `tag_groups_all` → `by_synthetic`), so borrowing the
    // raw-tag bucket cannot leak a synthetic match.
    if filter.tag_groups_all.is_empty() && filter.tags_any.is_empty() && filter.class.is_none() {
        let only = match (&filter.tags_all[..], filter.state, &filter.region) {
            ([tag], None, None) => Some(index.by_tag.get(tag.as_str())),
            ([], Some(state_filter), None) => Some(index.by_state.get(&state_filter)),
            ([], None, Some(region)) => Some(index.by_region.get(region)),
            _ => None,
        };
        if let Some(bucket) = only {
            return match bucket {
                Some(bits) => CandidateKeys::new(index, Bits::Borrowed(bits)),
                None => CandidateKeys::empty(index),
            };
        }
    }

    // General path. A missing bucket for any AND-ed constraint empties the
    // result at once.
    let mut sets: Vec<Bits<'a>> = Vec::new();
    for tag in &filter.tags_all {
        match index.by_tag.get(tag.as_str()) {
            Some(bits) => sets.push(Bits::Borrowed(bits)),
            None => return CandidateKeys::empty(index),
        }
    }
    // Empty groups carry no constraint and are skipped.
    for group in filter.tag_groups_all.iter().filter(|g| !g.is_empty()) {
        if let [tag] = &group[..] {
            // Synthetic axes resolve against `by_synthetic` only; see
            // `group_union`.
            match index.by_synthetic.get(tag) {
                Some(bits) => sets.push(Bits::Borrowed(bits)),
                None => return CandidateKeys::empty(index),
            }
        } else {
            let union = group_union(index, group);
            if union.is_empty() {
                return CandidateKeys::empty(index);
            }
            sets.push(Bits::Owned(union));
        }
    }
    if let Some(state_filter) = filter.state {
        match index.by_state.get(&state_filter) {
            Some(bits) => sets.push(Bits::Borrowed(bits)),
            None => return CandidateKeys::empty(index),
        }
    }
    if let Some(region) = &filter.region {
        match index.by_region.get(region) {
            Some(bits) => sets.push(Bits::Borrowed(bits)),
            None => return CandidateKeys::empty(index),
        }
    }
    if let [tag] = &filter.tags_any[..] {
        match index.by_tag.get(tag.as_str()) {
            Some(bits) => sets.push(Bits::Borrowed(bits)),
            None => return CandidateKeys::empty(index),
        }
    } else if !filter.tags_any.is_empty() {
        let mut union = RoaringBitmap::new();
        for tag in &filter.tags_any {
            if let Some(bits) = index.by_tag.get(tag.as_str()) {
                union |= bits;
            }
        }
        if union.is_empty() {
            return CandidateKeys::empty(index);
        }
        sets.push(Bits::Owned(union));
    }

    let Some(class) = filter.class else {
        if sets.is_empty() {
            // No constraint at all: every indexed entry.
            return CandidateKeys::new(index, Bits::Owned(index.slots.occupied(None)));
        }
        if sets.len() == 1 {
            // One set constraint and no class predicate: that set IS the
            // answer, borrowed when it is an index bucket.
            if let Some(only) = sets.pop() {
                return CandidateKeys::new(index, only);
            }
        }
        return CandidateKeys::new(index, Bits::Owned(intersect(sets)));
    };
    if sets.is_empty() {
        // Only a class predicate: the class's occupied slots.
        return CandidateKeys::new(index, Bits::Owned(index.slots.occupied(Some(class))));
    }
    let both = intersect(sets);
    let of_class = both
        .iter()
        .filter(|&slot| index.slots.key(slot).is_some_and(|k| k.0 == class));
    CandidateKeys::new(
        index,
        Bits::Owned(RoaringBitmap::from_sorted_iter(of_class).unwrap_or_default()),
    )
}

/// The AND of `sets`, seeded from the smallest. `sets` must be non-empty.
fn intersect(mut sets: Vec<Bits<'_>>) -> RoaringBitmap {
    let Some(seed_at) = (0..sets.len()).min_by_key(|&i| sets[i].get().len()) else {
        return RoaringBitmap::new();
    };
    let mut out = sets.swap_remove(seed_at).into_owned();
    for other in &sets {
        if out.is_empty() {
            break;
        }
        out &= other.get();
    }
    out
}

/// The resolver before Slice 4: clone the seed, then `retain` through
/// every other constraint. Kept as the oracle for
/// `resolve_candidate_keys_matches_pre_slice4_resolver`. Since Slice 7 it
/// works on KEY sets read out of the buckets (`keys_of`), not on the
/// bitmaps, so it shares no set algebra with the resolver it checks.
#[cfg(test)]
fn resolve_candidate_keys_pre_slice4(
    state: &FoldState<CapabilityFold>,
    index: &CapabilityIndexInner,
    filter: &CapabilityFilter,
) -> HashSet<(u64, NodeId)> {
    type Keys = HashSet<(u64, NodeId)>;
    let tag_keys = |tag: &str| index.by_tag.get(tag).map(|b| index.keys_of(b));
    let state_keys = |s: &NodeState| index.by_state.get(s).map(|b| index.keys_of(b));
    let region_keys = |r: &str| index.by_region.get(r).map(|b| index.keys_of(b));
    let group_keys = |group: &[String]| -> Keys {
        let mut union = Keys::new();
        for tag in group {
            if let Some(bits) = index.by_synthetic.get(tag) {
                union.extend(index.keys_of(bits));
            }
        }
        union
    };

    if filter.tag_groups_all.is_empty() && filter.tags_any.is_empty() && filter.class.is_none() {
        match (&filter.tags_all[..], filter.state, &filter.region) {
            ([tag], None, None) => return tag_keys(tag).unwrap_or_default(),
            ([], Some(state_filter), None) => return state_keys(&state_filter).unwrap_or_default(),
            ([], None, Some(region)) => return region_keys(region).unwrap_or_default(),
            _ => {}
        }
    }

    let mut group_unions: Vec<Keys> = Vec::new();
    let build_group_unions = || -> Vec<Keys> {
        filter
            .tag_groups_all
            .iter()
            .filter(|g| !g.is_empty())
            .map(|g| group_keys(g))
            .collect()
    };

    let mut candidates: Keys = if !filter.tags_all.is_empty() {
        let mut seed: Option<Keys> = None;
        for tag in &filter.tags_all {
            let bucket = tag_keys(tag).unwrap_or_default();
            seed = Some(match seed {
                None => bucket,
                Some(mut acc) => {
                    acc.retain(|k| bucket.contains(k));
                    acc
                }
            });
        }
        let seed = seed.unwrap_or_default();
        if !seed.is_empty() {
            group_unions = build_group_unions();
        }
        seed
    } else {
        group_unions = build_group_unions();
        if !group_unions.is_empty() {
            let smallest = group_unions
                .iter()
                .enumerate()
                .min_by_key(|(_, u)| u.len())
                .map(|(i, _)| i)
                .unwrap_or(0);
            group_unions.swap_remove(smallest)
        } else if let Some(state_filter) = filter.state {
            state_keys(&state_filter).unwrap_or_default()
        } else if let Some(region) = &filter.region {
            region_keys(region).unwrap_or_default()
        } else if let Some(class) = filter.class {
            state
                .entries
                .keys()
                .filter(|(c, _)| *c == class)
                .copied()
                .collect()
        } else {
            state.entries.keys().copied().collect()
        }
    };

    if let Some(class) = filter.class {
        candidates.retain(|(c, _)| *c == class);
    }
    if let Some(state_filter) = filter.state {
        let bucket = state_keys(&state_filter).unwrap_or_default();
        candidates.retain(|k| bucket.contains(k));
    }
    if let Some(region) = &filter.region {
        let bucket = region_keys(region).unwrap_or_default();
        candidates.retain(|k| bucket.contains(k));
    }
    if !filter.tags_any.is_empty() {
        let mut any = Keys::new();
        for tag in &filter.tags_any {
            any.extend(tag_keys(tag).unwrap_or_default());
        }
        candidates.retain(|k| any.contains(k));
    }
    for union in &group_unions {
        candidates.retain(|k| union.contains(k));
    }
    candidates
}

/// Union of the slots carrying at least one tag in `group`, the
/// OR-within-a-group half of `tag_groups_all`.
///
/// Resolves against `by_synthetic`, NOT `by_tag`: every
/// `tag_groups_all` entry is an index-only synthetic key
/// (`model:`/`tool:`/`gpu:`) manufactured by
/// `derive_synthetic_index_tags`. Reading the synthetic map keeps a
/// raw published tag of the same string from satisfying a model /
/// tool / gpu axis it has no real bundle / hardware for.
fn group_union(index: &CapabilityIndexInner, group: &[String]) -> RoaringBitmap {
    let mut union = RoaringBitmap::new();
    for tag in group {
        if let Some(bits) = index.by_synthetic.get(tag) {
            union |= bits;
        }
    }
    union
}

/// Evaluate a [`CapabilityQuery::Composite`] filter — resolves
/// the indexed-axis candidate set via [`resolve_candidate_keys`],
/// then materializes each match (cloning the payload) and applies
/// `filter.limit`.
fn composite_query(
    state: &FoldState<CapabilityFold>,
    index: &CapabilityIndexInner,
    filter: &CapabilityFilter,
) -> Vec<CapabilityMatch> {
    let candidates = resolve_candidate_keys(state, index, filter);
    // Materialize matches + apply limit during materialization.
    //
    // PERF_AUDIT §4.10 — pre-fix this collected every match (deep-
    // cloning every `CapabilityMembership` payload — tags Vec,
    // metadata BTreeMap, allow-lists) and only truncated AFTER. A
    // query with a small `limit` against a large candidate set
    // paid the full deep-clone cost on every over-limit match
    // just to drop it on the next line. With `take` before
    // `collect`, the clone runs exactly `limit` times.
    let it = candidates
        .keys()
        .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())));
    if filter.limit > 0 {
        it.take(filter.limit).collect()
    } else {
        it.collect()
    }
}

/// Return the union of every tag this publisher has advertised
/// across its [`CapabilityMembership`] class entries. Walks the
/// publisher's `by_node` reverse index; O(num classes * tags
/// per class), typically tiny. Used by the dataforts greedy
/// admission path to feed the scope gate after origin_hash →
/// node_id resolution.
///
/// Callers iterating over every publisher should use
/// [`capability_tags_for_all`] instead — single-shot batched
/// variant that avoids the `1 + N` `with_state` lock pattern.
pub fn capability_tags_for(fold: &super::Fold<CapabilityFold>, node_id: NodeId) -> Vec<String> {
    fold.with_state(|state| tags_union_for(state, node_id))
}

/// Return `(node_id, tags)` pairs for every publisher in the fold
/// under one `with_state` lock. Equivalent to
/// `state.by_node.keys().map(|n| (n, capability_tags_for(fold, n)))`
/// but acquires the lock once instead of `1 + N` times — the
/// planner's coverage walk and similar full-fold sweeps want this
/// shape.
pub fn capability_tags_for_all(
    fold: &super::Fold<CapabilityFold>,
) -> std::collections::HashMap<NodeId, Vec<String>> {
    fold.with_state(|state| {
        let mut out: std::collections::HashMap<NodeId, Vec<String>> =
            std::collections::HashMap::with_capacity(state.by_node.len());
        for node_id in state.by_node.keys() {
            out.insert(*node_id, tags_union_for(state, *node_id));
        }
        out
    })
}

/// Shared implementation: union the publisher's tag set across
/// every class entry it owns. Callers hold the state read lock.
/// Of `node_ids`, those advertising `tag` in their folded capability
/// set — computed under a single `with_state` lock, with no per-node
/// tag-`Vec` allocation. Coordinator selection filters its direct-peer
/// candidates by `RELAY_CAPABLE_TAG` this way, instead of taking the
/// fold lock and materializing a full tag union once per candidate
/// (a `1 + N`-lock, `N`-allocation pattern).
pub fn nodes_with_capability_tag(
    fold: &super::Fold<CapabilityFold>,
    node_ids: &[NodeId],
    tag: &str,
) -> std::collections::HashSet<NodeId> {
    fold.with_state(|state| {
        node_ids
            .iter()
            .copied()
            .filter(|node_id| node_has_tag(state, *node_id, tag))
            .collect()
    })
}

/// Whether `node_id` advertises `tag` in any of its folded class
/// entries. Non-allocating — walks the publisher's `by_node` reverse
/// index and short-circuits on the first match.
fn node_has_tag(state: &FoldState<CapabilityFold>, node_id: NodeId, tag: &str) -> bool {
    let Some(keys) = state.keys_for(node_id) else {
        return false;
    };
    keys.iter().any(|key| {
        state
            .entries
            .get(key)
            .is_some_and(|entry| entry.payload.tags.iter().any(|t| t == tag))
    })
}

fn tags_union_for(state: &FoldState<CapabilityFold>, node_id: NodeId) -> Vec<String> {
    let Some(keys) = state.keys_for(node_id) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for key in keys {
        if let Some(entry) = state.entries.get(key) {
            for tag in &entry.payload.tags {
                seen.insert(tag.as_str());
            }
        }
    }
    seen.into_iter().map(str::to_owned).collect()
}

/// Return `node_id`'s last-advertised reflex `SocketAddr`, or
/// `None` if no entry from that publisher carries one. Walks the
/// publisher's class entries via the `by_node` reverse index;
/// O(num classes this publisher is in), typically 0-3. Used by
/// NAT-traversal rendezvous (stage 3) — the punch coordinator
/// looks up the target's public address before scheduling the
/// punch fire.
pub fn reflex_addr_for(
    fold: &super::Fold<CapabilityFold>,
    node_id: NodeId,
) -> Option<std::net::SocketAddr> {
    fold.with_state(|state| {
        let keys = state.keys_for(node_id)?;
        for key in keys {
            if let Some(entry) = state.entries.get(key) {
                if let Some(addr) = entry.payload.reflex_addr {
                    return Some(addr);
                }
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    /// The capability fold's primary map and reverse index must use a
    /// keyed hasher: `class_hash` is publisher-declared, so an unkeyed one
    /// lets a publisher build colliding keys (PR #1198 review). Two
    /// independently built states must hash the same key differently;
    /// an unkeyed hasher hashes it identically in both.
    #[test]
    fn capability_fold_primary_map_hasher_is_keyed() {
        use std::hash::BuildHasher;
        let a = FoldState::<CapabilityFold>::new();
        let b = FoldState::<CapabilityFold>::new();
        let keys: [(u64, NodeId); 4] = [(0, 1), (0x100, 0xA), (u64::MAX, 7), (42, 42)];
        let differs = |x: &dyn Fn(&(u64, NodeId)) -> u64, y: &dyn Fn(&(u64, NodeId)) -> u64| {
            keys.iter().any(|k| x(k) != y(k))
        };
        assert!(
            differs(&|k| a.entries.hasher().hash_one(k), &|k| b
                .entries
                .hasher()
                .hash_one(k)),
            "entries hasher is not seeded per instance"
        );
        assert!(
            differs(&|k| a.by_node.hasher().hash_one(k.1), &|k| b
                .by_node
                .hasher()
                .hash_one(k.1)),
            "by_node hasher is not seeded per instance"
        );
    }

    /// CAPABILITY_FOLD_SCALE_PLAN.md Slice 4, kept permanently: the
    /// streaming resolver returns exactly the pre-Slice-4 resolver's
    /// key set over a filter matrix on a 10k fixture. The fixture has
    /// multi-class publishers (a second class whose tags differ, so an
    /// entry-level split predicate is exercised), and is built in both
    /// arrival orders, which must agree with each other too.
    #[test]
    fn resolve_candidate_keys_matches_pre_slice4_resolver() {
        use std::collections::BTreeMap;
        const N: u64 = 10_000;

        let membership = |class: u64, i: u64| {
            let mut tags = vec![format!("a{}", i % 4), format!("b{}", i % 7)];
            if i.is_multiple_of(997) {
                tags.push("rare".into());
            }
            tags.push(format!("software.model.0.id=m{}", i % 3));
            tags.push(format!("software.tool.0.tool_id:x{}", i % 2));
            if class == 0x200 {
                tags = vec!["a0".into(), "cross".into(), "software.model.0.id=m9".into()];
            }
            CapabilityMembership {
                class_hash: class,
                tags: tags.into_iter().map(Into::into).collect(),
                hardware: (!i.is_multiple_of(3)).then(|| HardwareSummary {
                    gpu_vendor: Some(if i.is_multiple_of(2) { "nvidia" } else { "amd" }.into()),
                    gpu_count: 1,
                    memory_gb: Some(64),
                    vram_gb: Some(24),
                }),
                state: if i.is_multiple_of(2) {
                    NodeState::Idle
                } else {
                    NodeState::Busy
                },
                region: (!i.is_multiple_of(11)).then(|| format!("r{}", i % 5)),
                price_quote: None,
                reflex_addr: None,
                noise_pubkey: None,
                rtc_bootstrap: None,
                rtc_addr: None,
                rtc_stun_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
                owner: None,
            }
        };
        let build = |order: &mut dyn Iterator<Item = u64>| {
            let fold = new_fold();
            for i in order {
                let mut classes = vec![0x100];
                if i.is_multiple_of(7) {
                    classes.push(0x200);
                }
                for class in classes {
                    fold.apply(SignedAnnouncement::placeholder(
                        CapabilityFold::KIND_ID,
                        class,
                        i + 1,
                        1,
                        EnvelopeMeta::default(),
                        membership(class, i),
                    ))
                    .expect("fixture apply");
                }
            }
            fold
        };
        let forward = build(&mut (0..N));
        let reverse = build(&mut (0..N).rev());

        let tags_all: [&[&str]; 4] = [&[], &["a0"], &["a0", "b3"], &["a1", "rare"]];
        let groups: [&[&[&str]]; 5] = [
            &[],
            &[&["model:m0"]],
            &[&["model:m0", "model:m9"]],
            &[&["model:m1"], &["tool:x1"]],
            &[&["gpu:vendor:nvidia", "gpu:present"], &[]],
        ];
        let states = [None, Some(NodeState::Idle)];
        let regions = [None, Some("r2")];
        let tags_any: [&[&str]; 3] = [&[], &["b1"], &["b1", "cross"]];
        let classes = [None, Some(0x100), Some(0x200)];
        let to_vec = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let sorted = |keys: &HashSet<(u64, NodeId)>| {
            let mut v: Vec<_> = keys.iter().copied().collect();
            v.sort_unstable();
            v
        };

        let mut combos = 0usize;
        let mut nonempty = 0usize;
        for ta in tags_all {
            for g in groups {
                for st in states {
                    for rg in regions {
                        for tany in tags_any {
                            for cl in classes {
                                let filter = CapabilityFilter {
                                    tags_all: to_vec(ta),
                                    tag_groups_all: g.iter().map(|grp| to_vec(grp)).collect(),
                                    state: st,
                                    region: rg.map(String::from),
                                    tags_any: to_vec(tany),
                                    class: cl,
                                    ..CapabilityFilter::default()
                                };
                                let mut results = Vec::new();
                                for fold in [&forward, &reverse] {
                                    fold.with_state_and_index(|state, index| {
                                        let new = sorted(
                                            &resolve_candidate_keys(state, index, &filter)
                                                .key_set(),
                                        );
                                        let old = sorted(&resolve_candidate_keys_pre_slice4(
                                            state, index, &filter,
                                        ));
                                        assert_eq!(new, old, "resolvers differ for {filter:?}");
                                        let scan = sorted(&brute_force(state, &filter));
                                        assert_eq!(
                                            new, scan,
                                            "resolver differs from a payload scan for {filter:?}"
                                        );
                                        results.push(new);
                                    });
                                }
                                assert_eq!(
                                    results[0], results[1],
                                    "arrival order changed {filter:?}"
                                );
                                combos += 1;
                                nonempty += usize::from(!results[0].is_empty());
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(combos, 4 * 5 * 2 * 2 * 3 * 3);
        assert!(
            nonempty > combos / 4,
            "the matrix must exercise non-empty results"
        );
    }

    /// CAPABILITY_FOLD_SCALE_PLAN.md Slice 3: the borrowing synthetic
    /// derivation must match the `Tag::parse` path it replaced, over
    /// both separators, embedded delimiters, empty values, bundle-key
    /// shapes, reserved prefixes and raw tags that look like synthetic
    /// names. Grammar must not change in a performance slice.
    #[test]
    fn synthetic_derivation_matches_tag_parse() {
        use std::collections::BTreeMap;
        let tag_sets: Vec<Vec<&str>> = vec![
            vec!["software.model.0.id=llama3", "software.tool.0.tool_id=repl"],
            vec!["software.model.0.id:llama3", "software.tool.0.tool_id:repl"],
            vec!["software.model.0.id=a:b", "software.model.1.id:a=b"],
            vec!["software.model.0.id=", "software.model.1.id:"],
            vec!["software.model.0.name=llama", "software.model.0.id.extra=x"],
            vec!["software.model.id=no-index", "software.model.0.0.id=deep"],
            vec![
                "software.tool.0.id=not-a-tool-id",
                "software.tool.0.tool_id=ok",
            ],
            vec![
                "model:llama3",
                "tool:repl",
                "gpu:present",
                "gpu:vendor:nvidia",
            ],
            vec!["scope:region:eu", "causal:software.model.0.id=x"],
            vec!["hardware.gpu.vram_gb=80", "software.os=linux", "inference"],
            vec!["software.model.0.id=dup", "software.model.1.id=dup"],
            vec![],
        ];
        let hardware = [
            None,
            Some(HardwareSummary {
                gpu_vendor: Some("nvidia".into()),
                gpu_count: 1,
                memory_gb: Some(64),
                vram_gb: Some(24),
            }),
            Some(HardwareSummary {
                gpu_vendor: None,
                gpu_count: 2,
                memory_gb: None,
                vram_gb: None,
            }),
            Some(HardwareSummary {
                gpu_vendor: None,
                gpu_count: 0,
                memory_gb: Some(16),
                vram_gb: None,
            }),
        ];
        for tags in &tag_sets {
            for hw in &hardware {
                let payload = CapabilityMembership {
                    class_hash: 0,
                    tags: tags.iter().map(|t| t.to_string().into()).collect(),
                    hardware: hw.clone(),
                    state: NodeState::Idle,
                    region: None,
                    price_quote: None,
                    reflex_addr: None,
                    noise_pubkey: None,
                    rtc_bootstrap: None,
                    rtc_addr: None,
                    rtc_stun_addr: None,
                    allowed_nodes: Vec::new(),
                    allowed_subnets: Vec::new(),
                    allowed_groups: Vec::new(),
                    metadata: BTreeMap::new(),
                    owner: None,
                };
                assert_eq!(
                    derive_synthetic_index_tags(&payload),
                    derive_synthetic_index_tags_via_parse(&payload),
                    "derivations differ for tags {tags:?}, hardware {hw:?}"
                );
            }
        }
    }

    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::adapter::net::behavior::fold::{
        ApplyOutcome, EnvelopeMeta, Fold, FoldRegistry, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;

    fn sign_cap(
        keypair: &EntityKeypair,
        publisher: NodeId,
        generation: u64,
        class: u64,
        tags: Vec<&str>,
        state: NodeState,
        region: Option<&str>,
    ) -> SignedAnnouncement<CapabilityMembership> {
        sign_cap_with_reflex(
            keypair, publisher, generation, class, tags, state, region, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn sign_cap_with_reflex(
        keypair: &EntityKeypair,
        publisher: NodeId,
        generation: u64,
        class: u64,
        tags: Vec<&str>,
        state: NodeState,
        region: Option<&str>,
        reflex_addr: Option<std::net::SocketAddr>,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            keypair,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            generation,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: tags.into_iter().map(Into::into).collect(),
                hardware: None,
                state,
                region: region.map(String::from),
                price_quote: None,
                reflex_addr,
                noise_pubkey: None,
                rtc_bootstrap: None,
                rtc_addr: None,
                rtc_stun_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
                owner: None,
            },
        )
        .expect("sign succeeds")
    }

    fn new_fold() -> Fold<CapabilityFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn first_announcement_installs_and_populates_secondary_index() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let outcome = fold
            .apply(sign_cap(
                &kp,
                0xA,
                1,
                0x100,
                vec!["hardware.gpu", "vendor.nvidia"],
                NodeState::Idle,
                Some("us-east"),
            ))
            .expect("apply");
        assert_eq!(outcome, ApplyOutcome::Inserted);

        // by-class scan finds it
        let hits = fold.query(CapabilityQuery::InClass(0x100));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, (0x100, 0xA));

        // by-tag indexed lookup finds it
        let hits = fold.query(CapabilityQuery::HasAllTags(vec!["hardware.gpu".into()]));
        assert_eq!(hits.len(), 1);

        // by-state indexed lookup
        let hits = fold.query(CapabilityQuery::InState(NodeState::Idle));
        assert_eq!(hits.len(), 1);

        // by-region indexed lookup
        let hits = fold.query(CapabilityQuery::InRegion("us-east".into()));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn each_publisher_owns_its_own_class_entry_no_cross_override() {
        // Two distinct publishers in the same class. Each
        // writes its own key; neither can overwrite the
        // other.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();

        fold.apply(sign_cap(
            &kp_a,
            0xA,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            None,
        ))
        .expect("a");
        fold.apply(sign_cap(
            &kp_b,
            0xB,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Busy,
            None,
        ))
        .expect("b");

        let hits = fold.query(CapabilityQuery::InClass(0x100));
        assert_eq!(hits.len(), 2, "both publishers' entries coexist");

        // Idle filter sees only A; busy filter sees only B.
        let idle = fold.query(CapabilityQuery::InState(NodeState::Idle));
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].0, (0x100, 0xA));

        let busy = fold.query(CapabilityQuery::InState(NodeState::Busy));
        assert_eq!(busy.len(), 1);
        assert_eq!(busy[0].0, (0x100, 0xB));
    }

    #[test]
    fn replace_updates_secondary_index_drops_stale_tags() {
        // A publisher transitions Idle → Busy AND swaps tags
        // (gpu → tpu). The secondary index must reflect both
        // changes: querying by the old tag finds nothing,
        // querying by the new tag finds the entry.
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("us-east"),
        ))
        .expect("v1");

        fold.apply(sign_cap(
            &kp,
            0xA,
            2,
            0x100,
            vec!["tpu"],
            NodeState::Busy,
            Some("us-west"),
        ))
        .expect("v2");

        // Stale tag finds nothing.
        let stale = fold.query(CapabilityQuery::HasAllTags(vec!["gpu".into()]));
        assert!(stale.is_empty());
        // New tag finds it.
        let fresh = fold.query(CapabilityQuery::HasAllTags(vec!["tpu".into()]));
        assert_eq!(fresh.len(), 1);

        // Stale state bucket: empty.
        let stale_state = fold.query(CapabilityQuery::InState(NodeState::Idle));
        assert!(stale_state.is_empty());
        // New state bucket: 1 entry.
        let new_state = fold.query(CapabilityQuery::InState(NodeState::Busy));
        assert_eq!(new_state.len(), 1);

        // Stale region: empty. New region: 1.
        assert!(fold
            .query(CapabilityQuery::InRegion("us-east".into()))
            .is_empty());
        assert_eq!(
            fold.query(CapabilityQuery::InRegion("us-west".into()))
                .len(),
            1
        );
    }

    /// PERF_AUDIT §4.5 — when a refresh announcement carries the
    /// same (tags, region, state) as the existing entry, the
    /// secondary index must NOT be churned. The skip optimization
    /// must still let the entry's generation/TTL update, and
    /// queries must continue to return the entry — verifying that
    /// the index dance was unnecessary, not just absent.
    ///
    /// `index_payload_equivalent` itself is unit-tested below.
    #[test]
    fn replace_same_payload_keeps_index_consistent_and_query_returns_entry() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        // v1: gpu+h100 tags, Idle, us-east.
        fold.apply(sign_cap(
            &kp,
            0xCAFE,
            1,
            0x100,
            vec!["gpu", "h100"],
            NodeState::Idle,
            Some("us-east"),
        ))
        .expect("v1");

        // v2: identical payload, higher generation (steady-state
        // refresh).
        let outcome = fold
            .apply(sign_cap(
                &kp,
                0xCAFE,
                2,
                0x100,
                vec!["gpu", "h100"],
                NodeState::Idle,
                Some("us-east"),
            ))
            .expect("v2");
        assert_eq!(outcome, ApplyOutcome::Replaced);

        // The post-refresh query results must reflect the entry
        // through every indexed dimension.
        let by_tag = fold.query(CapabilityQuery::HasAllTags(vec!["gpu".into()]));
        assert_eq!(by_tag.len(), 1, "tag bucket must still resolve the entry");
        let by_state = fold.query(CapabilityQuery::InState(NodeState::Idle));
        assert_eq!(by_state.len(), 1, "state bucket must still resolve");
        let by_region = fold.query(CapabilityQuery::InRegion("us-east".into()));
        assert_eq!(by_region.len(), 1, "region bucket must still resolve");
    }

    /// PERF_AUDIT §4.5 — `index_payload_equivalent` is the gate
    /// between "skip the index dance" and "rebuild the buckets".
    /// Pin both sides: identical (tags, region, state) returns
    /// true; any differing dimension returns false.
    #[test]
    fn index_payload_equivalent_matches_indexed_dimensions() {
        use std::collections::BTreeMap;
        let base = CapabilityMembership {
            class_hash: 0x100,
            tags: vec!["gpu".into(), "h100".into()],
            hardware: None,
            state: NodeState::Idle,
            region: Some("us-east".into()),
            price_quote: None,
            reflex_addr: None,
            noise_pubkey: None,
            rtc_bootstrap: None,
            rtc_addr: None,
            rtc_stun_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
            owner: None,
        };
        assert!(
            <CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &base.clone()),
            "byte-identical payload is equivalent"
        );

        // Tags differ.
        let mut t = base.clone();
        t.tags.push("a100".into());
        assert!(
            !<CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &t),
            "tag delta must invalidate"
        );

        // State differs.
        let mut s = base.clone();
        s.state = NodeState::Busy;
        assert!(
            !<CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &s),
            "state delta must invalidate"
        );

        // Region differs.
        let mut r = base.clone();
        r.region = Some("us-west".into());
        assert!(
            !<CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &r),
            "region delta must invalidate"
        );

        // Hardware differs — the `gpu:present` / `gpu:vendor:<v>`
        // synthetic index tags derive from `hardware`, so a GPU
        // shape change MUST invalidate or `by_synthetic` goes
        // stale on a tags-identical refresh.
        let mut h = base.clone();
        h.hardware = Some(HardwareSummary {
            gpu_vendor: Some("nvidia".into()),
            gpu_count: 1,
            memory_gb: None,
            vram_gb: None,
        });
        assert!(
            !<CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &h),
            "hardware delta must invalidate — synthetic gpu tags derive from it"
        );

        // Non-indexed dimension (metadata) — these CAN differ
        // without forcing an index rebuild. The skip is correct
        // because the index doesn't key on metadata at all.
        let mut m = base.clone();
        m.metadata.insert("intent".into(), "ml-training".into());
        assert!(
            <CapabilityIndexInner as super::super::FoldIndex<CapabilityFold>>::index_payload_equivalent(&base, &m),
            "metadata delta is OK to skip — index doesn't key on metadata"
        );
    }

    /// PERF_AUDIT §4.5 regression — a refresh that keeps (tags,
    /// region, state) identical but CHANGES the hardware GPU
    /// shape must still rebuild the synthetic index. Pre-fix the
    /// equivalence check ignored `hardware`, so the gained GPU
    /// never landed in `by_synthetic` (a `gpu:present` group
    /// query kept missing the node) and a lost GPU lingered
    /// stale. Drives the full apply path end-to-end.
    #[test]
    fn replace_with_changed_hardware_updates_synthetic_index() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let sign_with_hw = |generation: u64, hardware: Option<HardwareSummary>| {
            SignedAnnouncement::sign(
                &kp,
                CapabilityFold::KIND_ID,
                0x100,
                0xFACE,
                generation,
                EnvelopeMeta::default(),
                CapabilityMembership {
                    class_hash: 0x100,
                    tags: vec!["worker".into()],
                    hardware,
                    state: NodeState::Idle,
                    region: Some("us-east".into()),
                    price_quote: None,
                    reflex_addr: None,
                    noise_pubkey: None,
                    rtc_bootstrap: None,
                    rtc_addr: None,
                    rtc_stun_addr: None,
                    allowed_nodes: Vec::new(),
                    allowed_subnets: Vec::new(),
                    allowed_groups: Vec::new(),
                    metadata: BTreeMap::new(),
                    owner: None,
                },
            )
            .expect("sign succeeds")
        };
        let gpu_present_filter = || CapabilityFilter {
            tag_groups_all: vec![vec!["gpu:present".into()]],
            ..CapabilityFilter::default()
        };

        // v1: no hardware → no gpu:present synthetic tag.
        fold.apply(sign_with_hw(1, None)).expect("v1");
        let hits = fold.query(CapabilityQuery::Composite(gpu_present_filter()));
        assert!(hits.is_empty(), "no GPU yet — synthetic axis must miss");

        // v2: same tags/region/state, GPU appears. The refresh
        // must rebuild by_synthetic.
        fold.apply(sign_with_hw(
            2,
            Some(HardwareSummary {
                gpu_vendor: Some("nvidia".into()),
                gpu_count: 1,
                memory_gb: Some(64),
                vram_gb: Some(24),
            }),
        ))
        .expect("v2");
        let hits = fold.query(CapabilityQuery::Composite(gpu_present_filter()));
        assert_eq!(
            hits.len(),
            1,
            "GPU gained on refresh must be visible via the synthetic index"
        );

        // v3: GPU disappears again — the stale gpu:present bucket
        // must be dropped.
        fold.apply(sign_with_hw(3, None)).expect("v3");
        let hits = fold.query(CapabilityQuery::Composite(gpu_present_filter()));
        assert!(
            hits.is_empty(),
            "GPU lost on refresh must drop the stale synthetic bucket"
        );
    }

    #[test]
    fn has_all_tags_finds_only_entries_carrying_every_tag() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp,
            0x1,
            1,
            0x100,
            vec!["a", "b", "c"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0x2,
            1,
            0x100,
            vec!["a", "b"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0x3,
            1,
            0x100,
            vec!["a"],
            NodeState::Idle,
            None,
        ))
        .unwrap();

        // Need a + b + c → only node 1
        let hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::HasAllTags(vec![
                "a".into(),
                "b".into(),
                "c".into(),
            ]))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, [0x1].into_iter().collect());

        // Need a + b → nodes 1 and 2
        let hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::HasAllTags(vec!["a".into(), "b".into()]))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, [0x1, 0x2].into_iter().collect());

        // Need just a → all three
        let hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::HasAllTags(vec!["a".into()]))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, [0x1, 0x2, 0x3].into_iter().collect());
    }

    #[test]
    fn has_any_tag_returns_union_across_buckets() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp,
            0x1,
            1,
            0x100,
            vec!["x"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0x2,
            1,
            0x100,
            vec!["y"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0x3,
            1,
            0x100,
            vec!["z"],
            NodeState::Idle,
            None,
        ))
        .unwrap();

        let hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::HasAnyTag(vec!["x".into(), "y".into()]))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, [0x1, 0x2].into_iter().collect());
    }

    #[test]
    fn composite_query_intersects_every_populated_filter_axis() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();

        // Three entries: A (gpu/idle/us-east), B (gpu/busy/us-east),
        // C (gpu/idle/us-west). Composite filter (class + gpu +
        // idle + us-east) → only A.
        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("us-east"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xB,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Busy,
            Some("us-east"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xC,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("us-west"),
        ))
        .unwrap();

        let filter = CapabilityFilter {
            class: Some(0x100),
            tags_all: vec!["gpu".into()],
            state: Some(NodeState::Idle),
            region: Some("us-east".into()),
            ..CapabilityFilter::default()
        };
        let hits: Vec<_> = fold
            .query(CapabilityQuery::Composite(filter))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, vec![0xA]);
    }

    #[test]
    fn composite_query_honours_limit() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        for i in 0..10 {
            fold.apply(sign_cap(
                &kp,
                i,
                1,
                0x100,
                vec!["gpu"],
                NodeState::Idle,
                None,
            ))
            .unwrap();
        }
        let filter = CapabilityFilter {
            class: Some(0x100),
            limit: 3,
            ..CapabilityFilter::default()
        };
        let hits = fold.query(CapabilityQuery::Composite(filter));
        assert_eq!(hits.len(), 3);
    }

    /// 2026-06-11 service-discovery follow-up — single-constraint
    /// filters must take the borrowed fast path (the index bucket
    /// IS the answer; no clone/rehash of M candidate keys),
    /// composite filters must materialize, and the borrowed arm
    /// must select exactly what the general path would.
    #[test]
    fn single_constraint_filters_borrow_the_index_bucket() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x100,
            vec!["gpu", "fast"],
            NodeState::Idle,
            Some("us-east"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xB,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Busy,
            Some("us-west"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xC,
            1,
            0x200,
            vec!["cpu"],
            NodeState::Idle,
            Some("us-east"),
        ))
        .unwrap();

        fold.with_state_and_index(|state, index| {
            let nodes = |keys: &CandidateKeys<'_>| -> Vec<NodeId> {
                let mut v: Vec<NodeId> = keys.keys().map(|(_, n)| n).collect();
                v.sort_unstable();
                v
            };

            // Single tag → Borrowed: exactly the by_tag bucket.
            let tag_only = CapabilityFilter {
                tags_all: vec!["gpu".into()],
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &tag_only);
            assert!(
                got.is_borrowed(),
                "single-tag filter must borrow the index bucket"
            );
            assert_eq!(nodes(&got), vec![0xA, 0xB]);

            // Single state → Borrowed.
            let state_only = CapabilityFilter {
                state: Some(NodeState::Idle),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &state_only);
            assert!(got.is_borrowed());
            assert_eq!(nodes(&got), vec![0xA, 0xC]);

            // Single region → Borrowed.
            let region_only = CapabilityFilter {
                region: Some("us-east".into()),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &region_only);
            assert!(got.is_borrowed());
            assert_eq!(nodes(&got), vec![0xA, 0xC]);

            // Unknown single tag → provably empty (Owned default,
            // no bucket to borrow).
            let missing = CapabilityFilter {
                tags_all: vec!["nope".into()],
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &missing);
            assert!(!got.is_borrowed());
            assert_eq!(got.len(), 0);

            // Composite (tag + state) → Owned: the general path
            // must still materialize and intersect.
            let composite = CapabilityFilter {
                tags_all: vec!["gpu".into()],
                state: Some(NodeState::Idle),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &composite);
            assert!(
                !got.is_borrowed(),
                "composite filter must materialize a tightened set"
            );
            assert_eq!(nodes(&got), vec![0xA]);
        });
    }

    #[test]
    fn composite_query_with_tags_any_filters_correctly() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x100,
            vec!["common", "fast"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xB,
            1,
            0x100,
            vec!["common", "slow"],
            NodeState::Idle,
            None,
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xC,
            1,
            0x100,
            vec!["common"],
            NodeState::Idle,
            None,
        ))
        .unwrap();

        // tags_all=[common] + tags_any=[fast, slow] → A and B,
        // not C (C carries `common` but neither `fast` nor
        // `slow`).
        let filter = CapabilityFilter {
            tags_all: vec!["common".into()],
            tags_any: vec!["fast".into(), "slow".into()],
            ..CapabilityFilter::default()
        };
        let hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::Composite(filter))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(hits, [0xA, 0xB].into_iter().collect());
    }

    #[test]
    fn evict_node_drops_every_class_entry_and_cleans_indexes() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // Publisher 0xA in two classes; publisher 0xB in one
        // class as a control.
        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xA,
            1,
            0x200,
            vec!["tpu"],
            NodeState::Busy,
            Some("r2"),
        ))
        .unwrap();
        fold.apply(sign_cap(
            &kp,
            0xB,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("r1"),
        ))
        .unwrap();
        assert_eq!(fold.stats().entries, 3);

        fold.evict_node(0xA, "test");
        assert_eq!(fold.stats().entries, 1);
        assert_eq!(fold.stats().evictions, 2);

        // Tag indexes for evicted A's tags must be cleared (or
        // narrowed): "gpu" survives because B still carries it;
        // "tpu" had only A and is now empty.
        let gpu_hits: std::collections::HashSet<_> = fold
            .query(CapabilityQuery::HasAllTags(vec!["gpu".into()]))
            .into_iter()
            .map(|((_, n), _)| n)
            .collect();
        assert_eq!(gpu_hits, [0xB].into_iter().collect());
        let tpu_hits = fold.query(CapabilityQuery::HasAllTags(vec!["tpu".into()]));
        assert!(tpu_hits.is_empty());
    }

    #[test]
    fn reflex_addr_for_returns_first_advertised_addr_across_publisher_classes() {
        use std::net::SocketAddr;
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let addr: SocketAddr = "203.0.113.4:7000".parse().unwrap();

        // Publisher 0xAA in two classes; only the second carries a
        // reflex_addr. The lookup walks by_node and returns the
        // first Some across the class entries.
        fold.apply(sign_cap_with_reflex(
            &kp,
            0xAA,
            1,
            0x100,
            vec![],
            NodeState::Idle,
            None,
            None,
        ))
        .expect("class 0x100");
        fold.apply(sign_cap_with_reflex(
            &kp,
            0xAA,
            1,
            0x101,
            vec![],
            NodeState::Idle,
            None,
            Some(addr),
        ))
        .expect("class 0x101");

        assert_eq!(super::reflex_addr_for(&fold, 0xAA), Some(addr));
        // Unknown node → None (not in by_node).
        assert_eq!(super::reflex_addr_for(&fold, 0xBB), None);
    }

    #[test]
    fn reflex_addr_for_returns_none_when_publisher_advertises_no_addr() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_cap(&kp, 0xAA, 1, 0x100, vec![], NodeState::Idle, None))
            .expect("class 0x100");
        assert_eq!(super::reflex_addr_for(&fold, 0xAA), None);
    }

    #[test]
    fn capability_tags_for_all_matches_per_node_walk() {
        // Pin that the batched helper returns the same per-publisher
        // tag set as the single-node helper, but in one lock
        // acquisition. The shape callers depend on: every
        // `by_node` publisher gets an entry; tag sets are unioned
        // across the publisher's class entries.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp_a,
            0xA,
            1,
            0x100,
            vec!["gpu", "vendor.nvidia"],
            NodeState::Idle,
            None,
        ))
        .expect("a-100");
        // Same publisher, different class — tags should union.
        fold.apply(sign_cap(
            &kp_a,
            0xA,
            1,
            0x200,
            vec!["gpu", "model:llama"],
            NodeState::Idle,
            None,
        ))
        .expect("a-200");
        fold.apply(sign_cap(
            &kp_b,
            0xB,
            1,
            0x100,
            vec!["cpu-only"],
            NodeState::Idle,
            None,
        ))
        .expect("b-100");

        let batched = super::capability_tags_for_all(&fold);
        assert_eq!(batched.len(), 2);

        let mut tags_a = batched.get(&0xA).cloned().unwrap_or_default();
        tags_a.sort();
        assert_eq!(
            tags_a,
            vec![
                "gpu".to_string(),
                "model:llama".to_string(),
                "vendor.nvidia".to_string()
            ],
            "publisher A unions tags across both class entries"
        );

        let mut tags_b = batched.get(&0xB).cloned().unwrap_or_default();
        tags_b.sort();
        assert_eq!(tags_b, vec!["cpu-only".to_string()]);

        // Each entry should equal the single-node helper's result
        // for that publisher.
        for (node_id, batched_tags) in &batched {
            let mut single = super::capability_tags_for(&fold, *node_id);
            single.sort();
            let mut batched_sorted = batched_tags.clone();
            batched_sorted.sort();
            assert_eq!(single, batched_sorted, "mismatch for node 0x{:x}", node_id);
        }
    }

    #[test]
    fn capability_tags_for_all_returns_empty_for_empty_fold() {
        let fold = new_fold();
        let batched = super::capability_tags_for_all(&fold);
        assert!(batched.is_empty());
    }

    #[test]
    fn nodes_with_capability_tag_filters_the_batch() {
        const RELAY_CAPABLE_TAG: &str =
            crate::adapter::net::behavior::capability::RELAY_CAPABLE_TAG;
        // A and C advertise `relay-capable`; B does not. C carries it
        // on a *second* class entry only, so the union walk must see
        // it. Querying a mix (incl. an absent node id D) returns
        // exactly the matching subset.
        let fold = new_fold();
        let kp_a = EntityKeypair::generate();
        let kp_b = EntityKeypair::generate();
        let kp_c = EntityKeypair::generate();
        fold.apply(sign_cap(
            &kp_a,
            0xA,
            1,
            0x100,
            vec!["gpu", RELAY_CAPABLE_TAG],
            NodeState::Idle,
            None,
        ))
        .expect("a");
        fold.apply(sign_cap(
            &kp_b,
            0xB,
            1,
            0x100,
            vec!["cpu-only"],
            NodeState::Idle,
            None,
        ))
        .expect("b");
        fold.apply(sign_cap(
            &kp_c,
            0xC,
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            None,
        ))
        .expect("c-100");
        fold.apply(sign_cap(
            &kp_c,
            0xC,
            1,
            0x200,
            vec![RELAY_CAPABLE_TAG],
            NodeState::Idle,
            None,
        ))
        .expect("c-200");

        let mut got: Vec<u64> =
            super::nodes_with_capability_tag(&fold, &[0xA, 0xB, 0xC, 0xD], RELAY_CAPABLE_TAG)
                .into_iter()
                .collect();
        got.sort();
        assert_eq!(
            got,
            vec![0xA, 0xC],
            "only A and C advertise the tag (D is absent)"
        );

        // Batch predicate agrees with the per-node union helper.
        for nid in [0xA, 0xB, 0xC] {
            let via_union = super::capability_tags_for(&fold, nid)
                .iter()
                .any(|t| t == RELAY_CAPABLE_TAG);
            let via_batch =
                super::nodes_with_capability_tag(&fold, &[nid], RELAY_CAPABLE_TAG).contains(&nid);
            assert_eq!(
                via_union, via_batch,
                "batch vs union disagree for 0x{nid:x}"
            );
        }

        // Empty query → empty result.
        assert!(super::nodes_with_capability_tag(&fold, &[], RELAY_CAPABLE_TAG).is_empty());
    }

    #[test]
    fn runtime_ttl_sweeps_stale_capability_entries() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let ann = SignedAnnouncement::sign(
            &kp,
            CapabilityFold::KIND_ID,
            0x100,
            0xA,
            1,
            EnvelopeMeta {
                ttl_secs: Some(0),
                ..Default::default()
            },
            CapabilityMembership {
                class_hash: 0x100,
                tags: vec!["gpu".into()],
                hardware: None,
                state: NodeState::Idle,
                region: None,
                price_quote: None,
                reflex_addr: None,
                noise_pubkey: None,
                rtc_bootstrap: None,
                rtc_addr: None,
                rtc_stun_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
                owner: None,
            },
        )
        .unwrap();
        fold.apply(ann).unwrap();
        assert_eq!(fold.stats().entries, 1);

        std::thread::sleep(Duration::from_millis(10));
        let n = fold.sweep_expired_now();
        assert_eq!(n, 1);
        assert_eq!(fold.stats().entries, 0);
        assert_eq!(fold.stats().expiries, 1);

        // Secondary index must also be cleared by sweep.
        assert!(fold
            .query(CapabilityQuery::HasAllTags(vec!["gpu".into()]))
            .is_empty());
    }

    #[test]
    fn capability_fold_plugs_into_registry_and_dispatches_signed_envelopes() {
        let registry = FoldRegistry::new();
        let fold: Arc<Fold<CapabilityFold>> = Arc::new(new_fold());
        registry.register(fold.clone());

        let kp = EntityKeypair::generate();
        // Dispatch verifies the publisher-binding, so an honest
        // envelope must carry the signer's own node_id.
        let ann = sign_cap(
            &kp,
            kp.entity_id().node_id(),
            1,
            0x100,
            vec!["gpu"],
            NodeState::Idle,
            Some("us-east"),
        );
        let bytes = ann.encode().expect("encode");
        let outcome = registry.dispatch(&bytes, kp.entity_id()).expect("dispatch");
        assert_eq!(outcome, ApplyOutcome::Inserted);
        assert_eq!(fold.stats().entries, 1);
    }

    // ---------------------------------------------------------------
    // Slice 7: bitmap buckets over entry slots.
    // ---------------------------------------------------------------

    /// The keys a filter selects, by scanning every entry's payload: no
    /// index, no buckets. Synthetic keys come from the same derivation the
    /// index uses. The independent oracle for the bitmap resolver.
    fn brute_force(
        state: &FoldState<CapabilityFold>,
        filter: &CapabilityFilter,
    ) -> HashSet<(u64, NodeId)> {
        let mut out = HashSet::new();
        let mut scratch = String::new();
        for (key, entry) in &state.entries {
            let p = &entry.payload;
            let has = |t: &str| p.tags.iter().any(|x| *x == t);
            if !filter.tags_all.iter().all(|t| has(t)) {
                continue;
            }
            let mut synthetic: Vec<String> = Vec::new();
            for_each_synthetic_index_tag(p, &mut scratch, |t| synthetic.push(t.to_owned()));
            if !filter
                .tag_groups_all
                .iter()
                .filter(|g| !g.is_empty())
                .all(|g| g.iter().any(|t| synthetic.contains(t)))
            {
                continue;
            }
            if filter.state.is_some_and(|st| p.state != st) {
                continue;
            }
            if filter
                .region
                .as_ref()
                .is_some_and(|r| p.region.as_ref() != Some(r))
            {
                continue;
            }
            if !filter.tags_any.is_empty() && !filter.tags_any.iter().any(|t| has(t)) {
                continue;
            }
            if filter.class.is_some_and(|c| key.0 != c) {
                continue;
            }
            out.insert(*key);
        }
        out
    }

    fn placeholder(
        class: u64,
        node: NodeId,
        generation: u64,
        ttl_secs: Option<u32>,
        tags: &[&str],
        state: NodeState,
        region: Option<&str>,
    ) -> SignedAnnouncement<CapabilityMembership> {
        let mut ann = sign_cap(
            &EntityKeypair::generate(),
            node,
            generation,
            class,
            tags.to_vec(),
            state,
            region,
        );
        ann.ttl_secs = ttl_secs;
        ann
    }

    fn filter_tags(tags: &[&str]) -> CapabilityFilter {
        CapabilityFilter {
            tags_all: tags.iter().map(|t| t.to_string()).collect(),
            ..CapabilityFilter::default()
        }
    }

    fn resolved(fold: &Fold<CapabilityFold>, filter: &CapabilityFilter) -> Vec<(u64, NodeId)> {
        fold.with_state_and_index(|state, index| {
            let mut keys: Vec<_> = resolve_candidate_keys(state, index, filter)
                .keys()
                .collect();
            keys.sort_unstable();
            keys
        })
    }

    /// Bitmap identity is the (class, node) ENTRY: class A of node N
    /// carries `alpha`, class B carries `beta`, and a query for both
    /// matches nothing, though the node carries both tags.
    #[test]
    fn cross_class_split_predicate_does_not_match() {
        let fold = new_fold();
        let n: NodeId = 0x77;
        fold.apply(placeholder(
            0xA,
            n,
            1,
            None,
            &["alpha"],
            NodeState::Idle,
            None,
        ))
        .expect("class A");
        fold.apply(placeholder(
            0xB,
            n,
            1,
            None,
            &["beta"],
            NodeState::Idle,
            None,
        ))
        .expect("class B");
        assert!(resolved(&fold, &filter_tags(&["alpha", "beta"])).is_empty());
        assert_eq!(resolved(&fold, &filter_tags(&["alpha"])), vec![(0xA, n)]);
        // The same split across the OR-ed `tags_any` and the AND-ed
        // `tags_all` still evaluates per entry.
        let mixed = CapabilityFilter {
            tags_all: vec!["alpha".into()],
            tags_any: vec!["beta".into()],
            ..CapabilityFilter::default()
        };
        assert!(resolved(&fold, &mixed).is_empty());
    }

    /// The same population applied in opposite orders, then churned and
    /// restored, yields identical node lists from the public API and
    /// identical resolver key sets, although slot (and so bitmap) order
    /// follows arrival.
    #[test]
    fn output_order_independent_of_arrival() {
        use crate::adapter::net::behavior::capability::CapabilityFilter as LegacyFilter;
        let build = |order: &mut dyn Iterator<Item = u64>| {
            let fold = new_fold();
            for i in order {
                let tags: Vec<String> = vec![format!("t{}", i % 5), "common".into()];
                let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                fold.apply(placeholder(
                    1,
                    0x1000 + i,
                    1,
                    None,
                    &refs,
                    NodeState::Idle,
                    None,
                ))
                .expect("apply");
            }
            // Churn: evict every third, re-add half of them under new ids.
            for i in (0..200u64).step_by(3) {
                fold.evict_node(0x1000 + i, "churn");
                if i % 2 == 0 {
                    fold.apply(placeholder(
                        1,
                        0x9000 + i,
                        1,
                        None,
                        &["common", "t9"],
                        NodeState::Busy,
                        None,
                    ))
                    .expect("re-add");
                }
            }
            let restored = new_fold();
            restored.restore(fold.snapshot(), false).expect("restore");
            restored
        };
        let forward = build(&mut (0..200));
        let reverse = build(&mut (0..200).rev());

        let legacy = LegacyFilter {
            require_tags: vec!["common".into()],
            ..LegacyFilter::default()
        };
        let a = super::super::capability_bridge::find_nodes_matching(&forward, &legacy);
        let b = super::super::capability_bridge::find_nodes_matching(&reverse, &legacy);
        assert!(!a.is_empty());
        assert_eq!(a, b, "public node list");
        assert!(a.windows(2).all(|w| w[0] < w[1]), "sorted and deduplicated");
        for filter in [
            filter_tags(&["t3"]),
            filter_tags(&["t9"]),
            filter_tags(&["common", "t1"]),
        ] {
            assert_eq!(
                resolved(&forward, &filter),
                resolved(&reverse, &filter),
                "{filter:?}"
            );
        }
    }

    /// A freed slot is reused for a different class and publisher and
    /// inherits none of the old entry's raw, synthetic, region or state
    /// memberships, whether the old entry left by eviction, expiry or a
    /// restore.
    #[test]
    fn reused_slot_inherits_no_membership() {
        let old_tags = ["old-tag", "software.model.0.id=oldm"];
        let check_clean = |fold: &Fold<CapabilityFold>, new_key: (u64, NodeId)| {
            assert!(resolved(fold, &filter_tags(&["old-tag"])).is_empty(), "raw");
            let synthetic = CapabilityFilter {
                tag_groups_all: vec![vec!["model:oldm".into()]],
                ..CapabilityFilter::default()
            };
            assert!(resolved(fold, &synthetic).is_empty(), "synthetic");
            let region = CapabilityFilter {
                region: Some("old-region".into()),
                ..CapabilityFilter::default()
            };
            assert!(resolved(fold, &region).is_empty(), "region");
            let busy = CapabilityFilter {
                state: Some(NodeState::Busy),
                ..CapabilityFilter::default()
            };
            assert!(resolved(fold, &busy).is_empty(), "state");
            assert_eq!(resolved(fold, &filter_tags(&["new-tag"])), vec![new_key]);
        };

        // Eviction.
        let fold = new_fold();
        fold.apply(placeholder(
            1,
            0xA,
            1,
            None,
            &old_tags,
            NodeState::Busy,
            Some("old-region"),
        ))
        .expect("old");
        let old_slot = fold.with_state_and_index(|_, i| i.slot_of(&(1, 0xA)));
        fold.evict_node(0xA, "test");
        fold.apply(placeholder(
            2,
            0xB,
            1,
            None,
            &["new-tag"],
            NodeState::Idle,
            None,
        ))
        .expect("new");
        assert_eq!(
            fold.with_state_and_index(|_, i| i.slot_of(&(2, 0xB))),
            old_slot,
            "the freed slot is reused"
        );
        check_clean(&fold, (2, 0xB));

        // Expiry.
        let fold = new_fold();
        fold.apply(placeholder(
            1,
            0xA,
            1,
            Some(0),
            &old_tags,
            NodeState::Busy,
            Some("old-region"),
        ))
        .expect("old");
        let old_slot = fold.with_state_and_index(|_, i| i.slot_of(&(1, 0xA)));
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(fold.sweep_expired_now(), 1);
        fold.apply(placeholder(
            2,
            0xB,
            1,
            None,
            &["new-tag"],
            NodeState::Idle,
            None,
        ))
        .expect("new");
        assert_eq!(
            fold.with_state_and_index(|_, i| i.slot_of(&(2, 0xB))),
            old_slot
        );
        check_clean(&fold, (2, 0xB));

        // Restore over a fold whose slots held other entries.
        let fold = new_fold();
        fold.apply(placeholder(
            1,
            0xA,
            1,
            None,
            &old_tags,
            NodeState::Busy,
            Some("old-region"),
        ))
        .expect("old");
        let source = new_fold();
        source
            .apply(placeholder(
                2,
                0xB,
                1,
                None,
                &["new-tag"],
                NodeState::Idle,
                None,
            ))
            .expect("new");
        fold.restore(source.snapshot(), true).expect("restore");
        check_clean(&fold, (2, 0xB));
    }

    /// The slot space is bounded by the PEAK live entry count: freed
    /// slots are reused before the table grows, and nothing compacts.
    #[test]
    fn slot_space_tracks_the_peak_and_reuses_before_growth() {
        let fold = new_fold();
        for i in 0..1000u64 {
            fold.apply(placeholder(1, i, 1, None, &["t"], NodeState::Idle, None))
                .expect("apply");
        }
        for i in 0..900u64 {
            fold.evict_node(i, "shrink");
        }
        let mem = fold.with_state_and_index(|_, i| i.memory_breakdown());
        assert_eq!(
            (mem.occupied_slots, mem.slot_capacity, mem.free_slots),
            (100, 1000, 900),
            "occupied, capacity and free reported distinctly"
        );
        for i in 5000..5050u64 {
            fold.apply(placeholder(1, i, 1, None, &["t"], NodeState::Idle, None))
                .expect("apply");
        }
        let mem = fold.with_state_and_index(|_, i| i.memory_breakdown());
        assert_eq!(
            (mem.occupied_slots, mem.slot_capacity, mem.free_slots),
            (150, 1000, 850),
            "reused, not grown"
        );
    }

    /// An exhausted slot space refuses a new entry whole (no wrap, nothing
    /// changed); a Replace keeps its slot and still succeeds; a freed slot
    /// makes room again.
    #[test]
    fn exhausted_slot_space_refuses_an_insert_and_never_wraps() {
        let fold = new_fold();
        {
            // Two slots only.
            let mut index = fold.index.write();
            index.slots_mut().set_limit(2);
        }
        fold.apply(placeholder(1, 0xA, 1, None, &["a"], NodeState::Idle, None))
            .expect("first");
        fold.apply(placeholder(1, 0xB, 1, None, &["b"], NodeState::Idle, None))
            .expect("second");
        match fold.apply(placeholder(1, 0xC, 1, None, &["c"], NodeState::Idle, None)) {
            Err(super::super::FoldError::PayloadRejected {
                reason: super::super::PayloadRejection::IndexFull { slots: 2 },
                ..
            }) => {}
            other => panic!("expected IndexFull, got {other:?}"),
        }
        fold.with_state_and_index(|state, index| {
            assert_eq!(state.len(), 2);
            assert!(
                index.dictionary().canonical("c").is_none(),
                "nothing admitted"
            );
        });
        assert_eq!(fold.stats().budget_rejections, 1);

        // A Replace reuses its own slot even at the limit.
        assert_eq!(
            fold.apply(placeholder(1, 0xA, 2, None, &["a2"], NodeState::Busy, None))
                .expect("replace at the limit"),
            ApplyOutcome::Replaced
        );
        fold.evict_node(0xB, "room");
        fold.apply(placeholder(1, 0xC, 1, None, &["c"], NodeState::Idle, None))
            .expect("room again");
        assert_eq!(resolved(&fold, &filter_tags(&["c"])), vec![(1, 0xC)]);
    }
}
