//! Bridge from the legacy capability-query shapes to the
//! fold-backed query path.
//!
//! Centralizes filter-shape translation, post-query predicate
//! filtering (range predicates the fold's secondary index
//! doesn't index natively), and scope-filter composition for
//! callers migrating from
//! [`behavior::capability::CapabilityIndex`](super::super::capability::CapabilityIndex)
//! to [`Fold<CapabilityFold>`](super::Fold).
//!
//! ## What's here
//!
//! - [`translate_filter`] — legacy
//!   [`behavior::capability::CapabilityFilter`](super::super::capability::CapabilityFilter)
//!   → [`super::CapabilityFilter`]. Handles the indexable axes
//!   (tags, models, tools).
//! - [`membership_passes_post_filter`] — for the predicates the
//!   fold's secondary index doesn't surface (memory_gb,
//!   vram_gb, GPU presence, GPU vendor).
//! - [`find_nodes_matching`] — combine the two: query the fold,
//!   post-filter the returned memberships, dedupe by node_id.
//! - [`scope_from_membership_tags`] — derive a
//!   [`CapabilityScope`](super::super::capability::CapabilityScope)
//!   from the string-tag form the fold's payload carries.
//! - [`find_nodes_matching_scoped`] — the fold-flavored
//!   replacement for
//!   [`CapabilityIndex::find_nodes_scoped`](super::super::capability::CapabilityIndex::find_nodes_scoped).

use std::collections::HashSet;

use super::super::capability::{
    matches_scope, CapabilityAnnouncement, CapabilityFilter as LegacyFilter, CapabilityScope,
    GpuVendor, ScopeFilter,
};
use super::capability::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, HardwareSummary,
};
use super::{EnvelopeMeta, Fold, FoldKind, NodeId, NodeState, SignedAnnouncement};

/// Translate the legacy
/// [`behavior::capability::CapabilityFilter`](super::super::capability::CapabilityFilter)
/// into the fold's composite filter shape. Encodes models /
/// tools as canonical `"model:<name>"` / `"tool:<name>"` tags so
/// the fold's tag-based secondary index can resolve them via
/// the same intersection that handles plain tag predicates.
///
/// Range predicates (`min_memory_gb`, `min_vram_gb`,
/// `require_gpu`, `gpu_vendor`) and free-form fields
/// (`require_modalities`, `min_context_length`) are NOT carried
/// here — callers run them through [`membership_passes_post_filter`]
/// against each candidate the fold returns. The
/// `require_modalities` and `min_context_length` axes are
/// silently dropped on the fold path because the fold's
/// [`CapabilityMembership`] payload doesn't carry the model
/// metadata needed to evaluate them; callers that need them
/// keep the legacy index until the fold's payload is extended.
pub fn translate_filter(legacy: &LegacyFilter) -> CapabilityFilter {
    let mut tags_all: Vec<String> = legacy.require_tags.clone();
    for model in &legacy.require_models {
        tags_all.push(format!("model:{}", model));
    }
    for tool in &legacy.require_tools {
        tags_all.push(format!("tool:{}", tool));
    }
    CapabilityFilter {
        class: None,
        tags_all,
        tags_any: Vec::new(),
        state: None,
        region: None,
        limit: 0,
    }
}

/// `true` if `membership` satisfies the post-query predicates
/// the fold's index doesn't natively support. Caller is
/// expected to have already filtered candidates through the
/// fold's secondary index via [`translate_filter`].
pub fn membership_passes_post_filter(
    membership: &CapabilityMembership,
    legacy: &LegacyFilter,
) -> bool {
    if legacy.require_gpu {
        let has_gpu = match &membership.hardware {
            Some(h) => h.gpu_count > 0 || h.gpu_vendor.is_some(),
            None => false,
        };
        if !has_gpu {
            return false;
        }
    }
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
    true
}

fn gpu_vendor_matches(canonical: &str, want: GpuVendor) -> bool {
    matches!(
        (canonical, want),
        ("nvidia", GpuVendor::Nvidia)
            | ("amd", GpuVendor::Amd)
            | ("intel", GpuVendor::Intel)
    )
}

fn gpu_vendor_canonical(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        _ => "other",
    }
}

/// Apply `ann` to both the legacy [`CapabilityIndex`] (via
/// `index.index(ann)`) and the fold (via
/// `fold.apply(translate_announcement(&ann))`). Matches the
/// dual-population invariant the production cap-ann dispatch
/// handler maintains; intended primarily for test fixtures
/// that need both indices populated identically.
pub fn dual_apply(
    fold: &Fold<CapabilityFold>,
    index: &super::super::capability::CapabilityIndex,
    ann: CapabilityAnnouncement,
) {
    let fold_ann = translate_announcement(&ann);
    let _ = fold.apply(fold_ann);
    index.index(ann);
}

