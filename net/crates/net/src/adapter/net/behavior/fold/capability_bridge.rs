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
use super::super::org::OrgId;
use super::super::org_revocation::OrgRevocationState;
use super::capability::{
    resolve_candidate_keys, CapabilityFilter, CapabilityFold, CapabilityMembership,
    HardwareSummary, VerifiedOwner,
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
    // projection, and `tag_groups_all` resolves against the
    // index's separate `by_synthetic` map — so a raw published tag
    // whose string happens to equal a synthetic key (e.g. a
    // `Tag::Legacy("model:llama3")`) can never satisfy these axes.
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
/// typically `.expect("apply")`.
///
/// # Not a production ingest path
///
/// This is a FIXTURE helper. It passes `floors = None`, meaning every
/// certificate generation is admissible — the revocation floor check is
/// skipped entirely — because a fixture priming a bare fold has no node state
/// to check against. Real ingest goes through the dispatch path in `mesh.rs`,
/// which supplies the node's live floors and pairs the apply with a
/// `recheck_projected_owner_floor`.
///
/// Every in-tree caller is a test or a bench (verified: no production call
/// site remains, which makes the "~30 production call sites" note in
/// `CODE_REVIEW_2026_05_23_MULTIFOLD_DEFERRED.md` MD-1 stale). It stays `pub`
/// only because `benches/net.rs` is a separate compilation target and cannot
/// see a `#[cfg(test)]` item.
///
/// The unretractable-projection hazard this used to carry is closed at the
/// producer instead: [`verify_announced_owner_cert`] now refuses a cert whose
/// entity does not derive the announced node id (§12), so no caller of this
/// helper can install ownership that a floor raise could never clear.
pub fn apply_legacy_announcement(
    fold: &Fold<CapabilityFold>,
    ann: CapabilityAnnouncement,
) -> Result<ApplyOutcome, FoldError> {
    // Mirror the production dispatch path's OA-1 ingest
    // verification, INCLUDING the outer-signature precondition (no
    // revocation floors in this fixture-shaped helper — floors are
    // node state, and fixtures priming a bare fold have none).
    let outer_signature_verified = ann.verify().is_ok();
    let verified_owner = verify_announced_owner_cert(&ann, outer_signature_verified, None, 0);
    let fold_ann = translate_announcement(&ann, verified_owner);
    fold.apply(fold_ann)
}

/// OA-1 ingest verification for an announcement's `owner_cert` —
/// the ONLY producer of a `Some` value for the fold's
/// [`CapabilityMembership::owner`] projection.
///
/// Returns `Some(VerifiedOwner)` iff ALL of:
///
/// 1. `outer_signature_verified` — the ENCLOSING announcement's
///    signature verified (review-8 §1). A membership certificate
///    proves that an entity belongs to an organization; it binds
///    neither the advertised capabilities nor this announcement's
///    version, so a valid replayed cert must never lend an owned
///    projection to an unsigned (or signature-invalid) capability
///    statement. The precondition is part of the function
///    signature so no future caller can forget it.
/// 2. the announcement carries a cert,
/// 3. the cert's `member` equals the announcement's `entity_id`
///    (a cert vouches for exactly the announcing entity — a valid
///    cert for someone else is not belonging),
/// 4. the cert verifies structurally and cryptographically
///    (`verify_strict` under `net-org-cert-v1`, TTL ceiling) and
///    is inside its validity window with `skew_secs` tolerance,
/// 5. the cert's `generation` is at or above the node's persisted
///    revocation floor for `(org, member)` (`floors = None` means
///    this node tracks no floors — every generation admissible,
///    identical to an empty state).
///
/// On any failure the cert is dropped and announcement HANDLING is
/// unchanged (OA-1 exit-gate contract: "ingest drops bad certs,
/// not announcements"): an unsigned announcement stays governed by
/// the caller's existing `require_signed` policy — discoverable in
/// unsigned-discovery mode, merely never owned.
///
/// Belonging only: the returned projection feeds discovery, never
/// `may_execute`.
///
/// `pub(crate)` (review-9): `outer_signature_verified` is a
/// caller-asserted fact, so only the in-crate dispatch/self-index
/// paths — which computed it from a real signature check — may
/// call this. Combined with [`VerifiedOwner`]'s private
/// construction, verified ingest is structurally the only
/// ownership producer.
pub(crate) fn verify_announced_owner_cert(
    ann: &CapabilityAnnouncement,
    outer_signature_verified: bool,
    floors: Option<&OrgRevocationState>,
    skew_secs: u64,
) -> Option<VerifiedOwner> {
    let cert = ann.owner_cert.as_ref()?;
    if !outer_signature_verified {
        tracing::debug!(
            node_id = format!("{:#x}", ann.node_id),
            org = %cert.org_id,
            "dropping owner cert: enclosing announcement is not signature-verified \
             (announcement handling unchanged)"
        );
        return None;
    }
    if cert.member != ann.entity_id {
        tracing::debug!(
            node_id = format!("{:#x}", ann.node_id),
            org = %cert.org_id,
            "dropping owner cert: member does not match announcing entity (announcement kept)"
        );
        return None;
    }
    // §12 — the entity must actually BE the announcing node.
    //
    // `retract_floored_ownership` locates entries to retract via
    // `member.node_id()`, and the install sweep and post-apply recheck both
    // search `by_node[entity.node_id()]`. That only works because production
    // ingest guarantees `ann.entity_id.node_id() == ann.node_id` (enforced at
    // the dispatch site). An announcement violating it lands the projection in
    // `by_node[ann.node_id]` while every retraction path looks under
    // `by_node[entity.node_id()]` — so NO floor raise, no store install, and no
    // recheck can ever clear it, and `owner_org_for` keeps reporting the
    // revoked org indefinitely.
    //
    // Checked HERE rather than only at the dispatch site because this function
    // is the single producer of a `Some(VerifiedOwner)`: the `#[doc(hidden)]`
    // `MeshNode::test_inject_capability_announcement` seam (which ships in
    // release builds and is re-exported by the Python / Node / Go bindings as a
    // synthetic-peer helper) and the `pub` `apply_legacy_announcement` fixture
    // helper both reach the fold without passing the dispatch check. Enforcing
    // the bind at the producer makes the retraction invariant hold for every
    // path, present and future.
    //
    // Synthetic-peer injection is unaffected: those announcements carry no
    // `owner_cert`, so they return at the `?` above and never reach this check.
    // §12 — the entity must actually BE the announcing node.
    //
    // `retract_floored_ownership` locates entries to retract via
    // `member.node_id()`, and the install sweep and post-apply recheck both
    // search `by_node[entity.node_id()]`. That only works because production
    // ingest guarantees `ann.entity_id.node_id() == ann.node_id` (enforced at
    // the dispatch site). An announcement violating it lands the projection in
    // `by_node[ann.node_id]` while every retraction path looks under
    // `by_node[entity.node_id()]` — so NO floor raise, no store install, and no
    // recheck can ever clear it, and `owner_org_for` keeps reporting the
    // revoked org indefinitely.
    //
    // Checked HERE rather than only at the dispatch site because this function
    // is the single producer of a `Some(VerifiedOwner)`: the `#[doc(hidden)]`
    // `MeshNode::test_inject_capability_announcement` seam (which ships in
    // release builds and is re-exported by the Python / Node / Go bindings as a
    // synthetic-peer helper) and the `pub` `apply_legacy_announcement` fixture
    // helper both reach the fold without passing the dispatch check. Enforcing
    // the bind at the producer makes the retraction invariant hold for every
    // path, present and future.
    //
    // Synthetic-peer injection is unaffected: those announcements carry no
    // `owner_cert`, so they return at the `?` above and never reach this check.
    if ann.entity_id.node_id() != ann.node_id {
        tracing::debug!(
            node_id = format!("{:#x}", ann.node_id),
            entity_node_id = format!("{:#x}", ann.entity_id.node_id()),
            org = %cert.org_id,
            "dropping owner cert: announcing entity does not derive the announced              node id — an ownership projection under a mismatched node id could              never be retracted (announcement kept)"
        );
        return None;
    }
    if let Err(e) = cert.is_valid_with_skew(skew_secs) {
        tracing::debug!(
            node_id = format!("{:#x}", ann.node_id),
            org = %cert.org_id,
            error = %e,
            "dropping unverifiable owner cert (announcement kept)"
        );
        return None;
    }
    if let Some(floors) = floors {
        let floor = floors.floor_for(&cert.org_id, &cert.member);
        if cert.generation < floor {
            tracing::debug!(
                node_id = format!("{:#x}", ann.node_id),
                org = %cert.org_id,
                generation = cert.generation,
                floor,
                "dropping owner cert below revocation floor (announcement kept)"
            );
            return None;
        }
    }
    Some(VerifiedOwner::new(cert.org_id, cert.generation))
}

/// Post-apply floor recheck (review-9): a floor can rise BETWEEN
/// owner-cert verification and the fold apply — the raise callback
/// completes while the projection is not yet in the fold, the
/// delayed apply then installs it, and no future callback fires.
/// Every production apply that installed a `Some(VerifiedOwner)`
/// therefore rereads the CURRENT floors afterwards and retracts if
/// the just-applied projection is already below them. Combined
/// with the raise callback, every ordering retracts:
///
/// ```text
/// raise after apply:                the callback retracts
/// raise before the delayed apply:   this recheck retracts
/// raise between apply and recheck:  callback or recheck retracts
/// ```
///
/// The generation comparison stays exact — a newer projection is
/// never over-cleared. Returns how many entries were retracted.
pub(crate) fn recheck_projected_owner_floor(
    fold: &Fold<CapabilityFold>,
    floors: Option<&OrgRevocationState>,
    member: &crate::adapter::net::identity::EntityId,
    owner: &VerifiedOwner,
) -> usize {
    let Some(floors) = floors else {
        return 0;
    };
    let floor = floors.floor_for(&owner.org(), member);
    if owner.generation() < floor {
        retract_floored_ownership(fold, owner.org(), member, floor)
    } else {
        0
    }
}

/// The verified owner org projected for `node_id`, if any — walks
/// the publisher's fold entries via the `by_node` reverse index and
/// returns the first `Some` (mirrors `reflex_addr_for`'s
/// one-publisher-one-value shape).
pub fn owner_org_for(fold: &Fold<CapabilityFold>, node_id: NodeId) -> Option<OrgId> {
    fold.with_state(|state| {
        let keys = state.by_node.get(&node_id)?;
        keys.iter().find_map(|key| {
            state
                .entries
                .get(key)
                .and_then(|entry| entry.payload.owner.map(|owner| owner.org()))
        })
    })
}

/// Retract ownership projections a rising revocation floor just
/// invalidated (review-8 §9): every fold entry published by
/// `member` whose projection came from a cert of `org` with
/// `generation < floor` loses ONLY its `owner` field — the
/// capability entry stays present and queryable, and `may_execute`
/// is untouched. Projections from higher-generation certs survive:
/// the retained generation makes retraction exact, never an
/// over-clear.
///
/// The publisher's fold identity is derived from the member key
/// (`member.node_id()`) — the same entity→node binding announcement
/// ingest verified. Returns how many entries were retracted.
///
/// A retraction changes query-visible state (`owner_org_for`), so
/// it bumps the fold change generation exactly like an `apply`
/// (review-9): watch-based consumers and generation-keyed caches
/// observe it.
pub fn retract_floored_ownership(
    fold: &Fold<CapabilityFold>,
    org: OrgId,
    member: &crate::adapter::net::identity::EntityId,
    floor: u32,
) -> usize {
    let node_id = member.node_id();
    // §14: probe under a SHARED read first. `with_state_mut` takes an
    // exclusive write lock unconditionally, before it even checks whether this
    // node has any entries — and the install sweep calls this once per floor
    // in the persisted state, the overwhelming majority of which retract
    // nothing. Paying a write lock to discover "no entries for this node"
    // serialized every concurrent `may_execute` / `has_local_capability` /
    // discovery query behind a walk that had nothing to do.
    //
    // Not a TOCTOU: an entry appearing between the probe and the write can
    // only be a NEWER announcement, which carries its own ingest-time floor
    // check, and the post-apply `recheck_projected_owner_floor` covers the
    // interleaving explicitly. Missing it here is the same outcome as the
    // sweep having run a moment earlier.
    if fold.with_state(|state| !state.by_node.contains_key(&node_id)) {
        return 0;
    }
    let retracted = fold.with_state_mut(|state| {
        let Some(keys) = state.by_node.get(&node_id) else {
            return 0;
        };
        let keys: Vec<_> = keys.iter().copied().collect();
        let mut retracted = 0;
        for key in keys {
            if let Some(entry) = state.entries.get_mut(&key) {
                if let Some(owner) = entry.payload.owner {
                    if owner.org() == org && owner.generation() < floor {
                        entry.payload.owner = None;
                        retracted += 1;
                    }
                }
            }
        }
        retracted
    });
    if retracted > 0 {
        // §15: on the AUDIT plane, not only `tracing`. This is the one
        // security-relevant fold transition the org feature produces, and it
        // was the only one an installed `FoldAuditSink` never saw.
        fold.notify_projection_retracted(
            format!("node:{node_id:#x}"),
            format!("ownership retracted under org {org} at floor {floor} ({retracted} entries)"),
        );
    }
    retracted
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
    synthesize_capability_set_if_known(fold, node_id).unwrap_or_default()
}

/// `synthesize_capability_set` with an explicit "known to the fold"
/// signal: returns `None` when `node_id` has no fold entries at all
/// (matches the placement-side hard-veto contract: "unindexed
/// candidate" → reject without scoring).
///
/// Per PERF_AUDIT §4.9 — placement_score previously took the fold's
/// read lock twice per candidate: once for a `by_node.contains_key`
/// known-check, once for the full synthesize. Both can be served by
/// a single `with_state` that probes `by_node`, returns `None` on
/// miss, and synthesizes the set on hit. Cuts the per-candidate
/// lock acquisitions from 2 to 1, halving the lock-contention
/// surface area on the read side under high candidate counts.
pub fn synthesize_capability_set_if_known(
    fold: &Fold<CapabilityFold>,
    node_id: NodeId,
) -> Option<super::super::capability::CapabilitySet> {
    fold.with_state(|state| {
        let keys = state.by_node.get(&node_id)?;
        let mut caps = super::super::capability::CapabilitySet::new();
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
        Some(caps)
    })
}

/// Default capacity for the per-fold capability-set cache. Covers
/// typical mesh sizes; an operator-tuned `MeshNode` can override
/// via [`CapabilitySetCache::with_capacity`].
const CAPABILITY_SET_CACHE_DEFAULT_CAPACITY: usize = 256;

/// Bounded LRU cache of synthesized `Arc<CapabilitySet>` per node,
/// invalidated by the fold's change-generation
/// ([`Fold::change_generation`]). Eliminates the multi-µs
/// re-parse + re-allocate that `synthesize_capability_set` does
/// per call on the hot paths (per-packet greedy admission at
/// mesh.rs:5181, per-candidate `placement_score`, per-candidate
/// `best_by_score`, per-call `may_execute` retain loops). Per
/// PERF_AUDIT_2026_06_10_FULL_CRATE.md §4.1.
///
/// The generation is global to the fold — any fold change
/// invalidates every cached entry. That's coarse but accurate:
/// announcement rates are low relative to scoring/per-packet
/// rates, so the cache stays warm in steady state. On a generation
/// bump the next access re-synthesizes and re-caches under the new
/// generation; entries for other nodes return stale results once,
/// triggering their own re-synthesize on access.
///
/// Cache hits return a refcount-bumped `Arc` (~ns); misses pay the
/// existing `synthesize_capability_set` cost plus one Arc alloc.
pub struct CapabilitySetCache {
    inner: parking_lot::Mutex<lru::LruCache<NodeId, CachedCapabilitySetEntry>>,
}

struct CachedCapabilitySetEntry {
    generation: u64,
    caps: std::sync::Arc<super::super::capability::CapabilitySet>,
}

impl CapabilitySetCache {
    /// Construct with the default capacity (256 entries).
    pub fn new() -> Self {
        Self::with_capacity(CAPABILITY_SET_CACHE_DEFAULT_CAPACITY)
    }

    /// Construct with a caller-supplied capacity. `capacity == 0`
    /// is treated as 1 to satisfy `lru::LruCache`'s NonZero
    /// contract — the caller would have to deliberately defeat
    /// the cache to set 0, so silently rounding up is friendlier
    /// than panicking.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap =
            std::num::NonZeroUsize::new(capacity.max(1)).unwrap_or(std::num::NonZeroUsize::MIN);
        Self {
            inner: parking_lot::Mutex::new(lru::LruCache::new(cap)),
        }
    }

    /// Return a refcount-shareable snapshot of `node_id`'s
    /// capability set against the current fold generation. Cache
    /// hit returns an `Arc::clone`; miss re-synthesizes via
    /// [`synthesize_capability_set`] and stores against the
    /// fold's current generation before returning.
    pub fn get_or_synthesize(
        &self,
        fold: &Fold<CapabilityFold>,
        node_id: NodeId,
    ) -> std::sync::Arc<super::super::capability::CapabilitySet> {
        let current_gen = fold.change_generation();
        // Fast path — cache hit at the current generation.
        {
            let mut lru = self.inner.lock();
            if let Some(entry) = lru.get(&node_id) {
                if entry.generation == current_gen {
                    return entry.caps.clone();
                }
            }
        }
        // Miss / stale. Synthesize outside the cache lock so we
        // don't serialize callers on a long synthesize (the fold's
        // own read lock still serializes the with_state body, but
        // that's much cheaper).
        let caps = std::sync::Arc::new(synthesize_capability_set(fold, node_id));
        // Store against the generation captured BEFORE synthesize
        // (`current_gen`). The fold bumps its generation under the
        // state write lock AFTER mutating, so a set synthesized
        // after we read gen G reflects state at gen >= G; if a
        // concurrent apply/evict ran during synthesize the live
        // generation is already > G and the entry misses on the
        // next access (one wasted re-synthesize, never a stale
        // hit). Stamping the generation read AFTER synthesize
        // would invert that: a set built from pre-change state
        // could be stored under the post-change generation and
        // served stale until the next unrelated fold mutation.
        {
            let mut lru = self.inner.lock();
            lru.put(
                node_id,
                CachedCapabilitySetEntry {
                    generation: current_gen,
                    caps: caps.clone(),
                },
            );
        }
        caps
    }

    /// Drop every cached entry. Useful in tests; production code
    /// relies on the change-generation invalidation.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Number of currently-cached entries (any generation).
    /// Used by tests + operator metrics. Cheap; takes the lock.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// True if no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

impl Default for CapabilitySetCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CapabilitySetCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilitySetCache")
            .field("len", &self.len())
            .finish()
    }
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

