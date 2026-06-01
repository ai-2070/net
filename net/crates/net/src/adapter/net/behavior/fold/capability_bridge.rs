//! Bridge from the legacy capability-query shapes to the
//! fold-backed query path.
//!
//! Centralizes filter-shape translation, post-query predicate
//! filtering (range predicates the fold's secondary index
//! doesn't index natively), and scope-filter composition for
//! callers using [`Fold<CapabilityFold>`](super::Fold) as the
//! source of truth (the legacy `CapabilityIndex` was deleted in
//! Multifold Phase 3B; see `docs/plans/MULTIFOLD_PHASE_3B_CUTOVER.md`).
//!
//! ## What's here
//!
//! - [`translate_filter`] — legacy `CapabilityFilter` →
//!   [`super::CapabilityFilter`]. Handles the indexable axes
//!   (tags, models, tools).
//! - [`membership_passes_post_filter`] — for the predicates the
//!   fold's secondary index doesn't surface (memory_gb,
//!   vram_gb, GPU presence, GPU vendor).
//! - [`find_nodes_matching`] — combine the two: query the fold,
//!   post-filter the returned memberships, dedupe by node_id.
//! - `scope_from_membership_tags` (private) — derive a
//!   `CapabilityScope` from the string-tag form the fold's
//!   payload carries.
//! - [`find_nodes_matching_scoped`] — the fold-flavored
//!   replacement for the legacy `find_nodes_scoped`.

use super::super::capability::{
    matches_scope, CapabilityAnnouncement, CapabilityFilter as LegacyFilter, CapabilityScope,
    GpuVendor, ScopeFilter,
};
use super::capability::{
    resolve_candidate_keys, CapabilityFilter, CapabilityFold, CapabilityMembership, HardwareSummary,
};
use super::state::FoldError;
use super::{ApplyOutcome, EnvelopeMeta, Fold, FoldKind, NodeId, NodeState, SignedAnnouncement};

/// Translate the legacy
/// [`behavior::capability::CapabilityFilter`](super::super::capability::CapabilityFilter)
/// into the fold's composite filter shape. The `require_models`,
/// `require_tools`, `require_gpu`, and `gpu_vendor` axes become
/// `tag_groups_all` entries built from the index-only synthetic
/// tags (`model:<id>`, `tool:<id>`, `gpu:present`,
/// `gpu:vendor:<v>`) the fold derives at insert, so the fold's
/// secondary index resolves them with no per-candidate parse —
/// each axis is one group (OR within the axis, AND across axes).
///
/// The true range predicates (`min_memory_gb`, `min_vram_gb`) and
/// free-form fields (`require_modalities`, `min_context_length`)
/// are NOT carried here — the *bulk* path runs the ranges through
/// [`membership_passes_range_filter`] against each borrowed
/// candidate, and the single-target path runs the full
/// [`membership_passes_post_filter`]. The `require_modalities` and
/// `min_context_length` axes are silently dropped on the fold path
/// because the fold's [`CapabilityMembership`] payload doesn't
/// carry the model metadata needed to evaluate them; callers that
/// need them keep the legacy index until the fold's payload is
/// extended.
pub fn translate_filter(legacy: &LegacyFilter) -> CapabilityFilter {
    // Each non-tag axis becomes one `tag_groups_all` group of
    // index-only synthetic tags. `require_models` / `require_tools`
    // are "any of" (union → multi-element group); `require_gpu` /
    // `gpu_vendor` are single-element groups. The synthetic tags
    // are manufactured at insert by `derive_synthetic_index_tags`
    // from the canonical `software.model.<i>.id=` /
    // `software.tool.<i>.tool_id=` bundles and the hardware
    // projection — a publisher never emits them on the wire, so a
    // synthetic group key can't be matched by raw published tags.
    let mut tag_groups_all: Vec<Vec<String>> = Vec::new();
    if !legacy.require_models.is_empty() {
        tag_groups_all.push(
            legacy
                .require_models
                .iter()
                .map(|m| format!("model:{m}"))
                .collect(),
        );
    }
    if !legacy.require_tools.is_empty() {
        tag_groups_all.push(
            legacy
                .require_tools
                .iter()
                .map(|t| format!("tool:{t}"))
                .collect(),
        );
    }
    if legacy.require_gpu {
        tag_groups_all.push(vec!["gpu:present".to_string()]);
    }
    if let Some(vendor) = legacy.gpu_vendor {
        tag_groups_all.push(vec![format!("gpu:vendor:{}", gpu_vendor_canonical(vendor))]);
    }
    CapabilityFilter {
        class: None,
        tags_all: legacy.require_tags.clone(),
        tags_any: Vec::new(),
        tag_groups_all,
        state: None,
        region: None,
        limit: 0,
    }
}

