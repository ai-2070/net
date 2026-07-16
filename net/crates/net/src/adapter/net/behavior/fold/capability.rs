//! `CapabilityFold` — per-publisher capability membership.
//!
//! Each `(class_hash, publisher_node_id)` pair carries at most
//! one entry whose payload describes what the publisher claims
//! about its own membership in that capability class — tags,
//! hardware summary, current state, optional region + price
//! quote.
//!
//! Replaces the deleted `behavior::capability::CapabilityIndex` —
//! see `docs/plans/MULTIFOLD_PHASE_3B_CUTOVER.md` for the
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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::state::{FoldIndex, FoldState, NodeId};
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
    pub tags: Vec<String>,
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
    /// OA-1 ownership projection — the publisher's owner org,
    /// populated ONLY from an announcement `owner_cert` that passed
    /// ingest verification (signature, window, `member ==
    /// entity_id` binding, revocation floors). `None` for unowned
    /// publishers and for announcements whose cert failed
    /// verification (the cert is dropped; the entry is kept).
    ///
    /// Unlike `allowed_*` / tag-derived axes this is not
    /// self-declared — it is proven belonging. It is also NOT
    /// execution authority: `may_execute` never consults it
    /// (`ORG_CAPABILITY_AUTH_PLAN.md`, authority-dark OA-1).
    ///
    /// `#[serde(skip)]` is load-bearing twice over: (1) the fold
    /// payload rides `SUBPROTOCOL_FOLD` as positional postcard, so
    /// a serialized field would break every mixed-fleet fold frame
    /// at upgrade time; (2) a wire-carried `owner_org` would be a
    /// SELF-DECLARED ownership claim — the projection must only
    /// ever be derived on the receiving node from a cert it
    /// verified itself, and fold state (including snapshots) is
    /// never admission evidence. Decode always yields `None`.
    #[serde(skip)]
    pub owner_org: Option<super::super::org::OrgId>,
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
/// **PERF_AUDIT §4.6** — the inner `HashSet<(u64, NodeId)>` candidate
/// sets use `BuildU64TupleHasher` (private to this module), a fast
/// multiplicative mixer for
/// `(u64, u64)` keys. Pre-fix these used the std SipHash default,
/// which paid ~15-25 ns of mixing per insert/contains/remove on keys
/// that are already xxh3-hashed identity bytes (so collision
/// resistance is already there at construction; SipHash's DoS
/// resistance adds zero protection). The outer `HashMap<String, _>` /
/// `HashMap<NodeState, _>` keep the default hasher: tag / region
/// strings come from publishers and the SipHash protection is
/// legitimately relevant there.
#[derive(Debug, Default)]
pub struct CapabilityIndexInner {
    /// tag → set of (class, node) keys carrying that tag.
    by_tag: HashMap<String, HashSet<(u64, NodeId), BuildU64TupleHasher>>,
    /// Index-only synthetic tag (`model:`/`tool:`/`gpu:`) → set of
    /// (class, node) keys. Kept in a SEPARATE map from `by_tag` so a
    /// raw published tag string can never collide with a synthetic
    /// key: published tags are arbitrary strings (`Tag::Legacy`
    /// round-trips verbatim), so a publisher emitting a plain
    /// `"model:llama3"` tag must not be able to satisfy a
    /// `require_models` query it lacks the real bundle for. The bulk
    /// model/tool/gpu axes (`tag_groups_all`) resolve against this
    /// map only — see [`group_union`].
    by_synthetic: HashMap<String, HashSet<(u64, NodeId), BuildU64TupleHasher>>,
    /// region → set of (class, node) keys.
    by_region: HashMap<String, HashSet<(u64, NodeId), BuildU64TupleHasher>>,
    /// state → set of (class, node) keys.
    by_state: HashMap<NodeState, HashSet<(u64, NodeId), BuildU64TupleHasher>>,
}

/// Fast multiplicative `(u64, u64)` mixer for the inverted-index
/// candidate sets. Per PERF_AUDIT §4.6 — see [`CapabilityIndexInner`]
/// for the threat-model rationale (keys come from already-verified
/// announcements; SipHash DoS resistance is irrelevant).
///
/// `Hash for (u64, u64)` is `write_u64(self.0); write_u64(self.1);`,
/// so a hasher with a fast `write_u64` step and the byte-fallback for
/// completeness mixes the pair correctly in 2 multiplications.
#[derive(Default, Clone)]
pub(crate) struct U64TupleHasher(u64);