/// OA-2 §2.4a: does `target_node` carry `capability_tag` in the
/// fold, evaluating NO legacy allow-lists?
///
/// The narrow companion to [`may_execute`] for the
/// organization-admission seam. `may_execute` unions
/// `allowed_nodes` / `allowed_subnets` / `allowed_groups`
/// TARGET-WIDE across EVERY capability entry the target carries, so
/// an unrelated restricted capability (e.g. an admin service with a
/// tight `allowed_nodes`) on the same provider would gate a
/// protected service's callers before `OrgAdmission` ever ran. A
/// service whose admission is `OwnerDelegated` / `CrossOrgGranted`
/// therefore resolves its registered admission FIRST and uses THIS
/// check — "is the exact service locally registered and capable?"
/// — as its only fold precondition, then runs the OA-2 admission
/// engine
/// ([`verify_org_admission`](crate::adapter::net::behavior::org_admission::verify_org_admission))
/// as the load-bearing authority.
///
/// Reads ONLY tag presence — it evaluates no allow-lists and
/// confers no authority on its own, so `may_execute` stays
/// byte-for-byte unchanged for existing public / v0.4 services.
pub fn has_local_capability(
    fold: &Fold<CapabilityFold>,
    target_node: NodeId,
    capability_tag: &str,
) -> bool {
    fold.with_state(|state| {
        let Some(keys) = state.by_node.get(&target_node) else {
            return false;
        };
        keys.iter().any(|k| {
            state
                .entries
                .get(k)
                .is_some_and(|entry| entry.payload.tags.iter().any(|t| t == capability_tag))
        })
    })
}