/// `true` if `membership` satisfies the *range* predicates the
/// fold's secondary index can't resolve — `min_memory_gb` and
/// `min_vram_gb`. The bulk filter path uses this against borrowed
/// candidates after the index has already resolved the tag /
/// model / tool / gpu axes (the latter three via the synthetic-tag
/// groups `translate_filter` builds), so re-checking those here
/// would be redundant work.
pub fn membership_passes_range_filter(
    membership: &CapabilityMembership,
    legacy: &LegacyFilter,
) -> bool {
    if let Some(min_mem) = legacy.min_memory_gb {
        let mem = membership
            .hardware
            .as_ref()
            .and_then(|h| h.memory_gb)
            .unwrap_or(0);
        if mem < min_mem {
            return false;
        }
    }
    if let Some(min_vram) = legacy.min_vram_gb {
        let vram = membership
            .hardware
            .as_ref()
            .and_then(|h| h.vram_gb)
            .unwrap_or(0);
        if vram < min_vram {
            return false;
        }
    }
    true
}

/// `true` if `membership` satisfies every post-query predicate the
/// fold's tag intersection doesn't itself enforce: the range
/// predicates ([`membership_passes_range_filter`]) plus GPU
/// presence / vendor and model / tool membership.
///
/// This is the full, self-contained matcher used by the
/// single-target path ([`target_matches_filter`]), which walks one
/// publisher's entries directly and does NOT consult the secondary
/// index. The bulk path resolves the gpu / model / tool axes
/// through the index's synthetic-tag groups instead and only needs
/// [`membership_passes_range_filter`];
/// `target_matches_filter_agrees_with_find_nodes_matching` pins
/// that the two stay equivalent.
pub fn membership_passes_post_filter(
    membership: &CapabilityMembership,
    legacy: &LegacyFilter,
) -> bool {
    if !membership_passes_range_filter(membership, legacy) {
        return false;
    }
    if legacy.require_gpu {
        let has_gpu = match &membership.hardware {
            Some(h) => h.gpu_count > 0 || h.gpu_vendor.is_some(),
            None => false,
        };
        if !has_gpu {
            return false;
        }
    }
    if let Some(want_vendor) = legacy.gpu_vendor {
        let got = membership
            .hardware
            .as_ref()
            .and_then(|h| h.gpu_vendor.as_deref())
            .unwrap_or("");
        if !gpu_vendor_matches(got, want_vendor) {
            return false;
        }
    }
    // Models + tools are encoded as multi-tag bundles
    // (`software.model.<i>.id=<name>`, `software.tool.<i>.tool_id=<name>`)
    // on the wire. Reuse the legacy `CapabilitySet::has_model` /
    // `has_tool` scan against a synthesized set so the same
    // canonical-tag predicate runs here as in the legacy matcher.
    // `require_models` / `require_tools` are "any must match"
    // (union semantics), per the legacy `CapabilityFilter::matches`
    // impl.
    if !legacy.require_models.is_empty() || !legacy.require_tools.is_empty() {
        let mut caps = super::super::capability::CapabilitySet::new();
        for s in &membership.tags {
            if let Ok(tag) = super::super::tag::Tag::parse(s) {
                caps.tags.insert(tag);
            }
        }
        if !legacy.require_models.is_empty()
            && !legacy.require_models.iter().any(|m| caps.has_model(m))
        {
            return false;
        }
        if !legacy.require_tools.is_empty()
            && !legacy.require_tools.iter().any(|t| caps.has_tool(t))
        {
            return false;
        }
    }
    true
}

fn gpu_vendor_matches(canonical: &str, want: GpuVendor) -> bool {
    matches!(
        (canonical, want),
        ("nvidia", GpuVendor::Nvidia)
            | ("amd", GpuVendor::Amd)
            | ("intel", GpuVendor::Intel)
            | ("apple", GpuVendor::Apple)
            | ("qualcomm", GpuVendor::Qualcomm)
            | ("unknown", GpuVendor::Unknown)
    )
}

fn gpu_vendor_canonical(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        GpuVendor::Apple => "apple",
        GpuVendor::Qualcomm => "qualcomm",
        GpuVendor::Unknown => "unknown",
    }
}

/// Apply a legacy [`CapabilityAnnouncement`] to the fold via
/// [`translate_announcement`]. Test fixtures use this to prime
/// a `Fold<CapabilityFold>` with the same legacy-shape
/// announcement the production dispatch path would produce.
///
/// Returns the [`ApplyOutcome`] from the underlying `fold.apply`
/// call so callers can distinguish `Inserted` / `Replaced` from
/// `IgnoredOlder` / `IgnoredEqual`, and so a failing apply (invalid
/// generation, signature mismatch — anything `FoldError` grows into)
/// surfaces instead of being silently dropped. Test fixtures
/// typically `.expect("apply")`; production callers that
/// intentionally want a best-effort apply can `let _ = ` the result.
pub fn apply_legacy_announcement(
    fold: &Fold<CapabilityFold>,
    ann: CapabilityAnnouncement,
) -> Result<ApplyOutcome, FoldError> {
    let fold_ann = translate_announcement(&ann);
    fold.apply(fold_ann)
}