impl std::hash::Hasher for U64TupleHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write_u64(&mut self, v: u64) {
        // FxHash-style step: rotate, xor, multiply by a large odd
        // constant. ~1 ns; well-distributed for already-hashed input.
        const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(FX_SEED);
    }
    /// Defensive byte fallback — the std `Hash` impl for `(u64, u64)`
    /// calls `write_u64` directly, but a future change to the tuple's
    /// hash impl could route through `write(&[u8])`. Pack up to 8
    /// bytes per chunk and reuse `write_u64`.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(buf));
        }
    }
}

pub(crate) type BuildU64TupleHasher = std::hash::BuildHasherDefault<U64TupleHasher>;

impl FoldIndex<CapabilityFold> for CapabilityIndexInner {
    fn on_insert(&mut self, key: &(u64, NodeId), payload: &CapabilityMembership) {
        for tag in &payload.tags {
            self.by_tag.entry(tag.clone()).or_default().insert(*key);
        }
        // Index-only synthetic tags (model:/tool:/gpu:) live in
        // their own `by_synthetic` map so the model / tool / gpu
        // filter axes resolve without a per-query full scan and
        // without risking collision against a raw published tag of
        // the same string. Parsed once here at insert, never per
        // query.
        for tag in derive_synthetic_index_tags(payload) {
            self.by_synthetic.entry(tag).or_default().insert(*key);
        }
        if let Some(region) = &payload.region {
            self.by_region
                .entry(region.clone())
                .or_default()
                .insert(*key);
        }
        self.by_state.entry(payload.state).or_default().insert(*key);
    }

    fn on_remove(&mut self, key: &(u64, NodeId), payload: &CapabilityMembership) {
        for tag in &payload.tags {
            if let Some(set) = self.by_tag.get_mut(tag) {
                set.remove(key);
                if set.is_empty() {
                    self.by_tag.remove(tag);
                }
            }
        }
        // Mirror the synthetic tags added in `on_insert`. Derived
        // from the same payload, so the set is identical.
        for tag in derive_synthetic_index_tags(payload) {
            if let Some(set) = self.by_synthetic.get_mut(&tag) {
                set.remove(key);
                if set.is_empty() {
                    self.by_synthetic.remove(&tag);
                }
            }
        }
        if let Some(region) = &payload.region {
            if let Some(set) = self.by_region.get_mut(region) {
                set.remove(key);
                if set.is_empty() {
                    self.by_region.remove(region);
                }
            }
        }
        if let Some(set) = self.by_state.get_mut(&payload.state) {
            set.remove(key);
            if set.is_empty() {
                self.by_state.remove(&payload.state);
            }
        }
    }

    fn clear(&mut self) {
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
fn derive_synthetic_index_tags(payload: &CapabilityMembership) -> Vec<String> {
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
            CapabilityQuery::HasAllTags(tags) => resolve_keys_all_tags(index, &tags)
                .into_iter()
                .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
                .collect(),
            CapabilityQuery::HasAnyTag(tags) => {
                let mut seen: HashSet<(u64, NodeId)> = HashSet::new();
                for tag in &tags {
                    if let Some(keys) = index.by_tag.get(tag) {
                        seen.extend(keys.iter().copied());
                    }
                }
                seen.into_iter()
                    .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
                    .collect()
            }
            CapabilityQuery::InState(s) => index
                .by_state
                .get(&s)
                .into_iter()
                .flat_map(|set| set.iter().copied())
                .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
                .collect(),
            CapabilityQuery::InRegion(r) => index
                .by_region
                .get(&r)
                .into_iter()
                .flat_map(|set| set.iter().copied())
                .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
                .collect(),
            CapabilityQuery::Composite(filter) => composite_query(state, index, &filter),
        }
    }
}

