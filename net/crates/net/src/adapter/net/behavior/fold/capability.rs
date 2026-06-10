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
#[derive(Debug, Default)]
pub struct CapabilityIndexInner {
    /// tag → set of (class, node) keys carrying that tag.
    by_tag: HashMap<String, HashSet<(u64, NodeId)>>,
    /// Index-only synthetic tag (`model:`/`tool:`/`gpu:`) → set of
    /// (class, node) keys. Kept in a SEPARATE map from `by_tag` so a
    /// raw published tag string can never collide with a synthetic
    /// key: published tags are arbitrary strings (`Tag::Legacy`
    /// round-trips verbatim), so a publisher emitting a plain
    /// `"model:llama3"` tag must not be able to satisfy a
    /// `require_models` query it lacks the real bundle for. The bulk
    /// model/tool/gpu axes (`tag_groups_all`) resolve against this
    /// map only — see [`group_union`].
    by_synthetic: HashMap<String, HashSet<(u64, NodeId)>>,
    /// region → set of (class, node) keys.
    by_region: HashMap<String, HashSet<(u64, NodeId)>>,
    /// state → set of (class, node) keys.
    by_state: HashMap<NodeState, HashSet<(u64, NodeId)>>,
}

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
fn resolve_keys_all_tags(index: &CapabilityIndexInner, tags: &[String]) -> HashSet<(u64, NodeId)> {
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
        return HashSet::new();
    };
    let Some(initial) = index.by_tag.get(*first) else {
        // First tag has no entries → intersection is empty.
        return HashSet::new();
    };
    let mut candidates: HashSet<(u64, NodeId)> = initial.iter().copied().collect();
    for tag in tags_by_selectivity.iter().skip(1) {
        let Some(bucket) = index.by_tag.get(*tag) else {
            return HashSet::new();
        };
        candidates.retain(|k| bucket.contains(k));
        if candidates.is_empty() {
            break;
        }
    }
    candidates
}

/// Resolve the set of `(class, node)` keys a
/// [`CapabilityFilter`] selects on its *indexed* axes — tags,
/// state, region, class. Chooses the most-selective indexed
/// dimension as the seed, then tightens with the rest in memory.
///
/// Does NOT clone any payload, and does NOT apply `filter.limit`
/// or non-indexed predicates (hardware / model / tool). Callers
/// that only need keys — or that post-filter against borrowed
/// payloads — use this directly via
/// [`Fold::with_state_and_index`]; [`composite_query`] layers the
/// payload materialization + limit on top for the
/// `Vec<CapabilityMatch>` query path.
pub(crate) fn resolve_candidate_keys(
    state: &FoldState<CapabilityFold>,
    index: &CapabilityIndexInner,
    filter: &CapabilityFilter,
) -> HashSet<(u64, NodeId)> {
    // Each group's union (OR within a group) is needed both to seed
    // (when no `tags_all` is present) and to tighten further down.
    // `group_unions` holds the ones that still need to be applied as
    // retain filters; the seed branch may consume one of them.
    let mut group_unions: Vec<HashSet<(u64, NodeId)>> = Vec::new();

    // Seed candidate set: prefer tags_all (typically most
    // selective), then the most-selective tag group, then state,
    // then region, then class scan as fallback.
    let mut candidates: HashSet<(u64, NodeId)> = if !filter.tags_all.is_empty() {
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
        // once, then `retain`.
        let mut tags_any_union: HashSet<(u64, NodeId)> = HashSet::new();
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
    candidates
}

/// Materialize the union for each non-empty group in
/// `tag_groups_all` (OR within a group). Empty groups carry no
/// constraint, so they're skipped rather than producing an empty
/// union that would wrongly clear every candidate.
fn build_group_unions(
    index: &CapabilityIndexInner,
    groups: &[Vec<String>],
) -> Vec<HashSet<(u64, NodeId)>> {
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
fn group_union(index: &CapabilityIndexInner, group: &[String]) -> HashSet<(u64, NodeId)> {
    let mut union: HashSet<(u64, NodeId)> = HashSet::new();
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
    // Materialize matches + apply limit.
    let mut matches: Vec<CapabilityMatch> = candidates
        .into_iter()
        .filter_map(|k| state.entries.get(&k).map(|e| (k, e.payload.clone())))
        .collect();
    if filter.limit > 0 && matches.len() > filter.limit {
        matches.truncate(filter.limit);
    }
    matches
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