/// Synthesize a legacy [`CapabilitySet`](super::super::capability::CapabilitySet)
/// for `node_id` from every fold entry the publisher owns
/// (walked via the `by_node` reverse index). Tags are merged
/// into the set's `HashSet<Tag>`; the metadata BTreeMaps from
/// each entry's [`CapabilityMembership`] are merged into the
/// set's `metadata` field, with later entries overwriting
/// earlier ones on key collision.
///
/// Returns an empty `CapabilitySet` when the publisher has no
/// fold entries — matches the legacy `.unwrap_or_default()`
/// fallback for subscribe-before-announce / cap-propagation
/// races.
///
/// Routes the per-tag parse through [`super::super::tag::Tag::parse`]
/// (not `parse_user`) so reserved-prefix tags (`causal:`,
/// `heat:`, `fork-of:`, `scope:`) round-trip cleanly into the
/// `Tag::Reserved` variant. `parse_user` rejects reserved
/// prefixes by design; the fold-side synthesis is operating on
/// values the substrate already accepted, so we want the
/// permissive parse.
pub fn synthesize_capability_set(
    fold: &Fold<CapabilityFold>,
    node_id: NodeId,
) -> super::super::capability::CapabilitySet {
    let mut caps = super::super::capability::CapabilitySet::new();
    fold.with_state(|state| {
        let Some(keys) = state.by_node.get(&node_id) else {
            return;
        };
        for k in keys {
            let Some(entry) = state.entries.get(k) else {
                continue;
            };
            for s in &entry.payload.tags {
                if let Ok(tag) = super::super::tag::Tag::parse(s) {
                    caps.tags.insert(tag);
                }
            }
            for (mk, mv) in &entry.payload.metadata {
                caps.metadata.insert(mk.clone(), mv.clone());
            }
        }
    });
    caps
}

/// `true` if `caller_node` is authorized to invoke `target_node`
/// for `capability_tag`. Mirrors the legacy
/// `CapabilityIndex::may_execute` semantics against the fold's
/// state:
///
/// - `target` must have an entry carrying `capability_tag`.
/// - If `target`'s allow-lists are all empty, the call is
///   permitted (permissive default).
/// - Otherwise the caller is admitted iff at least one populated
///   axis (node / subnet / group) matches. Caller's subnet and
///   groups are read from the caller's own fold entries via
///   reserved `subnet:` / `group:` membership tags.
pub fn may_execute(
    fold: &Fold<CapabilityFold>,
    target_node: NodeId,
    capability_tag: &str,
    caller_node: NodeId,
) -> bool {
    fold.with_state(|state| {
        let Some(keys) = state.by_node.get(&target_node) else {
            return false;
        };
        let mut target_carries_tag = false;
        let mut allowed_nodes: Vec<u64> = Vec::new();
        let mut allowed_subnets: Vec<super::super::subnet::SubnetId> = Vec::new();
        let mut allowed_groups: Vec<super::super::group::GroupId> = Vec::new();
        for k in keys {
            let Some(entry) = state.entries.get(k) else {
                continue;
            };
            if entry.payload.tags.iter().any(|t| t == capability_tag) {
                target_carries_tag = true;
            }
            allowed_nodes.extend(entry.payload.allowed_nodes.iter().copied());
            allowed_subnets.extend(entry.payload.allowed_subnets.iter().copied());
            allowed_groups.extend(entry.payload.allowed_groups.iter().cloned());
        }
        if !target_carries_tag {
            return false;
        }
        if allowed_nodes.is_empty() && allowed_subnets.is_empty() && allowed_groups.is_empty() {
            return true;
        }
        if allowed_nodes.contains(&caller_node) {
            return true;
        }
        if !allowed_subnets.is_empty() || !allowed_groups.is_empty() {
            let Some(caller_keys) = state.by_node.get(&caller_node) else {
                return false;
            };
            let mut caller_subnet: Option<super::super::subnet::SubnetId> = None;
            let mut caller_groups: Vec<super::super::group::GroupId> = Vec::new();
            for k in caller_keys {
                let Some(entry) = state.entries.get(k) else {
                    continue;
                };
                for raw in &entry.payload.tags {
                    if let Some(subnet) = super::super::subnet::SubnetId::from_tag(raw) {
                        caller_subnet = Some(subnet);
                    }
                    if let Some(group) = super::super::group::GroupId::from_tag(raw) {
                        caller_groups.push(group);
                    }
                }
            }
            if let Some(subnet) = caller_subnet {
                if allowed_subnets.contains(&subnet) {
                    return true;
                }
            }
            for g in &caller_groups {
                if allowed_groups.contains(g) {
                    return true;
                }
            }
        }
        false
    })
}