/// Resolve the set of keys that carry EVERY tag in `tags`.
/// Uses the inverted-tag index: pick the smallest tag-bucket
/// as the candidate set, then retain only candidates present
/// in every subsequent bucket. Empty `tags` returns every key
/// (matches the `tags_all = []` "no constraint" convention).
fn resolve_keys_all_tags(
    index: &CapabilityIndexInner,
    tags: &[String],
) -> HashSet<(u64, NodeId), BuildU64TupleHasher> {
    if tags.is_empty() {
        // No tag constraint → every indexed key. Use the by_state
        // index as a proxy: every entry is indexed under exactly
        // one state, which gives the full key set without walking
        // by_tag.
        return index
            .by_state
            .values()
            .flat_map(|set| set.iter().copied())
            .collect();
    }
    // Pick the most-selective tag bucket as the candidate set.
    let mut tags_by_selectivity: Vec<&String> = tags.iter().collect();
    tags_by_selectivity.sort_by_key(|t| index.by_tag.get(*t).map(|s| s.len()).unwrap_or(0));

    let Some(first) = tags_by_selectivity.first() else {
        return HashSet::default();
    };
    let Some(initial) = index.by_tag.get(*first) else {
        // First tag has no entries → intersection is empty.
        return HashSet::default();
    };
    let mut candidates: HashSet<(u64, NodeId), BuildU64TupleHasher> =
        initial.iter().copied().collect();
    for tag in tags_by_selectivity.iter().skip(1) {
        let Some(bucket) = index.by_tag.get(*tag) else {
            return HashSet::default();
        };
        candidates.retain(|k| bucket.contains(k));
        if candidates.is_empty() {
            break;
        }
    }
    candidates
}

/// Borrow-or-own candidate set returned by
/// [`resolve_candidate_keys`]. A single-constraint filter resolves
/// to exactly one index bucket, and that bucket IS the answer —
/// returning it borrowed skips cloning every candidate key into a
/// fresh owned set (alloc + rehash of M keys; the dominant cost of
/// a high-cardinality single-tag discovery query). Composite
/// filters still materialize an owned, tightened set.
pub(crate) enum CandidateKeys<'a> {
    /// The filter constrained exactly one indexed dimension —
    /// the bucket is borrowed from the index untouched.
    Borrowed(&'a HashSet<(u64, NodeId), BuildU64TupleHasher>),
    /// Composite (or empty-result) filter — materialized set.
    Owned(HashSet<(u64, NodeId), BuildU64TupleHasher>),
}

impl CandidateKeys<'_> {
    /// The resolved key set, regardless of arm.
    pub(crate) fn as_set(&self) -> &HashSet<(u64, NodeId), BuildU64TupleHasher> {
        match self {
            Self::Borrowed(s) => s,
            Self::Owned(s) => s,
        }
    }
}