/// Synthesize a legacy [`CapabilitySet`](super::super::capability::CapabilitySet)
/// for `node_id` from the fold's tag set. Carries tags only;
/// the fold's [`CapabilityMembership`] doesn't model the legacy
/// `metadata` BTreeMap, so consumers that read metadata from
/// the returned set will see it empty. Callers that need
/// metadata access should keep the legacy `CapabilityIndex::get`
/// path until the fold payload is extended.
///
/// Returns an empty `CapabilitySet` when the publisher has no
/// fold entries — matches the legacy `.unwrap_or_default()`
/// fallback for subscribe-before-announce / cap-propagation
/// races.
pub fn synthesize_capability_set(
    fold: &Fold<CapabilityFold>,
    node_id: NodeId,
) -> super::super::capability::CapabilitySet {
    let tags = super::capability::capability_tags_for(fold, node_id);
    let mut caps = super::super::capability::CapabilitySet::new();
    for tag in tags {
        caps = caps.add_tag(tag);
    }
    caps
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
pub(crate) fn translate_announcement(
    ann: &CapabilityAnnouncement,
) -> SignedAnnouncement<CapabilityMembership> {
    let views = ann.capabilities.views();
    let hw_view = views.hardware();
    let primary_gpu = hw_view.gpu.as_ref();
    let gpu_count = (primary_gpu.is_some() as u8)
        .saturating_add(hw_view.additional_gpus.len() as u8);
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

    let tags: Vec<String> = ann.capabilities.tags.iter().map(|t| t.to_string()).collect();
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
        },
    )
}

/// Run a legacy-filter query against the fold and return the
/// matching node ids. Handles the two-stage shape: fold's
/// secondary index for the indexable axes, then in-memory
/// post-filter for the range predicates. Dedupes across
/// per-`(class, node)` entries that may match (a publisher in
/// multiple classes counts once).
pub fn find_nodes_matching(
    fold: &Fold<CapabilityFold>,
    legacy: &LegacyFilter,
) -> Vec<NodeId> {
    let fold_filter = translate_filter(legacy);
    let matches = fold.query(CapabilityQuery::Composite(fold_filter));
    let mut node_set: HashSet<NodeId> = HashSet::new();
    for ((_class, node_id), membership) in matches {
        if membership_passes_post_filter(&membership, legacy) {
            node_set.insert(node_id);
        }
    }
    node_set.into_iter().collect()
}

/// Derive a [`CapabilityScope`] from a [`CapabilityMembership`]'s
/// string-tag set. Mirrors the legacy `scope_from_tags` but
/// reads the canonical string form the fold's payload carries —
/// `"scope:global"`, `"scope:subnet-local"`,
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
    let matches = fold.query(CapabilityQuery::Composite(fold_filter));
    let mut node_set: HashSet<NodeId> = HashSet::new();
    for ((_class, node_id), membership) in matches {
        if !membership_passes_post_filter(&membership, legacy) {
            continue;
        }
        let candidate_scope = scope_from_membership_tags(&membership.tags);
        let same_subnet = same_subnet_lookup(node_id);
        if !matches_scope(&candidate_scope, scope, same_subnet) {
            continue;
        }
        node_set.insert(node_id);
    }
    node_set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::fold::{
        EnvelopeMeta, FoldKind, NodeState, SignedAnnouncement,
    };
    use crate::adapter::net::identity::EntityKeypair;
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
            },
        )
        .expect("sign")
    }

    fn new_fold() -> Fold<CapabilityFold> {
        Fold::with_sweep_interval(Duration::ZERO)
    }

    #[test]
    fn translate_filter_encodes_models_and_tools_as_tags() {
        let mut legacy = LegacyFilter::default();
        legacy.require_tags.push("gpu".into());
        legacy.require_models.push("llama3".into());
        legacy.require_tools.push("ffmpeg".into());
        let fold_filter = translate_filter(&legacy);
        assert!(fold_filter.tags_all.contains(&"gpu".to_string()));
        assert!(fold_filter.tags_all.contains(&"model:llama3".to_string()));
        assert!(fold_filter.tags_all.contains(&"tool:ffmpeg".to_string()));
    }

    #[test]
    fn membership_passes_post_filter_enforces_min_memory_and_gpu() {
        let mut legacy = LegacyFilter::default();
        legacy.min_memory_gb = Some(64);
        legacy.require_gpu = true;

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
    fn scope_from_membership_tags_parses_canonical_strings() {
        let global = scope_from_membership_tags(&["gpu".into(), "scope:global".into()]);
        assert!(matches!(global, CapabilityScope::Global));

        let subnet_local =
            scope_from_membership_tags(&["scope:subnet-local".into(), "gpu".into()]);
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
        let ann = CapabilityAnnouncement::new(
            0xAA,
            EntityId::from_bytes([0u8; 32]),
            7,
            caps,
        );

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
        let mut nodes = find_nodes_matching_scoped(
            &fold,
            &legacy,
            &ScopeFilter::SameSubnet,
            lookup,
        );
        nodes.sort();
        assert_eq!(nodes, vec![0xBB]);
    }
}