/// Translate a legacy [`CapabilityAnnouncement`] into a
/// fold-shaped [`SignedAnnouncement<CapabilityMembership>`]
/// suitable for [`Fold::apply`] dual-population during the
/// Phase 3b cutover. The fold-side envelope is stamped with
/// the [`super::wire::placeholder_signature`] sentinel — apply
/// trusts its input (the dispatch layer is the one that
/// verifies), so the placeholder is fine here. The legacy
/// announcement's own signature has already been verified by
/// the cap-ann dispatch handler upstream.
///
/// `class_hash = 0` is a cutover sentinel: legacy announcements
/// don't carry the fold's per-class sharding model, so every
/// translated entry shares the same `(class=0, node_id)` key.
/// Queries that don't constrain on class — which is every
/// caller in this codebase per the prior survey — work
/// transparently against this layout.
pub fn translate_announcement(
    ann: &CapabilityAnnouncement,
) -> SignedAnnouncement<CapabilityMembership> {
    let views = ann.capabilities.views();
    let hw_view = views.hardware();
    let primary_gpu = hw_view.gpu.as_ref();
    let gpu_count =
        (primary_gpu.is_some() as u8).saturating_add(hw_view.additional_gpus.len() as u8);
    let gpu_vendor = primary_gpu.map(|g| gpu_vendor_canonical(g.vendor).to_string());
    let vram_gb = {
        let mut total: u32 = 0;
        if let Some(g) = primary_gpu {
            total = total.saturating_add(g.vram_gb);
        }
        for g in &hw_view.additional_gpus {
            total = total.saturating_add(g.vram_gb);
        }
        (gpu_count > 0).then_some(total)
    };
    let memory_gb = (hw_view.memory_gb > 0).then_some(hw_view.memory_gb);
    let hardware = if primary_gpu.is_some() || memory_gb.is_some() {
        Some(HardwareSummary {
            gpu_vendor,
            gpu_count,
            memory_gb,
            vram_gb,
        })
    } else {
        None
    };

    let tags: Vec<String> = ann
        .capabilities
        .tags
        .iter()
        .map(|t| t.to_string())
        .collect();
    let region = tags
        .iter()
        .find_map(|t| t.strip_prefix("scope:region:").map(String::from));

    SignedAnnouncement::placeholder(
        CapabilityFold::KIND_ID,
        0,
        ann.node_id,
        ann.version.max(1),
        EnvelopeMeta {
            announced_at: ann.timestamp_ns / 1_000,
            ttl_secs: Some(ann.ttl_secs),
            flags: 0,
        },
        CapabilityMembership {
            class_hash: 0,
            tags,
            hardware,
            state: NodeState::Idle,
            region,
            price_quote: None,
            reflex_addr: ann.reflex_addr,
            allowed_nodes: ann.allowed_nodes.clone(),
            allowed_subnets: ann.allowed_subnets.clone(),
            allowed_groups: ann.allowed_groups.clone(),
            metadata: ann.capabilities.metadata.clone(),
        },
    )
}

/// Run a legacy-filter query against the fold and return the
/// matching node ids. Handles the two-stage shape: fold's
/// secondary index for the indexable axes, then in-memory
/// post-filter for the range predicates. Dedupes across
/// per-`(class, node)` entries that may match (a publisher in
/// multiple classes counts once).
pub fn find_nodes_matching(fold: &Fold<CapabilityFold>, legacy: &LegacyFilter) -> Vec<NodeId> {
    let fold_filter = translate_filter(legacy);
    // Resolve the indexed-axis candidate keys and run the
    // non-indexed post-filter against *borrowed* payloads, all
    // under one read-lock acquisition. The bulk path only needs
    // node ids out, so we never clone a `CapabilityMembership` —
    // unlike the `Vec<CapabilityMatch>` query path, which clones
    // every match before the caller can discard it.
    let mut out: Vec<NodeId> = fold.with_state_and_index(|state, index| {
        let candidates = resolve_candidate_keys(state, index, &fold_filter);
        let mut ids: Vec<NodeId> = Vec::with_capacity(candidates.len());
        for key in candidates {
            let Some(entry) = state.entries.get(&key) else {
                continue;
            };
            // The index already resolved the tag / model / tool /
            // gpu axes; only the range predicates remain.
            if membership_passes_range_filter(&entry.payload, legacy) {
                ids.push(key.1);
            }
        }
        ids
    });
    // Sort + dedup: callers (e.g. the scheduler's `FirstMatch`
    // placement) need a deterministic order across processes, and a
    // publisher present in multiple classes must count once. Sorting
    // the Vec and deduping is cheaper than routing through a
    // `HashSet<NodeId>` first.
    out.sort_unstable();
    out.dedup();
    out
}

/// `true` if `node_id` has any fold entry that satisfies `legacy`.
/// Single-target variant of [`find_nodes_matching`] — avoids the
/// full composite query when callers (the placement layer's
/// per-target scorer) only need a yes/no for one specific node.
///
/// Walks the publisher's class entries via the `by_node` reverse
/// index (O(num classes the publisher owns), typically 0-3) and
/// runs the same `tags_all` intersection + post-filter the bulk
/// path applies. Returns `false` for unknown publishers, matching
/// the bulk path's "missing publishers don't appear" contract.
pub fn target_matches_filter(
    fold: &Fold<CapabilityFold>,
    node_id: NodeId,
    legacy: &LegacyFilter,
) -> bool {
    fold.with_state(|state| {
        let Some(keys) = state.by_node.get(&node_id) else {
            return false;
        };
        for key in keys {
            let Some(entry) = state.entries.get(key) else {
                continue;
            };
            let membership = &entry.payload;
            // Same tag-intersection rule the fold's secondary
            // index applies (`tags_all` ⊆ membership.tags), then
            // the range / vendor / model / tool post-filter.
            let tags_ok = legacy
                .require_tags
                .iter()
                .all(|t| membership.tags.iter().any(|m| m == t));
            if !tags_ok {
                continue;
            }
            if !membership_passes_post_filter(membership, legacy) {
                continue;
            }
            return true;
        }
        false
    })
}