/// Resolve the set of `(class, node)` keys a
/// [`CapabilityFilter`] selects on its *indexed* axes — tags,
/// state, region, class. Chooses the most-selective indexed
/// dimension as the seed, then tightens with the rest in memory.
/// Single-constraint filters return the index bucket borrowed
/// (see [`CandidateKeys`]); composite filters materialize.
///
/// Does NOT clone any payload, and does NOT apply `filter.limit`
/// or non-indexed predicates (hardware / model / tool). Callers
/// that only need keys — or that post-filter against borrowed
/// payloads — use this directly via
/// [`Fold::with_state_and_index`]; [`composite_query`] layers the
/// payload materialization + limit on top for the
/// `Vec<CapabilityMatch>` query path.
pub(crate) fn resolve_candidate_keys<'a>(
    state: &FoldState<CapabilityFold>,
    index: &'a CapabilityIndexInner,
    filter: &CapabilityFilter,
) -> CandidateKeys<'a> {
    // Single-constraint fast path (2026-06-11 service-discovery
    // follow-up): when the filter constrains exactly one indexed
    // dimension and nothing else would tighten the seed, the index
    // bucket already IS the final candidate set. Borrow it instead
    // of cloning every key into an owned set — and, for the state /
    // region shapes, instead of also running the general path's
    // redundant self-retain against the very bucket it seeded from.
    // `tags_all` resolves against `by_tag` only (synthetic model /
    // tool / gpu axes ride `tag_groups_all` → `by_synthetic`), so
    // borrowing the raw-tag bucket cannot leak a synthetic match.
    if filter.tag_groups_all.is_empty() && filter.tags_any.is_empty() && filter.class.is_none() {
        match (&filter.tags_all[..], filter.state, &filter.region) {
            ([tag], None, None) => {
                return match index.by_tag.get(tag) {
                    Some(bucket) => CandidateKeys::Borrowed(bucket),
                    None => CandidateKeys::Owned(HashSet::default()),
                };
            }
            ([], Some(state_filter), None) => {
                return match index.by_state.get(&state_filter) {
                    Some(bucket) => CandidateKeys::Borrowed(bucket),
                    None => CandidateKeys::Owned(HashSet::default()),
                };
            }
            ([], None, Some(region)) => {
                return match index.by_region.get(region) {
                    Some(bucket) => CandidateKeys::Borrowed(bucket),
                    None => CandidateKeys::Owned(HashSet::default()),
                };
            }
            _ => {}
        }
    }

    // Each group's union (OR within a group) is needed both to seed
    // (when no `tags_all` is present) and to tighten further down.
    // `group_unions` holds the ones that still need to be applied as
    // retain filters; the seed branch may consume one of them.
    let mut group_unions: Vec<HashSet<(u64, NodeId), BuildU64TupleHasher>> = Vec::new();

    // Seed candidate set: prefer tags_all (typically most
    // selective), then the most-selective tag group, then state,
    // then region, then class scan as fallback.
    let mut candidates: HashSet<(u64, NodeId), BuildU64TupleHasher> = if !filter.tags_all.is_empty()
    {
        let seed = resolve_keys_all_tags(index, &filter.tags_all);
        // Only materialize the group unions if the seed left
        // something to filter — when `tags_all` selects nothing the
        // result is already empty, so building them is wasted work.
        if !seed.is_empty() {
            group_unions = build_group_unions(index, &filter.tag_groups_all);
        }
        seed
    } else {
        group_unions = build_group_unions(index, &filter.tag_groups_all);
        if !group_unions.is_empty() {
            // Seed from the smallest group union, removing it so the
            // retain pass below doesn't re-scan it.
            // Non-empty (checked above), so min_by_key yields Some;
            // the `unwrap_or(0)` is just a panic-free fallback.
            let smallest = group_unions
                .iter()
                .enumerate()
                .min_by_key(|(_, u)| u.len())
                .map(|(i, _)| i)
                .unwrap_or(0);
            group_unions.swap_remove(smallest)
        } else if let Some(state_filter) = filter.state {
            index
                .by_state
                .get(&state_filter)
                .cloned()
                .unwrap_or_default()
        } else if let Some(region) = &filter.region {
            index.by_region.get(region).cloned().unwrap_or_default()
        } else if let Some(class) = filter.class {
            state
                .entries
                .keys()
                .filter(|(c, _)| *c == class)
                .copied()
                .collect()
        } else {
            // No selective predicate → every key.
            state.entries.keys().copied().collect()
        }
    };

    // Tighten with remaining predicates.
    if let Some(class) = filter.class {
        candidates.retain(|(c, _)| *c == class);
    }
    if let Some(state_filter) = filter.state {
        if let Some(bucket) = index.by_state.get(&state_filter) {
            candidates.retain(|k| bucket.contains(k));
        } else {
            candidates.clear();
        }
    }
    if let Some(region) = &filter.region {
        if let Some(bucket) = index.by_region.get(region) {
            candidates.retain(|k| bucket.contains(k));
        } else {
            candidates.clear();
        }
    }
    if !filter.tags_any.is_empty() {
        // Keep only candidates that carry at least one of the
        // tags_any list. Build the union of those tag buckets
        // once, then `retain`. Same PERF_AUDIT §4.6 fast mixer as
        // the other `(u64, NodeId)` intermediates in this resolver.
        let mut tags_any_union: HashSet<(u64, NodeId), BuildU64TupleHasher> = HashSet::default();
        for tag in &filter.tags_any {
            if let Some(bucket) = index.by_tag.get(tag) {
                tags_any_union.extend(bucket.iter().copied());
            }
        }
        candidates.retain(|k| tags_any_union.contains(k));
    }

    // AND across groups, OR within each group: a candidate must
    // appear in every remaining group's union (the seed group, if
    // any, was already consumed above).
    for union in &group_unions {
        candidates.retain(|k| union.contains(k));
        if candidates.is_empty() {
            break;
        }
    }

    // No tags_all re-check needed: when `tags_all` is non-empty it
    // is always the seed (the first branch above), so `candidates`
    // already equals its intersection and every retain since has
    // only narrowed it.
    CandidateKeys::Owned(candidates)
}