/// Batched `may_execute` for the caller-side `candidates.retain(...)`
/// path: takes ONE fold read lock for the whole batch and derives
/// the caller's subnet + group membership ONCE outside the per-
/// target loop. Returns a `Vec<bool>` parallel to `targets` — index
/// `i` is `true` iff `caller_node` may execute `capability_tag` on
/// `targets[i]`.
///
/// Per PERF_AUDIT §4.2 — pre-fix the retain-style callers at
/// `mesh_rpc.rs:3093` and `:3185` called the single-target
/// [`may_execute`] per candidate. Each call took a fresh
/// `with_state` read lock, walked the caller's entries to re-parse
/// `subnet:` / `group:` tags from the string form, and allocated
/// three allow-list Vecs. With 100 candidates that's 100 lock
/// acquisitions + 100 parses of the caller's tag bag + 300 Vec
/// allocations for the same answer.
pub fn may_execute_batch(
    fold: &Fold<CapabilityFold>,
    targets: &[NodeId],
    capability_tag: &str,
    caller_node: NodeId,
) -> Vec<bool> {
    if targets.is_empty() {
        return Vec::new();
    }
    fold.with_state(|state| {
        // Hoist the caller's subnet + groups out of the per-target
        // loop. Identical across every iteration; pre-fix this re-
        // walked + re-parsed every retain step.
        let (caller_subnet, caller_groups) = derive_caller_axes(state, caller_node);
        targets
            .iter()
            .map(|target_node| {
                may_execute_with_caller(
                    state,
                    *target_node,
                    capability_tag,
                    caller_node,
                    caller_subnet.as_ref(),
                    &caller_groups,
                )
            })
            .collect()
    })
}

/// Internal: derive `(subnet, groups)` for `caller_node` from the
/// fold's `by_node` reverse index. `subnet:<hex>` / `group:<hex>`
/// tags are mapped through `SubnetId::from_tag` / `GroupId::from_tag`.
/// Returns `(None, Vec::new())` for an unknown caller.
fn derive_caller_axes(
    state: &super::state::FoldState<CapabilityFold>,
    caller_node: NodeId,
) -> (
    Option<super::super::subnet::SubnetId>,
    Vec<super::super::group::GroupId>,
) {
    let Some(caller_keys) = state.by_node.get(&caller_node) else {
        return (None, Vec::new());
    };
    let mut caller_subnet = None;
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
    (caller_subnet, caller_groups)
}