/// Derive a [`CapabilityScope`] from a [`CapabilityMembership`]'s
/// string-tag set. Reads the canonical string form the fold's
/// payload carries — `"scope:global"`, `"scope:subnet-local"`,
/// `"scope:tenant:<id>"`, `"scope:region:<name>"`.
///
/// `pub(crate)` because [`CapabilityScope`] is itself
/// `pub(crate)`; downstream callers reach scope filtering
/// through [`find_nodes_matching_scoped`].
pub(crate) fn scope_from_membership_tags(tags: &[String]) -> CapabilityScope {
    let mut tenants: Vec<String> = Vec::new();
    let mut regions: Vec<String> = Vec::new();
    let mut subnet_local = false;
    for tag in tags {
        // Reserved-tag prefix lives at "scope:"; everything after
        // is the body.
        let Some(body) = tag.strip_prefix("scope:") else {
            continue;
        };
        if body == "subnet-local" {
            subnet_local = true;
        } else if let Some(id) = body.strip_prefix("tenant:") {
            if !id.is_empty() {
                tenants.push(id.to_string());
            }
        } else if let Some(name) = body.strip_prefix("region:") {
            if !name.is_empty() {
                regions.push(name.to_string());
            }
        }
        // "scope:global" is the default; presence is a no-op.
    }
    if subnet_local {
        CapabilityScope::SubnetLocal
    } else {
        match (tenants.is_empty(), regions.is_empty()) {
            (true, true) => CapabilityScope::Global,
            (false, true) => CapabilityScope::Tenants(tenants),
            (true, false) => CapabilityScope::Regions(regions),
            (false, false) => CapabilityScope::TenantsAndRegions { tenants, regions },
        }
    }
}

/// Run a [`Predicate`](super::super::predicate::Predicate)
/// against every publisher in the fold and return the matching
/// `(node_id, synthesized_caps)` pairs. Walks the fold's
/// `by_node` reverse index and builds the `EvalContext` from a
/// synthesized
/// [`CapabilitySet`](super::super::capability::CapabilitySet).
///
/// Tag-based predicates work fully — reserved-prefix tags
/// round-trip through `Tag::parse` inside
/// [`synthesize_capability_set`]. Metadata-based predicates
/// see the merged BTreeMap from every entry the publisher owns,
/// per [`synthesize_capability_set`]'s last-write-wins merge
/// on key collision.
pub fn filter_by_predicate(
    fold: &Fold<CapabilityFold>,
    predicate: &super::super::predicate::Predicate,
) -> Vec<(NodeId, super::super::capability::CapabilitySet)> {
    let publishers: Vec<NodeId> = fold.with_state(|state| state.by_node.keys().copied().collect());
    let mut out = Vec::new();
    for node_id in publishers {
        let caps = synthesize_capability_set(fold, node_id);
        let owned_tags: Vec<super::super::tag::Tag> = caps.tags.iter().cloned().collect();
        let ctx = super::super::predicate::EvalContext::new(&owned_tags, &caps.metadata);
        if predicate.evaluate_unplanned(&ctx) {
            out.push((node_id, caps));
        }
    }
    out
}