/// Materialize the union for each non-empty group in
/// `tag_groups_all` (OR within a group). Empty groups carry no
/// constraint, so they're skipped rather than producing an empty
/// union that would wrongly clear every candidate.
fn build_group_unions(
    index: &CapabilityIndexInner,
    groups: &[Vec<String>],
) -> Vec<HashSet<(u64, NodeId), BuildU64TupleHasher>> {
    groups
        .iter()
        .filter(|g| !g.is_empty())
        .map(|g| group_union(index, g))
        .collect()
}

/// Union of the `(class, node)` keys carrying at least one tag in
/// `group` — the OR-within-a-group half of `tag_groups_all`.
///
/// Resolves against `by_synthetic`, NOT `by_tag`: every
/// `tag_groups_all` entry is an index-only synthetic key
/// (`model:`/`tool:`/`gpu:`) manufactured by
/// `derive_synthetic_index_tags`. Reading the synthetic map keeps a
/// raw published tag of the same string from satisfying a model /
/// tool / gpu axis it has no real bundle / hardware for.
fn group_union(
    index: &CapabilityIndexInner,
    group: &[String],
) -> HashSet<(u64, NodeId), BuildU64TupleHasher> {
    let mut union: HashSet<(u64, NodeId), BuildU64TupleHasher> = HashSet::default();
    for tag in group {
        if let Some(bucket) = index.by_synthetic.get(tag) {
            union.extend(bucket.iter().copied());
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
        .as_set()
        .iter()
        .filter_map(|&k| state.entries.get(&k).map(|e| (k, e.payload.clone())));
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
    let Some(keys) = state.by_node.get(&node_id) else {
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
    let Some(keys) = state.by_node.get(&node_id) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in keys {
        if let Some(entry) = state.entries.get(key) {
            for tag in &entry.payload.tags {
                seen.insert(tag.clone());
            }
        }
    }
    seen.into_iter().collect()
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
        let keys = state.by_node.get(&node_id)?;
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
                tags: tags.into_iter().map(String::from).collect(),
                hardware: None,
                state,
                region: region.map(String::from),
                price_quote: None,
                reflex_addr,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
                owner_org: None,
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
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
            owner_org: None,
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
                    allowed_nodes: Vec::new(),
                    allowed_subnets: Vec::new(),
                    allowed_groups: Vec::new(),
                    metadata: BTreeMap::new(),
                    owner_org: None,
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
                let mut v: Vec<NodeId> = keys.as_set().iter().map(|&(_, n)| n).collect();
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
                matches!(got, CandidateKeys::Borrowed(_)),
                "single-tag filter must borrow the index bucket"
            );
            assert_eq!(nodes(&got), vec![0xA, 0xB]);

            // Single state → Borrowed.
            let state_only = CapabilityFilter {
                state: Some(NodeState::Idle),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &state_only);
            assert!(matches!(got, CandidateKeys::Borrowed(_)));
            assert_eq!(nodes(&got), vec![0xA, 0xC]);

            // Single region → Borrowed.
            let region_only = CapabilityFilter {
                region: Some("us-east".into()),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &region_only);
            assert!(matches!(got, CandidateKeys::Borrowed(_)));
            assert_eq!(nodes(&got), vec![0xA, 0xC]);

            // Unknown single tag → provably empty (Owned default,
            // no bucket to borrow).
            let missing = CapabilityFilter {
                tags_all: vec!["nope".into()],
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &missing);
            assert!(matches!(got, CandidateKeys::Owned(_)));
            assert!(got.as_set().is_empty());

            // Composite (tag + state) → Owned: the general path
            // must still materialize and intersect.
            let composite = CapabilityFilter {
                tags_all: vec!["gpu".into()],
                state: Some(NodeState::Idle),
                ..CapabilityFilter::default()
            };
            let got = resolve_candidate_keys(state, index, &composite);
            assert!(
                matches!(got, CandidateKeys::Owned(_)),
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
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
                owner_org: None,
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
}