/// Internal: per-target verdict that takes the pre-derived caller
/// axes instead of re-walking the caller's entries per call. Same
/// semantics as the inner body of [`may_execute`].
fn may_execute_with_caller(
    state: &super::state::FoldState<CapabilityFold>,
    target_node: NodeId,
    capability_tag: &str,
    caller_node: NodeId,
    caller_subnet: Option<&super::super::subnet::SubnetId>,
    caller_groups: &[super::super::group::GroupId],
) -> bool {
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
    if !allowed_subnets.is_empty() {
        if let Some(subnet) = caller_subnet {
            if allowed_subnets.contains(subnet) {
                return true;
            }
        }
    }
    if !allowed_groups.is_empty() {
        for g in caller_groups {
            if allowed_groups.contains(g) {
                return true;
            }
        }
    }
    false
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
///
/// `verified_owner` is the OA-1 ownership projection and MUST
/// come from `verify_announced_owner_cert` (or be `None`). The
/// parameter is deliberately explicit rather than derived here:
/// this function is pure and is also reached from paths that never
/// verified anything (test injection, fixture priming), so the
/// authority decision has to be visible at every call site.
pub fn translate_announcement(
    ann: &CapabilityAnnouncement,
    verified_owner: Option<VerifiedOwner>,
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
            owner: verified_owner,
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
    let range_predicates_present = legacy.min_memory_gb.is_some()
        || legacy.min_vram_gb.is_some()
        || !legacy.require_modalities.is_empty()
        || legacy.min_context_length.is_some();
    // PERF_AUDIT §4.11 — permissive fast path: when no field of
    // the translated filter constrains the candidate set AND no
    // legacy range/modality predicate would tighten the post-
    // filter, the result is simply "every distinct publisher
    // node id in the fold". Skip the full `HashSet<(class,
    // NodeId)>` build + per-key retain loops that the general
    // path runs, and the per-entry payload borrow + range check
    // the legacy post-filter does. `state.by_node` already keys
    // by NodeId so iteration is dedup-free; sort to preserve the
    // deterministic-order contract callers rely on.
    if fold_filter.is_permissive() && !range_predicates_present {
        return fold.with_state(|state| {
            let mut ids: Vec<NodeId> = state.by_node.keys().copied().collect();
            ids.sort_unstable();
            ids
        });
    }
    // Resolve the indexed-axis candidate keys and run the
    // non-indexed post-filter against *borrowed* payloads, all
    // under one read-lock acquisition. The bulk path only needs
    // node ids out, so we never clone a `CapabilityMembership` —
    // unlike the `Vec<CapabilityMatch>` query path, which clones
    // every match before the caller can discard it.
    let mut out: Vec<NodeId> = fold.with_state_and_index(|state, index| {
        let candidates = resolve_candidate_keys(state, index, &fold_filter);
        let candidates = candidates.as_set();
        let mut ids: Vec<NodeId> = Vec::with_capacity(candidates.len());
        for &key in candidates {
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
    // candidate keys via the index and run the range post-filter
    // against borrowed payloads without cloning. We derive each
    // survivor's scope here too (cheap, just parses the borrowed
    // tags), but we do NOT call `same_subnet_lookup` under the
    // lock — it's a caller-supplied closure opaque to the bridge,
    // so running it while we hold the fold read locks risks lock
    // contention or re-entrancy (a closure that itself queries the
    // fold). Collect `(node, scope)` and apply the subnet/scope
    // gate after the locks drop.
    let scoped: Vec<(NodeId, CapabilityScope)> = fold.with_state_and_index(|state, index| {
        let candidates = resolve_candidate_keys(state, index, &fold_filter);
        let candidates = candidates.as_set();
        let mut acc: Vec<(NodeId, CapabilityScope)> = Vec::with_capacity(candidates.len());
        for &key in candidates {
            let Some(entry) = state.entries.get(&key) else {
                continue;
            };
            let membership = &entry.payload;
            // The index already resolved the tag / model / tool /
            // gpu axes; only the range predicates remain.
            if !membership_passes_range_filter(membership, legacy) {
                continue;
            }
            acc.push((key.1, scope_from_membership_tags(&membership.tags)));
        }
        acc
    });
    // Locks released: apply the scope gate, invoking the caller's
    // subnet closure outside any fold lock.
    let mut out: Vec<NodeId> = scoped
        .into_iter()
        .filter(|(node_id, candidate_scope)| {
            matches_scope(candidate_scope, scope, same_subnet_lookup(*node_id))
        })
        .map(|(node_id, _)| node_id)
        .collect();
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
                owner: None,
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
            !tags.iter().any(|t| t.starts_with("model:")
                || t.starts_with("tool:")
                || t.starts_with("gpu:")),
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
            owner: None,
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
            owner: None,
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

    /// PERF_AUDIT §4.11 — a filter whose only constraint is a
    /// range predicate translates to a permissive fold filter
    /// (`is_permissive() == true`), so it MUST NOT take the
    /// permissive fast path: the range post-filter still tightens
    /// the result. Pins the `range_predicates_present` guard in
    /// `find_nodes_matching` — dropping it would make a
    /// `min_memory_gb` query return every node in the fold.
    #[test]
    fn find_nodes_matching_range_only_filter_skips_permissive_fast_path() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let big = HardwareSummary {
            gpu_vendor: None,
            gpu_count: 0,
            memory_gb: Some(128),
            vram_gb: None,
        };
        fold.apply(sign_member(&kp, 0xA1, 0x100, vec!["gpu"], Some(big)))
            .expect("apply big");
        fold.apply(sign_member(&kp, 0xA2, 0x100, vec!["gpu"], None))
            .expect("apply no-hw");

        let range_only = LegacyFilter {
            min_memory_gb: Some(64),
            ..LegacyFilter::default()
        };
        // The translated fold filter carries no constraint...
        assert!(translate_filter(&range_only).is_permissive());
        // ...but the range predicate must still apply: only the
        // 128 GB node passes; the hardware-less node fails closed.
        let nodes = find_nodes_matching(&fold, &range_only);
        assert_eq!(nodes, vec![0xA1]);

        // Sanity: the truly permissive filter returns both, sorted.
        let all = find_nodes_matching(&fold, &LegacyFilter::default());
        assert_eq!(all, vec![0xA1, 0xA2]);
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
        // CC: a spoofer. Emits raw tags whose strings collide with
        // the index-only synthetic namespace, but carries no real
        // model/tool bundle and no hardware. Both paths must agree
        // it matches none of the model/tool/gpu axes — the synthetic
        // index is fed only by `derive_synthetic_index_tags`, never
        // by raw published tag strings.
        fold.apply(sign_member(
            &kp,
            0xCC,
            0x100,
            vec![
                "model:llama3",
                "tool:ffmpeg",
                "gpu:present",
                "gpu:vendor:nvidia",
            ],
            None,
        ))
        .expect("apply CC");

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
        // 0xCC is probed on every index-resolved axis: its raw
        // colliding tags must NOT satisfy any of them on either path.
        let model_hit = LegacyFilter {
            require_models: vec!["llama3".into()],
            ..LegacyFilter::default()
        };
        probe(&model_hit, &[0xAA, 0xBB, 0xCC]);
        let model_miss = LegacyFilter {
            require_models: vec!["does-not-exist".into()],
            ..LegacyFilter::default()
        };
        probe(&model_miss, &[0xAA, 0xBB, 0xCC]);
        let tool_hit = LegacyFilter {
            require_tools: vec!["ffmpeg".into()],
            ..LegacyFilter::default()
        };
        probe(&tool_hit, &[0xAA, 0xBB, 0xCC]);
        let gpu = LegacyFilter {
            require_gpu: true,
            ..LegacyFilter::default()
        };
        probe(&gpu, &[0xAA, 0xBB, 0xCC]);
        let vendor_hit = LegacyFilter {
            gpu_vendor: Some(GpuVendor::Nvidia),
            ..LegacyFilter::default()
        };
        probe(&vendor_hit, &[0xAA, 0xBB, 0xCC]);
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
    fn raw_tags_cannot_spoof_the_synthetic_model_tool_gpu_namespace() {
        // The model / tool / gpu axes resolve through the index-only
        // synthetic tag map, which is fed solely by
        // `derive_synthetic_index_tags`. A publisher must not be able
        // to satisfy those axes by emitting a raw tag string that
        // happens to equal a synthetic key — published tags are
        // arbitrary (`Tag::Legacy` round-trips verbatim), so this
        // would otherwise be a free capability spoof on the bulk
        // path while the single-target path (which scans the real
        // bundle / hardware) disagreed.
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let nvidia_hw = HardwareSummary {
            gpu_vendor: Some("nvidia".into()),
            gpu_count: 1,
            memory_gb: Some(64),
            vram_gb: Some(24),
        };
        // Honest node: real model/tool bundle + nvidia hardware.
        fold.apply(sign_member(
            &kp,
            0xAA,
            0x100,
            vec![
                "software.model.0.id=llama3",
                "software.tool.0.tool_id=ffmpeg",
            ],
            Some(nvidia_hw),
        ))
        .expect("apply AA");
        // Spoofer: raw tags string-equal to the synthetic keys, but
        // no bundle and no hardware.
        fold.apply(sign_member(
            &kp,
            0xBB,
            0x100,
            vec![
                "model:llama3",
                "tool:ffmpeg",
                "gpu:present",
                "gpu:vendor:nvidia",
            ],
            None,
        ))
        .expect("apply BB");

        // Only the honest node resolves on each axis — the spoofer is
        // invisible to the synthetic index.
        let model = find_nodes_matching(
            &fold,
            &LegacyFilter {
                require_models: vec!["llama3".into()],
                ..LegacyFilter::default()
            },
        );
        assert_eq!(model, vec![0xAA]);
        let tool = find_nodes_matching(
            &fold,
            &LegacyFilter {
                require_tools: vec!["ffmpeg".into()],
                ..LegacyFilter::default()
            },
        );
        assert_eq!(tool, vec![0xAA]);
        let gpu = find_nodes_matching(
            &fold,
            &LegacyFilter {
                require_gpu: true,
                ..LegacyFilter::default()
            },
        );
        assert_eq!(gpu, vec![0xAA]);
        let vendor = find_nodes_matching(
            &fold,
            &LegacyFilter {
                gpu_vendor: Some(GpuVendor::Nvidia),
                ..LegacyFilter::default()
            },
        );
        assert_eq!(vendor, vec![0xAA]);

        // And the bulk verdict for the spoofer matches the
        // single-target path on every axis (both: no match).
        for legacy in [
            LegacyFilter {
                require_models: vec!["llama3".into()],
                ..LegacyFilter::default()
            },
            LegacyFilter {
                require_tools: vec!["ffmpeg".into()],
                ..LegacyFilter::default()
            },
            LegacyFilter {
                require_gpu: true,
                ..LegacyFilter::default()
            },
            LegacyFilter {
                gpu_vendor: Some(GpuVendor::Nvidia),
                ..LegacyFilter::default()
            },
        ] {
            assert!(
                !target_matches_filter(&fold, 0xBB, &legacy),
                "spoofer unexpectedly matched single-target path for {legacy:?}"
            );
        }
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

        let translated = translate_announcement(&ann, None);
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
        let translated = translate_announcement(&ann, None);
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

    /// PERF_AUDIT §4.1 — cache hits return the SAME `Arc` instance
    /// across calls without re-synthesizing. Pre-fix, every call
    /// to `synthesize_capability_set` allocated a fresh
    /// `CapabilitySet`; with the cache, hits are refcount bumps of
    /// one shared snapshot.
    #[test]
    fn capability_set_cache_returns_same_arc_on_hit() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_member(&kp, 0xAB, 0x100, vec!["gpu"], None))
            .expect("apply AB");

        let cache = CapabilitySetCache::new();
        let first = cache.get_or_synthesize(&fold, 0xAB);
        let second = cache.get_or_synthesize(&fold, 0xAB);
        // Arc::ptr_eq is the strict guarantee the audit asks for —
        // hits must be refcount bumps, not fresh allocations.
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "cache hit must return the same Arc instance"
        );
        // And the content must reflect the fold state.
        // Tag::Display round-trips byte-for-byte across all variants,
        // so checking the display form is the robust shape-agnostic
        // way to assert the published "gpu" tag landed in the cache.
        assert!(
            first.tags.iter().any(|t| t.to_string() == "gpu"),
            "synthesized capability set should contain the published `gpu` tag: {:?}",
            first.tags
        );
    }

    /// PERF_AUDIT §4.1 — a fold mutation must invalidate the
    /// cached entry on the next access, returning a fresh `Arc`
    /// whose contents reflect the post-mutation state.
    #[test]
    fn capability_set_cache_invalidates_on_fold_change() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_member(&kp, 0xCD, 0x100, vec!["gpu"], None))
            .expect("apply CD v1");
        let cache = CapabilitySetCache::new();
        let v1 = cache.get_or_synthesize(&fold, 0xCD);
        let v1_tag_count = v1.tags.len();

        // Replace announcement with a richer tag set (bumps the
        // fold's change generation via the apply path).
        let v2_ann = SignedAnnouncement::sign(
            &kp,
            super::super::capability::CapabilityFold::KIND_ID,
            0x100,
            0xCD,
            2,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: 0x100,
                tags: vec!["gpu".into(), "cuda".into(), "fp16".into()],
                hardware: None,
                state: NodeState::Idle,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
                owner: None,
            },
        )
        .expect("sign v2");
        fold.apply(v2_ann).expect("apply CD v2");

        let v2 = cache.get_or_synthesize(&fold, 0xCD);
        assert!(
            !std::sync::Arc::ptr_eq(&v1, &v2),
            "fold change must invalidate the cached entry"
        );
        assert!(
            v2.tags.len() > v1_tag_count,
            "post-mutation cache miss must reflect the new tag set"
        );
    }

    /// PERF_AUDIT §4.1 — unknown node (no fold entry) returns an
    /// empty set; the cache should still populate against it so a
    /// subsequent lookup is a refcount hit, not a no-op
    /// re-synthesize.
    #[test]
    fn capability_set_cache_populates_for_unknown_node() {
        let fold = new_fold();
        let cache = CapabilitySetCache::new();
        let first = cache.get_or_synthesize(&fold, 0xDEAD_BEEF);
        assert!(first.tags.is_empty());
        assert!(first.metadata.is_empty());
        let second = cache.get_or_synthesize(&fold, 0xDEAD_BEEF);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "unknown-node entries still hit the cache on repeat access"
        );
    }

    /// PERF_AUDIT §4.9 — `synthesize_capability_set_if_known`
    /// folds the placement-side known-check and the synthesize
    /// into one lock acquisition. Pin its three-branch contract:
    /// unknown publisher → `None` (placement hard-veto), known
    /// publisher with no tags → `Some(empty)` (indexed, proceeds
    /// to scoring), known publisher with tags → `Some(populated)`.
    /// The legacy `synthesize_capability_set` wrapper must map
    /// `None` to an empty set (its pre-fix shape).
    #[test]
    fn synthesize_if_known_distinguishes_unknown_from_empty() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_member(&kp, 0xB1, 0x100, vec!["gpu"], None))
            .expect("apply tagged");
        fold.apply(sign_member(&kp, 0xB2, 0x100, vec![], None))
            .expect("apply untagged");

        // Unknown → None (hard veto).
        assert!(synthesize_capability_set_if_known(&fold, 0xDEAD).is_none());
        // Known + tags → Some(populated).
        let tagged =
            synthesize_capability_set_if_known(&fold, 0xB1).expect("tagged publisher is known");
        assert!(tagged.tags.iter().any(|t| t.to_string() == "gpu"));
        // Known + no tags → Some(empty) — indexed candidates with
        // empty tag sets still proceed to scoring.
        let untagged = synthesize_capability_set_if_known(&fold, 0xB2)
            .expect("untagged publisher is still known");
        assert!(untagged.tags.is_empty());
        // Wrapper parity: unknown maps to the empty default.
        assert!(synthesize_capability_set(&fold, 0xDEAD).tags.is_empty());
    }

    /// PERF_AUDIT §4.1 — node REMOVAL (`evict_node`, which the
    /// SWIM death path drives) must invalidate the cached entry:
    /// the eviction bumps the fold's change generation, so the
    /// next lookup misses and re-synthesizes an empty set instead
    /// of serving the dead node's capabilities forever.
    #[test]
    fn capability_set_cache_invalidates_on_node_eviction() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        fold.apply(sign_member(&kp, 0xEE, 0x100, vec!["gpu"], None))
            .expect("apply EE");
        let cache = CapabilitySetCache::new();
        let live = cache.get_or_synthesize(&fold, 0xEE);
        assert!(
            live.tags.iter().any(|t| t.to_string() == "gpu"),
            "pre-eviction lookup should see the published tag"
        );

        fold.evict_node(0xEE, "swim-dead");

        let after = cache.get_or_synthesize(&fold, 0xEE);
        assert!(
            !std::sync::Arc::ptr_eq(&live, &after),
            "eviction must invalidate the cached entry"
        );
        assert!(
            after.tags.is_empty(),
            "post-eviction set must be empty, not the dead node's cached tags: {:?}",
            after.tags
        );
    }

    /// PERF_AUDIT §4.2 — `may_execute_batch` must produce
    /// byte-identical verdicts to the per-target `may_execute`
    /// across every realistic shape (target known / unknown,
    /// target carries / doesn't carry tag, allow-lists empty /
    /// populated). The retain-loop callers replaced their per-
    /// candidate `may_execute` calls with `may_execute_batch`,
    /// so any divergence between the two produces silent auth
    /// behavior drift.
    #[test]
    fn may_execute_batch_matches_per_target_may_execute() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        // Three publishers: a target carrying the gated tag with
        // empty allow-lists (permissive), a target carrying it
        // with a populated allow-list, and a target not carrying
        // the tag at all. Plus the caller itself.
        let caller: NodeId = 0xCA;
        let permissive: NodeId = 0xAA;
        let restricted: NodeId = 0xBB;
        let no_tag: NodeId = 0xCC;
        fold.apply(sign_member(&kp, permissive, 0x100, vec!["nrpc:echo"], None))
            .expect("permissive");
        // Restricted: allow only the caller.
        let restricted_ann = SignedAnnouncement::sign(
            &kp,
            super::super::capability::CapabilityFold::KIND_ID,
            0x100,
            restricted,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: 0x100,
                tags: vec!["nrpc:echo".into()],
                hardware: None,
                state: NodeState::Idle,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: vec![caller],
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
                owner: None,
            },
        )
        .expect("sign restricted");
        fold.apply(restricted_ann).expect("restricted apply");
        fold.apply(sign_member(&kp, no_tag, 0x100, vec!["gpu"], None))
            .expect("no-tag apply");
        // Caller's own self-ann so subnet/group derivation has
        // something to walk (it doesn't carry subnet/group tags
        // here — that's OK, derivation returns (None, []) and
        // the allow-list match falls back to the node axis).
        fold.apply(sign_member(&kp, caller, 0x100, vec!["scope:user"], None))
            .expect("caller apply");

        let targets = vec![permissive, restricted, no_tag, 0xDEAD /* unknown */];
        let tag = "nrpc:echo";
        let batch = may_execute_batch(&fold, &targets, tag, caller);
        let per_target: Vec<bool> = targets
            .iter()
            .map(|t| may_execute(&fold, *t, tag, caller))
            .collect();
        assert_eq!(
            batch, per_target,
            "batched verdicts must equal per-target verdicts"
        );
        // Pin the explicit per-target outcomes so a refactor that
        // breaks the verdict semantics fails loudly here, not
        // only via a downstream auth integration test.
        assert_eq!(
            batch,
            vec![true, true, false, false],
            "permissive admits, restricted admits caller via node axis, \
             no-tag denies, unknown denies"
        );
    }

    /// OA-2 §2.4a: `has_local_capability` reports tag presence
    /// only, evaluating NO allow-lists — the red-witnessed point
    /// that the OA-2 admission engine, not the legacy gate, is the
    /// authority for protected services. A restricted target that
    /// `may_execute` would DENY for an unrelated caller still
    /// `has_local_capability` == true.
    #[test]
    fn has_local_capability_ignores_allow_lists() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let restricted: NodeId = 0xBB;
        let no_tag: NodeId = 0xCC;
        let outsider: NodeId = 0xDD;

        // A target carrying the tag but restricted to a specific
        // (different) node — may_execute denies the outsider.
        let restricted_ann = SignedAnnouncement::sign(
            &kp,
            super::super::capability::CapabilityFold::KIND_ID,
            0x100,
            restricted,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: 0x100,
                tags: vec!["nrpc:echo".into()],
                hardware: None,
                state: NodeState::Idle,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: vec![0x1234], // NOT the outsider
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: std::collections::BTreeMap::new(),
                owner: None,
            },
        )
        .expect("sign restricted");
        fold.apply(restricted_ann).expect("apply restricted");
        fold.apply(sign_member(&kp, no_tag, 0x100, vec!["gpu"], None))
            .expect("apply no-tag");

        // may_execute DENIES the outsider (allow-list miss)…
        assert!(!may_execute(&fold, restricted, "nrpc:echo", outsider));
        // …but has_local_capability sees the tag regardless of the
        // allow-list — the exact service IS locally registered.
        assert!(has_local_capability(&fold, restricted, "nrpc:echo"));

        // Absent tag / unknown node / wrong tag are all false.
        assert!(!has_local_capability(&fold, no_tag, "nrpc:echo"));
        assert!(!has_local_capability(&fold, restricted, "nrpc:other"));
        assert!(!has_local_capability(&fold, 0xDEAD, "nrpc:echo"));
    }

    /// PERF_AUDIT §4.2 — empty `targets` slice short-circuits
    /// without taking the fold lock. Pin the zero-allocation
    /// contract: an empty input must return an empty Vec.
    #[test]
    fn may_execute_batch_empty_targets_returns_empty() {
        let fold = new_fold();
        let got = may_execute_batch(&fold, &[], "nrpc:noop", 0xCA);
        assert!(got.is_empty());
    }

    /// PERF_AUDIT §4.2 — exercise the hoisted `derive_caller_axes`
    /// path with REAL `subnet:` / `group:` membership tags. The
    /// node-axis test above never reaches the subnet/group
    /// derivation, so a regression in the once-per-batch hoist
    /// (e.g. deriving from the wrong node, or dropping the parse)
    /// would slip past it. Three restricted targets: subnet-allowed
    /// (admit), group-allowed (admit), foreign-subnet (deny) — and
    /// the batched verdicts must equal the per-target ones.
    #[test]
    fn may_execute_batch_derives_caller_subnet_and_groups_once() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let caller: NodeId = 0xCA;
        let by_subnet: NodeId = 0xA1;
        let by_group: NodeId = 0xA2;
        let foreign: NodeId = 0xA3;

        let caller_subnet =
            super::super::super::subnet::SubnetId::from_tag(&format!("subnet:{}", "11".repeat(16)))
                .expect("parse caller subnet tag");
        let other_subnet =
            super::super::super::subnet::SubnetId::from_tag(&format!("subnet:{}", "22".repeat(16)))
                .expect("parse other subnet tag");
        let caller_group =
            super::super::super::group::GroupId::from_tag(&format!("group:{}", "33".repeat(32)))
                .expect("parse caller group tag");

        // Caller publishes its subnet + group membership tags.
        let caller_subnet_tag = caller_subnet.to_tag();
        let caller_group_tag = caller_group.to_tag();
        fold.apply(sign_member(
            &kp,
            caller,
            0x100,
            vec![
                "scope:user",
                caller_subnet_tag.as_str(),
                caller_group_tag.as_str(),
            ],
            None,
        ))
        .expect("caller apply");

        let restricted =
            |node: NodeId,
             subnets: Vec<super::super::super::subnet::SubnetId>,
             groups: Vec<super::super::super::group::GroupId>| {
                SignedAnnouncement::sign(
                    &kp,
                    super::super::capability::CapabilityFold::KIND_ID,
                    0x100,
                    node,
                    1,
                    EnvelopeMeta::default(),
                    CapabilityMembership {
                        class_hash: 0x100,
                        tags: vec!["nrpc:echo".into()],
                        hardware: None,
                        state: NodeState::Idle,
                        region: None,
                        price_quote: None,
                        reflex_addr: None,
                        allowed_nodes: Vec::new(),
                        allowed_subnets: subnets,
                        allowed_groups: groups,
                        metadata: std::collections::BTreeMap::new(),
                        owner: None,
                    },
                )
                .expect("sign restricted")
            };
        fold.apply(restricted(by_subnet, vec![caller_subnet], Vec::new()))
            .expect("by_subnet apply");
        fold.apply(restricted(by_group, Vec::new(), vec![caller_group]))
            .expect("by_group apply");
        fold.apply(restricted(foreign, vec![other_subnet], Vec::new()))
            .expect("foreign apply");

        let targets = vec![by_subnet, by_group, foreign];
        let tag = "nrpc:echo";
        let batch = may_execute_batch(&fold, &targets, tag, caller);
        let per_target: Vec<bool> = targets
            .iter()
            .map(|t| may_execute(&fold, *t, tag, caller))
            .collect();
        assert_eq!(
            batch, per_target,
            "batched subnet/group verdicts must equal per-target verdicts"
        );
        assert_eq!(
            batch,
            vec![true, true, false],
            "subnet-allowed admits, group-allowed admits, foreign subnet denies"
        );
    }

    // ------------- OA-1: owner-cert ingest verification -------------

    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use crate::adapter::net::behavior::org_revocation::OrgRevocationState;

    fn org_root() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    /// Build a signed announcement the way PRODUCTION dispatch would.
    ///
    /// `node_id` is derived from the keypair rather than taken as a parameter:
    /// real ingest enforces `ann.entity_id.node_id() == ann.node_id`, and
    /// `verify_announced_owner_cert` now refuses a cert that violates it (§12),
    /// because an ownership projection filed under a mismatched node id could
    /// never be retracted. Fixtures that passed an unrelated literal were
    /// building announcements the wire could not carry.
    fn signed_announcement_with_cert(
        kp: &EntityKeypair,
        cert: Option<OrgMembershipCert>,
    ) -> CapabilityAnnouncement {
        let node_id = kp.entity_id().node_id();
        use crate::adapter::net::behavior::capability::CapabilitySet;
        let caps = CapabilitySet::new().add_tag("nrpc:echo".to_string());
        let mut ann = CapabilityAnnouncement::new(node_id, kp.entity_id().clone(), 1, caps)
            .with_owner_cert(cert);
        ann.sign(kp);
        ann
    }

    /// A verified cert projects `owner_org` into the fold; the
    /// entry itself is a normal, queryable membership.
    #[test]
    fn verified_owner_cert_projects_owner_org() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
            .expect("issue");
        let ann = signed_announcement_with_cert(&kp, Some(cert));

        apply_legacy_announcement(&fold, ann).expect("apply");
        assert_eq!(
            owner_org_for(&fold, kp.entity_id().node_id()),
            Some(org_root().org_id())
        );
    }

    /// §12 — a cert on an announcement whose entity does not derive the
    /// announced node id is DROPPED, so no ownership projection can be filed
    /// where retraction would never find it.
    ///
    /// `retract_floored_ownership` locates entries via `member.node_id()`, and
    /// the install sweep and post-apply recheck both search
    /// `by_node[entity.node_id()]`. A projection filed under a mismatched
    /// `ann.node_id` therefore sits in a bucket no retraction path ever
    /// visits: no floor raise, no store install, and no recheck can clear it,
    /// and `owner_org_for` keeps reporting the revoked org forever.
    ///
    /// Production dispatch already enforced the bind, but
    /// `verify_announced_owner_cert` is the single producer of a
    /// `Some(VerifiedOwner)` and two other callers reach it: the
    /// `#[doc(hidden)]` `MeshNode::test_inject_capability_announcement` seam,
    /// which ships in release builds and is re-exported through the Python /
    /// Node / Go bindings, and the `pub` `apply_legacy_announcement` fixture
    /// helper. Enforcing at the producer covers all three.
    ///
    /// The announcement itself is KEPT (OA-1 exit-gate contract: ingest drops
    /// bad certs, not announcements) — only the ownership projection is
    /// refused.
    ///
    /// Red-witness: removing the bind check makes `owner_org_for` return the
    /// org under the mismatched node id.
    #[test]
    fn a_cert_whose_entity_does_not_derive_the_node_id_is_dropped() {
        let kp = EntityKeypair::generate();
        let real_node_id = kp.entity_id().node_id();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
            .expect("issue");

        // Hand-build the announcement production could never emit: a node id
        // unrelated to the announcing entity.
        use crate::adapter::net::behavior::capability::CapabilitySet;
        let mismatched: NodeId = real_node_id ^ 0xFFFF_FFFF;
        assert_ne!(mismatched, real_node_id, "fixture must actually differ");
        let caps = CapabilitySet::new().add_tag("nrpc:echo".to_string());
        let mut ann = CapabilityAnnouncement::new(mismatched, kp.entity_id().clone(), 1, caps)
            .with_owner_cert(Some(cert));
        ann.sign(&kp);

        // The cert is refused even though it is otherwise entirely valid:
        // signed, in-window, member matches the announcing entity, no floors.
        assert_eq!(
            verify_announced_owner_cert(&ann, true, None, 0),
            None,
            "a cert under a mismatched node id must not project ownership",
        );

        // …and the announcement survives: the publisher stays discoverable,
        // just unowned.
        let fold = new_fold();
        apply_legacy_announcement(&fold, ann).expect("apply");
        let filter = LegacyFilter {
            require_tags: vec!["nrpc:echo".into()],
            ..LegacyFilter::default()
        };
        assert!(
            find_nodes_matching(&fold, &filter).contains(&mismatched),
            "the announcement itself must be kept",
        );
        assert_eq!(
            owner_org_for(&fold, mismatched),
            None,
            "no ownership may be projected under the mismatched node id",
        );

        // Positive control: the same cert on a correctly-bound announcement
        // DOES project — so the refusal above is the bind, not the fixture.
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
            .expect("issue");
        let bound = signed_announcement_with_cert(&kp, Some(cert));
        let fold = new_fold();
        apply_legacy_announcement(&fold, bound).expect("apply");
        assert_eq!(
            owner_org_for(&fold, real_node_id),
            Some(org_root().org_id()),
            "a correctly-bound announcement still projects ownership",
        );
    }

    /// OA-1 exit-gate contract: ingest drops bad CERTS, not
    /// announcements. Every failure mode leaves the publisher
    /// discoverable with `owner_org = None`.
    #[test]
    fn bad_owner_cert_is_dropped_but_announcement_is_kept() {
        use crate::adapter::net::identity::EntityId;
        let kp = EntityKeypair::generate();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();

        // (a) member mismatch — a valid cert vouching for someone else.
        let stranger = EntityId::from_bytes([0x77u8; 32]);
        let wrong_member =
            OrgMembershipCert::try_issue(&org_root(), stranger, 1, 3600).expect("issue");
        // (b) tampered signature.
        let mut tampered =
            OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
                .expect("issue");
        tampered.signature[0] ^= 1;
        // (c) expired window.
        let expired = OrgMembershipCert::issue_at(
            &org_root(),
            kp.entity_id().clone(),
            1,
            now - 2000,
            now - 1000,
            7,
        );

        let node_id = kp.entity_id().node_id();
        for (label, cert) in [
            ("member mismatch", wrong_member),
            ("tampered signature", tampered),
            ("expired window", expired),
        ] {
            let fold = new_fold();
            let ann = signed_announcement_with_cert(&kp, Some(cert));
            apply_legacy_announcement(&fold, ann).expect("apply");
            // Announcement kept: the publisher is in the fold and
            // queryable by tag.
            let filter = LegacyFilter {
                require_tags: vec!["nrpc:echo".into()],
                ..LegacyFilter::default()
            };
            assert!(
                find_nodes_matching(&fold, &filter).contains(&node_id),
                "{label}: announcement must be kept"
            );
            // Cert dropped: no ownership projected.
            assert_eq!(
                owner_org_for(&fold, node_id),
                None,
                "{label}: cert must be dropped"
            );
        }
    }

    /// A cert below the node's persisted revocation floor is
    /// dropped at ingest; at or above the floor it projects.
    #[test]
    fn floored_cert_is_dropped_at_ingest() {
        use crate::adapter::net::behavior::org::OrgRevocationBundle;
        let kp = EntityKeypair::generate();

        let mut floors_map = std::collections::BTreeMap::new();
        floors_map.insert(kp.entity_id().clone(), 5u32);
        let bundle = OrgRevocationBundle::try_issue(&org_root(), &floors_map).expect("issue");
        bundle.verify().expect("bundle verifies");
        let mut floors = OrgRevocationState::empty();
        floors.merge_bundle(&bundle);

        let below = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 4, 3600)
            .expect("issue");
        let at = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 5, 3600)
            .expect("issue");

        let ann_below = signed_announcement_with_cert(&kp, Some(below));
        let ann_at = signed_announcement_with_cert(&kp, Some(at));

        assert_eq!(
            verify_announced_owner_cert(&ann_below, true, Some(&floors), 0),
            None,
            "generation below floor must be dropped"
        );
        assert_eq!(
            verify_announced_owner_cert(&ann_at, true, Some(&floors), 0),
            Some(VerifiedOwner::new(org_root().org_id(), 5)),
            "generation at floor must project"
        );
        // No floors tracked (un-adopted node) ⇒ implicit floor 0.
        assert_eq!(
            verify_announced_owner_cert(&ann_below, true, None, 0),
            Some(VerifiedOwner::new(org_root().org_id(), 4)),
            "no floor state ⇒ every generation admissible"
        );
    }

    /// Review-8 §1 witness: a valid replayed membership cert on an
    /// UNSIGNED (or signature-invalid) announcement must never
    /// produce an ownership projection — the announcement itself
    /// may remain discoverable (unsigned-discovery mode), merely
    /// unowned.
    #[test]
    fn unsigned_announcement_never_projects_ownership() {
        use crate::adapter::net::behavior::capability::CapabilitySet;
        let kp = EntityKeypair::generate();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
            .expect("issue");

        // Unsigned outer announcement carrying the valid cert.
        let fold = new_fold();
        let unsigned = CapabilityAnnouncement::new(
            0xE1,
            kp.entity_id().clone(),
            1,
            CapabilitySet::new().add_tag("nrpc:echo"),
        )
        .with_owner_cert(Some(cert.clone()));
        assert!(unsigned.signature.is_none());
        apply_legacy_announcement(&fold, unsigned).expect("apply");
        // Discoverable in unsigned mode…
        let filter = LegacyFilter {
            require_tags: vec!["nrpc:echo".into()],
            ..LegacyFilter::default()
        };
        assert!(find_nodes_matching(&fold, &filter).contains(&0xE1));
        // …but never owned.
        assert_eq!(
            owner_org_for(&fold, 0xE1),
            None,
            "unsigned announcement must not project ownership"
        );

        // Signature-INVALID outer announcement: same refusal.
        let fold = new_fold();
        let mut tampered = signed_announcement_with_cert(&kp, Some(cert));
        tampered.version += 1; // breaks the outer signature
        apply_legacy_announcement(&fold, tampered).expect("apply");
        assert_eq!(
            owner_org_for(&fold, 0xE2),
            None,
            "signature-invalid announcement must not project ownership"
        );
    }

    /// Review-9 race witness, deterministic: a floor rises AFTER
    /// owner-cert verification but BEFORE the fold apply — the
    /// raise's retraction callback completes against a fold that
    /// does not yet hold the projection, the delayed apply then
    /// installs it, and no future callback fires. The production
    /// post-apply recheck must retract it.
    #[test]
    fn delayed_apply_after_floor_raise_still_retracts() {
        use crate::adapter::net::behavior::org::OrgRevocationBundle;
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let node_id = kp.entity_id().node_id();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 4, 3600)
            .expect("issue");
        let ann = signed_announcement_with_cert(&kp, Some(cert));

        // 1. Ingest verifies the cert at floor 0 (no floors yet).
        let owner = verify_announced_owner_cert(&ann, true, None, 0).expect("verifies at floor 0");

        // 2. THE RACE: the floor rises to 5 and its retraction
        //    callback completes — against a fold that does not yet
        //    hold the projection (a no-op).
        let mut floors = OrgRevocationState::empty();
        let mut floors_map = std::collections::BTreeMap::new();
        floors_map.insert(kp.entity_id().clone(), 5u32);
        let bundle = OrgRevocationBundle::try_issue(&org_root(), &floors_map).expect("issue");
        bundle.verify().expect("bundle verifies");
        floors.merge_bundle(&bundle);
        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(
            retracted, 0,
            "callback fires before the apply — nothing to retract"
        );

        // 3. The DELAYED apply installs the stale projection…
        let fold_ann = translate_announcement(&ann, Some(owner));
        fold.apply(fold_ann).expect("apply");
        assert_eq!(
            owner_org_for(&fold, node_id),
            Some(org_root().org_id()),
            "without the recheck the revoked projection would persist — the review-9 red"
        );

        // 4. …and the production post-apply recheck retracts it.
        let retracted = recheck_projected_owner_floor(&fold, Some(&floors), kp.entity_id(), &owner);
        assert_eq!(retracted, 1);
        assert_eq!(
            owner_org_for(&fold, node_id),
            None,
            "final owner must be None"
        );
        // Capability entry remains; verdicts untouched.
        assert!(may_execute(&fold, node_id, "nrpc:echo", 0xCA11));
    }

    /// Review-9: retraction changes query-visible state, so it
    /// advances the fold change generation exactly like an apply;
    /// a no-op retraction does not.
    #[test]
    fn retraction_advances_the_fold_change_generation() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let node_id = kp.entity_id().node_id();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 4, 3600)
            .expect("issue");
        let ann = signed_announcement_with_cert(&kp, Some(cert));
        apply_legacy_announcement(&fold, ann).expect("apply");
        assert_eq!(owner_org_for(&fold, node_id), Some(org_root().org_id()));

        let before = fold.change_generation();
        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(retracted, 1);
        assert!(
            fold.change_generation() > before,
            "retraction must signal fold subscribers"
        );

        // A retraction that clears nothing leaves the generation
        // untouched (no spurious wakeups).
        let before = fold.change_generation();
        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(retracted, 0);
        assert_eq!(fold.change_generation(), before);
    }

    /// §15 — ownership retraction is recorded on the AUDIT plane.
    ///
    /// Every other fold transition (create, replace, evict, expire) emits an
    /// `AuditEvent`. Retraction emitted none, so a deployment with an
    /// installed `FoldAuditSink` logged capability lifecycle faithfully and
    /// was silent on the one security-relevant transition the org feature
    /// produces: a revocation floor rising and stripping a node's proven
    /// ownership. The only trace was a `tracing::info!`, which is not the
    /// audit plane and is not what a compliance consumer reads.
    ///
    /// Red-witness: reverting to `notify_projection_changed` records nothing
    /// and the sink stays empty.
    #[test]
    fn ownership_retraction_is_recorded_on_the_audit_plane() {
        use crate::adapter::net::behavior::fold::audit::VecFoldAuditSink;
        use crate::adapter::net::behavior::fold::AuditKind;

        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let node_id = kp.entity_id().node_id();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 4, 3600)
            .expect("issue");
        let ann = signed_announcement_with_cert(&kp, Some(cert));
        apply_legacy_announcement(&fold, ann).expect("apply");
        assert_eq!(owner_org_for(&fold, node_id), Some(org_root().org_id()));

        // Install the sink AFTER the apply so the only event we can observe is
        // the retraction itself.
        let sink = std::sync::Arc::new(VecFoldAuditSink::new());
        fold.set_audit_sink(Some(sink.clone()));
        assert!(sink.is_empty(), "sink starts clean");

        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(retracted, 1, "the stale projection was retracted");
        assert_eq!(owner_org_for(&fold, node_id), None);

        let events = sink.snapshot();
        assert_eq!(events.len(), 1, "exactly one audit event; got {events:?}");
        assert_eq!(
            events[0].kind,
            AuditKind::Custom("ownership-retracted"),
            "the retraction is its own audit kind",
        );
        let detail = events[0].detail.as_deref().unwrap_or_default();
        assert!(
            detail.contains("floor 5") && detail.contains(&org_root().org_id().to_string()),
            "the detail must name the org and the floor for an auditor; got {detail:?}",
        );

        // A retraction that changes nothing must NOT emit — otherwise the
        // install sweep would flood the audit plane with no-ops.
        let before = sink.len();
        assert_eq!(
            retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 9),
            0,
        );
        assert_eq!(sink.len(), before, "a no-op retraction emits nothing");
    }

    /// Review-8 §9 witness: a rising floor retracts a stale
    /// ownership projection IMMEDIATELY — no re-announcement — while
    /// the capability entry stays present, `may_execute` verdicts
    /// are untouched, and higher-generation projections survive
    /// (the retained generation makes retraction exact).
    #[test]
    fn floor_raise_retracts_stale_ownership_immediately() {
        let fold = new_fold();
        let kp = EntityKeypair::generate();
        let node_id = kp.entity_id().node_id();
        let cert = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 4, 3600)
            .expect("issue");
        let ann = signed_announcement_with_cert(&kp, Some(cert));
        apply_legacy_announcement(&fold, ann).expect("apply");
        assert_eq!(owner_org_for(&fold, node_id), Some(org_root().org_id()));

        // Floor rises to 5: the generation-4 projection retracts.
        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(retracted, 1);
        assert_eq!(
            owner_org_for(&fold, node_id),
            None,
            "stale projection must retract without a re-announcement"
        );
        // The capability entry itself is untouched: still
        // discoverable, still the same permissive verdict.
        let filter = LegacyFilter {
            require_tags: vec!["nrpc:echo".into()],
            ..LegacyFilter::default()
        };
        assert!(find_nodes_matching(&fold, &filter).contains(&node_id));
        assert!(may_execute(&fold, node_id, "nrpc:echo", 0xCA11));

        // A higher-generation projection SURVIVES the same floor.
        let fold = new_fold();
        let cert7 = OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 7, 3600)
            .expect("issue");
        let ann7 = signed_announcement_with_cert(&kp, Some(cert7));
        apply_legacy_announcement(&fold, ann7).expect("apply");
        let retracted = retract_floored_ownership(&fold, org_root().org_id(), kp.entity_id(), 5);
        assert_eq!(retracted, 0, "generation 7 ≥ floor 5 must survive");
        assert_eq!(owner_org_for(&fold, node_id), Some(org_root().org_id()));
    }

    /// Authority-dark pin: `owner_org` never enters `may_execute`.
    /// A cert-bearing permissive announcement admits everyone; a
    /// restricted one denies the same caller — identical verdicts
    /// to the cert-free announcements. (The full exit-gate pin runs
    /// end-to-end in the integration suite.)
    #[test]
    fn owner_org_never_enters_may_execute() {
        let kp = EntityKeypair::generate();
        let caller: NodeId = 0xCA11;
        let cert = || {
            OrgMembershipCert::try_issue(&org_root(), kp.entity_id().clone(), 1, 3600)
                .expect("issue")
        };

        // Permissive (no allow-lists): admitted with or without cert.
        for with_cert in [false, true] {
            let fold = new_fold();
            let ann = signed_announcement_with_cert(&kp, with_cert.then(cert));
            apply_legacy_announcement(&fold, ann).expect("apply");
            assert!(
                may_execute(&fold, kp.entity_id().node_id(), "nrpc:echo", caller),
                "permissive verdict must not depend on owner_org (with_cert={with_cert})"
            );
        }

        // Restricted to a different node: denied with or without cert.
        for with_cert in [false, true] {
            let fold = new_fold();
            let mut ann = signed_announcement_with_cert(&kp, with_cert.then(cert));
            ann.allowed_nodes = vec![0xFFFF];
            ann.sign(&kp);
            apply_legacy_announcement(&fold, ann).expect("apply");
            assert!(
                !may_execute(&fold, kp.entity_id().node_id(), "nrpc:echo", caller),
                "restricted verdict must not depend on owner_org (with_cert={with_cert})"
            );
        }
    }
}