/// Scoped variant of [`find_nodes_matching`]. Filters
/// candidates through `scope` (resolved from each
/// publisher's `scope:*` tags) on top of the capability
/// filter. `same_subnet_lookup(node_id) -> bool` is supplied
/// by the caller; the bridge has no native subnet state.
///
/// **Warm-up semantics** match the legacy path: when the
/// caller's subnet membership is unknown for a candidate, the
/// caller's closure decides whether to admit (typically
/// `true` once a subnet policy is installed, `false`
/// otherwise).
pub fn find_nodes_matching_scoped(
    fold: &Fold<CapabilityFold>,
    legacy: &LegacyFilter,
    scope: &ScopeFilter<'_>,
    same_subnet_lookup: impl Fn(NodeId) -> bool,
) -> Vec<NodeId> {
    let fold_filter = translate_filter(legacy);
    // Borrow-and-filter, same as `find_nodes_matching`: resolve
    // candidate keys via the index, then run the post-filter and
    // scope gate against borrowed payloads without cloning.
    let mut out: Vec<NodeId> = fold.with_state_and_index(|state, index| {
        let candidates = resolve_candidate_keys(state, index, &fold_filter);
        let mut ids: Vec<NodeId> = Vec::with_capacity(candidates.len());
        for key in candidates {
            let Some(entry) = state.entries.get(&key) else {
                continue;
            };
            let membership = &entry.payload;
            // The index already resolved the tag / model / tool /
            // gpu axes; only the range predicates remain.
            if !membership_passes_range_filter(membership, legacy) {
                continue;
            }
            let candidate_scope = scope_from_membership_tags(&membership.tags);
            let same_subnet = same_subnet_lookup(key.1);
            if !matches_scope(&candidate_scope, scope, same_subnet) {
                continue;
            }
            ids.push(key.1);
        }
        ids
    });
    // Sort + dedup: deterministic order and one entry per publisher
    // (a publisher may match under multiple classes).
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::{
        EnvelopeMeta, FoldKind, NodeState, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;
    use std::collections::HashSet;
    use std::time::Duration;

    fn sign_member(
        kp: &EntityKeypair,
        node_id: NodeId,
        class: u64,
        tags: Vec<&str>,
        hardware: Option<super::super::capability::HardwareSummary>,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            super::super::capability::CapabilityFold::KIND_ID,
            class,
            node_id,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: tags.into_iter().map(String::from).collect(),
                hardware,
                state: NodeState::Idle,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    fn new_fold() -> Fold<CapabilityFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn translate_filter_passes_require_tags_through_and_groups_models_tools_gpu() {
        let legacy = LegacyFilter {
            require_tags: vec!["gpu".into()],
            require_models: vec!["llama3".into(), "mistral".into()],
            require_tools: vec!["ffmpeg".into()],
            require_gpu: true,
            gpu_vendor: Some(GpuVendor::Nvidia),
            ..LegacyFilter::default()
        };
        let fold_filter = translate_filter(&legacy);
        // `require_tags` go directly through to `tags_all` (AND).
        assert_eq!(fold_filter.tags_all, vec!["gpu".to_string()]);
        // Models / tools / gpu / vendor are encoded as the
        // index-only synthetic-tag groups the fold derives at
        // insert — one group per axis (OR within, AND across).
        // `require_models` is "any of", so both models land in a
        // single group.
        assert_eq!(
            fold_filter.tag_groups_all,
            vec![
                vec!["model:llama3".to_string(), "model:mistral".to_string()],
                vec!["tool:ffmpeg".to_string()],
                vec!["gpu:present".to_string()],
                vec!["gpu:vendor:nvidia".to_string()],
            ]
        );
    }

    #[test]
    fn synthetic_index_tags_are_queryable_but_never_leak_into_enumeration() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let hw = HardwareSummary {
            gpu_vendor: Some("nvidia".into()),
            gpu_count: 1,
            memory_gb: Some(64),
            vram_gb: Some(24),
        };
        fold.apply(sign_member(
            &kp,
            0xAA,
            0x100,
            vec![
                "gpu",
                "software.model.0.id=llama3",
                "software.tool.0.tool_id=ffmpeg",
            ],
            Some(hw),
        ))
        .expect("apply AA");

        // Tag enumeration returns the real published tags...
        let tags = super::super::capability::capability_tags_for(&fold, 0xAA);
        assert!(tags.contains(&"gpu".to_string()));
        assert!(tags.contains(&"software.model.0.id=llama3".to_string()));
        // ...but never the index-only synthetic tags.
        assert!(
            !tags
                .iter()
                .any(|t| t.starts_with("model:") || t.starts_with("tool:") || t.starts_with("gpu:")),
            "synthetic index tags leaked into enumeration: {tags:?}"
        );

        // Yet the synthetic tags ARE resolvable through the index.
        let by_model = find_nodes_matching(
            &fold,
            &LegacyFilter {
                require_models: vec!["llama3".into()],
                ..LegacyFilter::default()
            },
        );
        assert_eq!(by_model, vec![0xAA]);
        let by_tool_and_gpu = find_nodes_matching(
            &fold,
            &LegacyFilter {
                require_tools: vec!["ffmpeg".into()],
                require_gpu: true,
                gpu_vendor: Some(GpuVendor::Nvidia),
                ..LegacyFilter::default()
            },
        );
        assert_eq!(by_tool_and_gpu, vec![0xAA]);
    }

    #[test]
    fn membership_passes_post_filter_matches_models_via_canonical_tag_bundle() {
        let legacy = LegacyFilter {
            require_models: vec!["llama3".into()],
            ..LegacyFilter::default()
        };
        let pass = CapabilityMembership {
            class_hash: 0x100,
            tags: vec!["software.model.0.id=llama3".into()],
            hardware: None,
            state: NodeState::Idle,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        };
        assert!(membership_passes_post_filter(&pass, &legacy));

        let fail = CapabilityMembership {
            tags: vec!["software.model.0.id=mistral".into()],
            ..pass.clone()
        };
        assert!(!membership_passes_post_filter(&fail, &legacy));

        // No models advertised at all → reject.
        let bare = CapabilityMembership {
            tags: vec![],
            ..pass
        };
        assert!(!membership_passes_post_filter(&bare, &legacy));
    }

    #[test]
    fn membership_passes_post_filter_enforces_min_memory_and_gpu() {
        let legacy = LegacyFilter {
            min_memory_gb: Some(64),
            require_gpu: true,
            ..LegacyFilter::default()
        };

        let ok = CapabilityMembership {
            class_hash: 0x100,
            tags: vec![],
            hardware: Some(super::super::capability::HardwareSummary {
                gpu_vendor: Some("nvidia".into()),
                gpu_count: 2,
                memory_gb: Some(128),
                vram_gb: Some(80),
            }),
            state: NodeState::Idle,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        };
        assert!(membership_passes_post_filter(&ok, &legacy));

        // Same shape but only 32 GB memory — rejected.
        let low_mem = CapabilityMembership {
            hardware: Some(super::super::capability::HardwareSummary {
                gpu_vendor: Some("nvidia".into()),
                gpu_count: 2,
                memory_gb: Some(32),
                vram_gb: Some(80),
            }),
            ..ok.clone()
        };
        assert!(!membership_passes_post_filter(&low_mem, &legacy));

        // No hardware reported — require_gpu fails closed.
        let no_hw = CapabilityMembership {
            hardware: None,
            ..ok
        };
        assert!(!membership_passes_post_filter(&no_hw, &legacy));
    }

    #[test]
    fn find_nodes_matching_dedupes_publisher_across_classes() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // Same publisher 0xAA in two classes, both carrying "gpu".
        fold.apply(sign_member(&kp, 0xAA, 0x100, vec!["gpu"], None))
            .expect("apply 0x100");
        fold.apply(sign_member(&kp, 0xAA, 0x101, vec!["gpu"], None))
            .expect("apply 0x101");

        let mut legacy = LegacyFilter::default();
        legacy.require_tags.push("gpu".into());

        let nodes = find_nodes_matching(&fold, &legacy);
        assert_eq!(nodes, vec![0xAA]);
    }

    #[test]
    fn target_matches_filter_agrees_with_find_nodes_matching() {
        // Two publishers, mixed tag sets — both filter inputs must
        // get the same yes/no verdict from the per-target check and
        // the bulk find. Pins parity so the placement layer's O(1)
        // fast path can't silently drift from the bulk query.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let nvidia_hw = HardwareSummary {
            gpu_vendor: Some("nvidia".into()),
            gpu_count: 2,
            memory_gb: Some(128),
            vram_gb: Some(80),
        };
        // AA: gpu tags + a model/tool bundle + nvidia hardware.
        fold.apply(sign_member(
            &kp,
            0xAA,
            0x100,
            vec![
                "gpu",
                "cuda",
                "software.model.0.id=llama3",
                "software.tool.0.tool_id=ffmpeg",
            ],
            Some(nvidia_hw),
        ))
        .expect("apply AA");
        // BB: cpu-only, no hardware, no bundles.
        fold.apply(sign_member(&kp, 0xBB, 0x100, vec!["cpu-only"], None))
            .expect("apply BB");

        let probe = |legacy: &LegacyFilter, candidates: &[NodeId]| {
            let bulk: HashSet<NodeId> = find_nodes_matching(&fold, legacy).into_iter().collect();
            for &n in candidates {
                assert_eq!(
                    bulk.contains(&n),
                    target_matches_filter(&fold, n, legacy),
                    "node 0x{:x} verdict mismatch for filter {:?}",
                    n,
                    legacy
                );
            }
        };

        // Permissive filter: both publishers pass either path.
        probe(&LegacyFilter::default(), &[0xAA, 0xBB, 0xCC]);

        // `require_tags = ["gpu"]`: only AA passes.
        let mut f = LegacyFilter::default();
        f.require_tags.push("gpu".into());
        probe(&f, &[0xAA, 0xBB, 0xCC]);

        // The index-resolved axes must agree between paths too:
        // a present model, a missing model, a present tool, GPU
        // presence, and a matching/mismatching vendor.
        let model_hit = LegacyFilter {
            require_models: vec!["llama3".into()],
            ..LegacyFilter::default()
        };
        probe(&model_hit, &[0xAA, 0xBB]);
        let model_miss = LegacyFilter {
            require_models: vec!["does-not-exist".into()],
            ..LegacyFilter::default()
        };
        probe(&model_miss, &[0xAA, 0xBB]);
        let tool_hit = LegacyFilter {
            require_tools: vec!["ffmpeg".into()],
            ..LegacyFilter::default()
        };
        probe(&tool_hit, &[0xAA, 0xBB]);
        let gpu = LegacyFilter {
            require_gpu: true,
            ..LegacyFilter::default()
        };
        probe(&gpu, &[0xAA, 0xBB]);
        let vendor_hit = LegacyFilter {
            gpu_vendor: Some(GpuVendor::Nvidia),
            ..LegacyFilter::default()
        };
        probe(&vendor_hit, &[0xAA, 0xBB]);
        let vendor_miss = LegacyFilter {
            gpu_vendor: Some(GpuVendor::Amd),
            ..LegacyFilter::default()
        };
        probe(&vendor_miss, &[0xAA, 0xBB]);

        // Unknown publisher: per-target check returns false (matches
        // bulk path's "missing publishers don't appear").
        assert!(!target_matches_filter(
            &fold,
            0xDEAD,
            &LegacyFilter::default()
        ));
    }

    #[test]
    fn target_matches_filter_applies_post_filter_predicates() {
        // Min-memory predicate is in the post-filter slice; pin
        // that the per-target check honors it, not just the
        // indexable tag intersection.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let hw = HardwareSummary {
            gpu_vendor: None,
            gpu_count: 0,
            memory_gb: Some(32),
            vram_gb: None,
        };
        fold.apply(sign_member(&kp, 0xAA, 0x100, vec!["gpu"], Some(hw)))
            .expect("apply AA");

        let mut tight = LegacyFilter::default();
        tight.require_tags.push("gpu".into());
        tight.min_memory_gb = Some(64);
        assert!(!target_matches_filter(&fold, 0xAA, &tight));

        let mut loose = LegacyFilter::default();
        loose.require_tags.push("gpu".into());
        loose.min_memory_gb = Some(16);
        assert!(target_matches_filter(&fold, 0xAA, &loose));
    }

    #[test]
    fn scope_from_membership_tags_parses_canonical_strings() {
        let global = scope_from_membership_tags(&["gpu".into(), "scope:global".into()]);
        assert!(matches!(global, CapabilityScope::Global));

        let subnet_local = scope_from_membership_tags(&["scope:subnet-local".into(), "gpu".into()]);
        assert!(matches!(subnet_local, CapabilityScope::SubnetLocal));

        let tenant = scope_from_membership_tags(&["scope:tenant:acme".into()]);
        match tenant {
            CapabilityScope::Tenants(ts) => assert_eq!(ts, vec!["acme".to_string()]),
            other => panic!("expected Tenants, got {other:?}"),
        }

        let region = scope_from_membership_tags(&["scope:region:us-east".into()]);
        match region {
            CapabilityScope::Regions(rs) => assert_eq!(rs, vec!["us-east".to_string()]),
            other => panic!("expected Regions, got {other:?}"),
        }
    }

    #[test]
    fn translate_announcement_projects_legacy_hardware_into_summary() {
        use crate::adapter::net::behavior::capability::{
            CapabilityAnnouncement, CapabilitySet, GpuInfo, GpuVendor as LegacyGpuVendor,
            HardwareCapabilities,
        };
        use crate::adapter::net::identity::EntityId;

        let caps = CapabilitySet::new().with_hardware(
            HardwareCapabilities::new()
                .with_memory(128)
                .with_gpu(GpuInfo {
                    vendor: LegacyGpuVendor::Nvidia,
                    model: "h100".into(),
                    vram_gb: 80,
                    compute_units: 0,
                    tensor_cores: 0,
                    fp16_tflops_x10: 0,
                }),
        );
        let ann = CapabilityAnnouncement::new(0xAA, EntityId::from_bytes([0u8; 32]), 7, caps);

        let translated = translate_announcement(&ann);
        assert_eq!(translated.node_id, 0xAA);
        assert_eq!(translated.generation, 7);
        let hw = translated.payload.hardware.expect("hardware summary set");
        assert_eq!(hw.memory_gb, Some(128));
        assert_eq!(hw.gpu_count, 1);
        assert_eq!(hw.gpu_vendor.as_deref(), Some("nvidia"));
        assert_eq!(hw.vram_gb, Some(80));
    }

    #[test]
    fn translate_announcement_promotes_version_zero_to_generation_one() {
        // The fold rejects generation == 0 (wire sentinel). The
        // legacy CapabilityAnnouncement::new defaults version to
        // whatever the caller passes; if a legacy caller used 0
        // we must promote to 1 so the fold accepts the apply.
        use crate::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
        use crate::adapter::net::identity::EntityId;

        let ann = CapabilityAnnouncement::new(
            0xAA,
            EntityId::from_bytes([0u8; 32]),
            0,
            CapabilitySet::new(),
        );
        let translated = translate_announcement(&ann);
        assert_eq!(translated.generation, 1);
    }

    #[test]
    fn find_nodes_matching_scoped_excludes_subnet_local_non_same_subnet() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // Two publishers; one is scope:subnet-local. SameSubnet
        // filter admits only candidates the lookup says are
        // co-resident.
        fold.apply(sign_member(
            &kp,
            0xAA,
            0x100,
            vec!["gpu", "scope:subnet-local"],
            None,
        ))
        .expect("apply AA subnet-local");
        fold.apply(sign_member(&kp, 0xBB, 0x100, vec!["gpu"], None))
            .expect("apply BB global");

        let mut legacy = LegacyFilter::default();
        legacy.require_tags.push("gpu".into());

        // SameSubnet lookup says BB is co-resident, AA isn't.
        let lookup = |nid: NodeId| nid == 0xBB;
        let mut nodes =
            find_nodes_matching_scoped(&fold, &legacy, &ScopeFilter::SameSubnet, lookup);
        nodes.sort();
        assert_eq!(nodes, vec![0xBB]);
    }
}
