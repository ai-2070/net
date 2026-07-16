//! SI-2b — the Layer-1 candidate snapshot over the REAL planes
//! (SENSING_INTEREST_COALESCING_PLAN v4.3, §4.7/§4.10).
//!
//! [`resolve_candidates`](super::controller::resolve_candidates)
//! computes `fold ∩ selector ∩ authority ∩ reachability`,
//! proximity-ranked — but SI-2a fed it an EMPTY snapshot. This
//! module builds the real one, split along a deliberate seam:
//!
//! - [`extract_declarers`] reads the capability fold in **one**
//!   `with_state` pass (the `capability_tags_for_all` lock
//!   discipline — never `1 + N` acquisitions) and resolves each
//!   declarer's TOFU-pinned entity root AFTER the fold lock is
//!   released;
//! - [`build_candidate_snapshot`] is a pure assembly over those
//!   declarers plus two injected planes (a route estimator and a
//!   reachability predicate), so the policy — §4.10 authorization,
//!   tag provenance, the ranking inputs — unit-tests without a
//!   node. `MeshNode::sensing_candidate_snapshot` is a thin adapter
//!   over both.
//!
//! ## What "declares the capability" means (v1)
//!
//! A fold node declares capability `Y` iff at least one of its
//! folded capability entries carries
//!
//! - a tag whose wire form is EXACTLY `Y`'s name (the plain/legacy
//!   tag shape — `"print.document"` announced as-is), or
//! - a `software.tool.<id>.tool_id=<Y>` axis tag — the same tool
//!   bucket the fold's synthetic `tool:` index is derived from
//!   (`derive_synthetic_index_tags`), so a tool-capability
//!   announcement and a `tool:`-indexed query agree on who
//!   provides `Y`.
//!
//! This is the documented v1 tag/name structural match; constraint
//! (`C`) evaluation stays with the provider-side evaluator (§3.3)
//! and richer class-hash matching is a later slice.
//!
//! ## Authority and provenance (§4.10 v1)
//!
//! `authorized` is `declarer's pinned entity root == the local
//! sensing owner root` — the same TOFU pin (`peer_entity_ids`) the
//! dispatch arm derives session roots from, so the candidate plane
//! and the frame-intake plane cannot disagree about who the owner
//! is. Tag assertions carry `asserted_by = the DECLARER's pinned
//! root` (the announcer is the asserter); owner-authored therefore
//! means `asserted_by == owner_root`, which is exactly the
//! provenance the resolver's `Tags` filter accepts — a foreign or
//! unpinned provider cannot enter a candidate set by self-labeling
//! an authority-implying tag. An unpinned declarer contributes NO
//! assertions at all (nothing provable to attribute).
//!
//! `groups` is empty in SI-2b: the owner-scoped `GroupRef`
//! membership fold surface is a later slice, so `Group` selectors
//! resolve to no candidates yet (fail-closed, never fail-open).

use std::collections::BTreeSet;
use std::time::Duration;

use super::controller::{CandidateProvider, TagAssertion};
use super::identity::{AudienceScopeCommitment, CapabilityId};
use crate::adapter::net::behavior::fold::{CapabilityFold, Fold};
use crate::adapter::net::behavior::proximity::ProximityGraph;
use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

/// One capability declarer as read out of the fold — the
/// intermediate between [`extract_declarers`]' single-lock fold
/// pass and [`build_candidate_snapshot`]'s pure assembly.
#[derive(Clone, Debug)]
pub struct DeclaredProvider {
    /// Declarer node id.
    pub node_id: u64,
    /// The declarer's announce generation — the capability
    /// announcement `version` the fold stored — maximized across
    /// its DECLARING entries when it declares in several classes.
    pub capability_generation: u64,
    /// The declarer's folded tag union (raw wire-form strings,
    /// across ALL of its class entries — the
    /// `capability_tags_for` read shape), sorted for determinism.
    pub tags: Vec<String>,
    /// The declarer's TOFU-pinned entity root (§4.10). `None` when
    /// no pin exists — an unpinned declarer can never be
    /// authorized and asserts nothing.
    pub entity_root: Option<AudienceScopeCommitment>,
}

/// Whether a folded tag set declares `capability_id` under the v1
/// structural match (module doc: exact wire-form name equality, or
/// the `software.tool.<id>.tool_id=<name>` tool bucket).
pub fn declares_capability(tags: &[String], capability_id: &CapabilityId) -> bool {
    let name = capability_id.as_str();
    tags.iter().any(|tag| {
        if tag.as_str() == name {
            return true;
        }
        let Ok(Tag::AxisValue {
            axis: TaxonomyAxis::Software,
            key,
            value,
            ..
        }) = Tag::parse(tag)
        else {
            return false;
        };
        value == name
            && key
                .strip_prefix("tool.")
                .is_some_and(|rest| matches!(rest.split_once('.'), Some((_, "tool_id"))))
    })
}

/// Extract every declarer of `capability_id` from the capability
/// fold in **one** `with_state` pass (fold-lock discipline: the
/// `capability_tags_for_all` shape — one read-lock acquisition,
/// never `1 + N`). Expired-but-unswept entries are read as-is,
/// exactly like every other fold read (expiry is the sweeper's
/// job).
///
/// `entity_root_of` resolves a declarer's TOFU-pinned entity root
/// and is deliberately called AFTER the fold lock is released, so
/// no foreign lock (e.g. the `peer_entity_ids` map) ever nests
/// inside the fold's.
///
/// The result is sorted by `node_id` so downstream ranking ties
/// break deterministically.
pub fn extract_declarers<F>(
    fold: &Fold<CapabilityFold>,
    capability_id: &CapabilityId,
    entity_root_of: F,
) -> Vec<DeclaredProvider>
where
    F: Fn(u64) -> Option<AudienceScopeCommitment>,
{
    // ONE pass under the state read lock: node id, max declaring
    // generation, full tag union. Nothing else happens inside.
    let mut raw: Vec<(u64, u64, Vec<String>)> = fold.with_state(|state| {
        let mut out = Vec::new();
        for (node_id, keys) in &state.by_node {
            let mut generation: Option<u64> = None;
            let mut tags: BTreeSet<String> = BTreeSet::new();
            for key in keys {
                let Some(entry) = state.entries.get(key) else {
                    continue;
                };
                if declares_capability(&entry.payload.tags, capability_id) {
                    generation =
                        Some(generation.map_or(entry.generation, |g| g.max(entry.generation)));
                }
                tags.extend(entry.payload.tags.iter().cloned());
            }
            if let Some(generation) = generation {
                out.push((*node_id, generation, tags.into_iter().collect()));
            }
        }
        out
    });
    raw.sort_unstable_by_key(|(node_id, ..)| *node_id);
    raw.into_iter()
        .map(|(node_id, capability_generation, tags)| DeclaredProvider {
            node_id,
            capability_generation,
            tags,
            entity_root: entity_root_of(node_id),
        })
        .collect()
}

/// Per-hop charge when the proximity plane knows a path exists but
/// holds no measured latency for it — rung 2 and 3 of the
/// [`proximity_route_estimate`] ladder.
pub const HOP_FALLBACK_ESTIMATE: Duration = Duration::from_millis(50);

/// Conservative default when the proximity plane knows NOTHING
/// about a candidate (no edge, no path): large enough that any
/// candidate with real proximity evidence outranks it, small
/// enough that a lone unranked candidate still passes a sane
/// consumer budget.
pub const UNKNOWN_ROUTE_ESTIMATE: Duration = Duration::from_secs(1);

/// This node's route estimate toward `node_id` off the proximity
/// graph — the §4.7 ranking key. The fallback ladder:
///
/// 1. self → `Duration::ZERO`;
/// 2. a MEASURED direct edge in either orientation
///    ([`ProximityGraph::edge_latency`], undirected view) — the
///    minimum of the non-zero readings;
/// 3. a direct edge that exists but reads zero (the session-setup
///    placeholder — present, unmeasured) → one
///    [`HOP_FALLBACK_ESTIMATE`], so an unmeasured edge never
///    outranks a measured near-zero one;
/// 4. a BFS path ([`ProximityGraph::path_to`]) →
///    `hops × HOP_FALLBACK_ESTIMATE`;
/// 5. nothing known → [`UNKNOWN_ROUTE_ESTIMATE`].
pub fn proximity_route_estimate(graph: &ProximityGraph, node_id: u64) -> Duration {
    let local = graph.my_id();
    let target = graph_node_id(node_id);
    if target == local {
        return Duration::ZERO;
    }
    let out = graph.edge_latency(local, target);
    let back = graph.edge_latency(target, local);
    if let Some(measured) = [out, back]
        .into_iter()
        .flatten()
        .filter(|d| !d.is_zero())
        .min()
    {
        return measured;
    }
    if out.is_some() || back.is_some() {
        return HOP_FALLBACK_ESTIMATE;
    }
    match graph.path_to(&target) {
        Some(path) if path.len() > 1 => {
            HOP_FALLBACK_ESTIMATE.saturating_mul((path.len() - 1) as u32)
        }
        _ => UNKNOWN_ROUTE_ESTIMATE,
    }
}

/// The proximity graph keys nodes by the u64 node id zero-padded
/// to 32 bytes (mesh.rs `node_id_to_graph_id` — every peer edge is
/// seeded in this encoding).
fn graph_node_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// Pure snapshot assembly (plan §4.7/§4.10): map fold declarers
/// plus the two injected planes into the resolver's
/// [`CandidateProvider`] rows. NO filtering happens here — the
/// resolver ([`resolve_candidates`](super::controller::resolve_candidates))
/// is the single point that applies `authorized ∧ reachable ∧
/// selector`; the snapshot only reports the facts:
///
/// - `authorized` — the declarer's pinned entity root equals the
///   local sensing owner root (§4.10 v1 owner-root rule; unpinned
///   ⇒ unauthorized);
/// - `reachable` — `reachable(node_id)` (routing-table lookup OR a
///   live direct session at the adapter);
/// - `route_estimate` — `route_estimate(node_id)` (the
///   [`proximity_route_estimate`] ladder at the adapter);
/// - `tags` — the declarer's folded tags as [`TagAssertion`]s with
///   `asserted_by = the declarer's own pinned root` (the announcer
///   is the asserter). The v1 wire-form mapping: `key=value` tags
///   split at the first `=`; every other shape is a presence
///   assertion with the empty-string value. Unpinned declarers
///   assert nothing;
/// - `groups` — empty (the `GroupRef` fold surface is a later
///   slice; `Group` selectors thus resolve fail-closed).
pub fn build_candidate_snapshot<F, G>(
    declarers: &[DeclaredProvider],
    local_owner_root: &AudienceScopeCommitment,
    route_estimate: F,
    reachable: G,
) -> Vec<CandidateProvider>
where
    F: Fn(u64) -> Duration,
    G: Fn(u64) -> bool,
{
    declarers
        .iter()
        .map(|declarer| CandidateProvider {
            node_id: declarer.node_id,
            capability_generation: declarer.capability_generation,
            authorized: declarer.entity_root.as_ref() == Some(local_owner_root),
            reachable: reachable(declarer.node_id),
            route_estimate: route_estimate(declarer.node_id),
            tags: match declarer.entity_root {
                Some(asserted_by) => declarer
                    .tags
                    .iter()
                    .map(|tag| tag_assertion(tag, asserted_by))
                    .collect(),
                None => Vec::new(),
            },
            groups: Vec::new(),
        })
        .collect()
}

/// v1 wire-form → assertion mapping (see
/// [`build_candidate_snapshot`]): `key=value` splits at the first
/// `=`; anything else is a presence assertion (empty value).
fn tag_assertion(tag: &str, asserted_by: AudienceScopeCommitment) -> TagAssertion {
    match tag.split_once('=') {
        Some((key, value)) => TagAssertion {
            key: key.to_string(),
            value: value.to_string(),
            asserted_by,
        },
        None => TagAssertion {
            key: tag.to_string(),
            value: String::new(),
            asserted_by,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::controller::{resolve_candidates, CandidatePolicy};
    use super::super::identity::{ProviderSelector, ResultMode, TagMatch};
    use super::*;
    use crate::adapter::net::behavior::fold::{
        CapabilityMembership, EnvelopeMeta, FoldKind, NodeState, SignedAnnouncement,
    };
    use crate::adapter::net::behavior::proximity::ProximityConfig;
    use crate::adapter::net::identity::EntityKeypair;

    fn root(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn declarer(
        node_id: u64,
        generation: u64,
        tags: &[&str],
        entity_root: Option<AudienceScopeCommitment>,
    ) -> DeclaredProvider {
        DeclaredProvider {
            node_id,
            capability_generation: generation,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            entity_root,
        }
    }

    fn cap() -> CapabilityId {
        CapabilityId::new("print.document")
    }

    #[test]
    fn declares_capability_matches_name_and_tool_bucket() {
        let by_name = vec!["print.document".to_string()];
        let by_tool = vec!["software.tool.print.tool_id=print.document".to_string()];
        let neither = vec![
            "print.documentx".to_string(),
            "software.tool.print.name=print.document".to_string(),
            "software.model.p.id=print.document".to_string(),
            "calibrated=true".to_string(),
        ];
        assert!(declares_capability(&by_name, &cap()));
        assert!(declares_capability(&by_tool, &cap()));
        assert!(!declares_capability(&neither, &cap()));
        assert!(!declares_capability(&[], &cap()));
    }

    #[test]
    fn authorization_follows_the_pinned_root() {
        let owner = root(0xAA);
        let foreign = root(0xBB);
        let declarers = [
            declarer(1, 3, &["print.document"], Some(owner)),
            declarer(2, 5, &["print.document"], Some(foreign)),
            declarer(3, 1, &["print.document"], None),
        ];
        let snapshot =
            build_candidate_snapshot(&declarers, &owner, |_| Duration::from_millis(1), |_| true);
        assert_eq!(snapshot.len(), 3, "no pre-filtering in the snapshot");
        assert!(snapshot[0].authorized, "owner-root pin is authorized");
        assert!(!snapshot[1].authorized, "foreign pin is not");
        assert!(!snapshot[2].authorized, "unpinned is not");
        assert!(
            snapshot[2].tags.is_empty(),
            "an unpinned declarer asserts nothing"
        );
        // The resolver — the single filter point — admits exactly
        // the owner-root candidate.
        let resolved = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Each,
            &snapshot,
            &owner,
            &CandidatePolicy::default(),
        )
        .expect("Each under the cap");
        assert_eq!(resolved.active, vec![1]);
    }

    #[test]
    fn reachability_and_ranking_inputs_feed_the_resolver() {
        let owner = root(0xAA);
        let declarers = [
            declarer(1, 1, &["print.document"], Some(owner)),
            declarer(2, 1, &["print.document"], Some(owner)),
            declarer(3, 1, &["print.document"], Some(owner)),
        ];
        // Node 2 is closest but unreachable; 3 beats 1 on the
        // estimate.
        let estimate = |node: u64| {
            Duration::from_millis(match node {
                1 => 40,
                2 => 1,
                _ => 7,
            })
        };
        let snapshot = build_candidate_snapshot(&declarers, &owner, estimate, |node| node != 2);
        assert!(!snapshot[1].reachable);
        assert_eq!(snapshot[0].route_estimate, Duration::from_millis(40));
        assert_eq!(snapshot[2].route_estimate, Duration::from_millis(7));
        let resolved = resolve_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Any,
            &snapshot,
            &owner,
            &CandidatePolicy::default(),
        )
        .expect("Any never refuses");
        assert_eq!(resolved.active, vec![3], "closest reachable wins");
        assert_eq!(resolved.standby, vec![1], "unreachable never a standby");
    }

    #[test]
    fn provenance_maps_wire_tags_and_gates_tag_selectors() {
        let owner = root(0xAA);
        let foreign = root(0xBB);
        let declarers = [
            declarer(
                1,
                1,
                &["print.document", "calibrated=true", "hardware.gpu"],
                Some(owner),
            ),
            // Same self-labeled tag under a foreign root: must
            // never satisfy an owner-root Tags selector (§4.10).
            declarer(2, 1, &["print.document", "calibrated=true"], Some(foreign)),
        ];
        let snapshot =
            build_candidate_snapshot(&declarers, &owner, |_| Duration::from_millis(1), |_| true);
        let calibrated = snapshot[0]
            .tags
            .iter()
            .find(|t| t.key == "calibrated")
            .expect("key=value split");
        assert_eq!(calibrated.value, "true");
        assert_eq!(calibrated.asserted_by, owner, "the announcer asserts");
        let presence = snapshot[0]
            .tags
            .iter()
            .find(|t| t.key == "hardware.gpu")
            .expect("presence tag kept");
        assert_eq!(presence.value, "", "presence asserts the empty value");
        assert!(
            snapshot[1].tags.iter().all(|t| t.asserted_by == foreign),
            "provenance is the declarer's own root, never the owner's"
        );
        assert!(
            snapshot.iter().all(|c| c.groups.is_empty()),
            "SI-2b: no groups"
        );

        let selector = ProviderSelector::tags(vec![TagMatch {
            key: "calibrated".into(),
            value: "true".into(),
        }]);
        let resolved = resolve_candidates(
            &selector,
            ResultMode::Each,
            &snapshot,
            &owner,
            &CandidatePolicy::default(),
        )
        .expect("Each under the cap");
        assert_eq!(
            resolved.active,
            vec![1],
            "self-labeling under a foreign root never enters the set"
        );
    }

    fn announce(
        keypair: &EntityKeypair,
        publisher: u64,
        generation: u64,
        class: u64,
        tags: &[&str],
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
                tags: tags.iter().map(|t| t.to_string()).collect(),
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
        .expect("sign succeeds")
    }

    #[test]
    fn extract_declarers_is_one_pass_and_deterministic() {
        let fold: Fold<CapabilityFold> = Fold::with_sweep_interval(Duration::ZERO);
        let kp = EntityKeypair::generate();
        // Node 0xB declares in TWO classes (generations 2 and 7)
        // and carries an extra non-declaring entry whose tags must
        // still union in.
        fold.apply(announce(&kp, 0xB, 2, 1, &["print.document"]))
            .expect("apply");
        fold.apply(announce(
            &kp,
            0xB,
            7,
            2,
            &["print.document", "calibrated=true"],
        ))
        .expect("apply");
        fold.apply(announce(&kp, 0xB, 9, 3, &["mesh.relay"]))
            .expect("apply");
        // Node 0xA declares via the tool bucket; node 0xC never
        // declares.
        fold.apply(announce(
            &kp,
            0xA,
            4,
            1,
            &["software.tool.print.tool_id=print.document"],
        ))
        .expect("apply");
        fold.apply(announce(&kp, 0xC, 1, 1, &["other.capability"]))
            .expect("apply");

        let owner = root(0xAA);
        let declarers = extract_declarers(&fold, &cap(), |node| (node == 0xB).then_some(owner));
        assert_eq!(declarers.len(), 2);
        assert_eq!(declarers[0].node_id, 0xA, "sorted by node id");
        assert_eq!(declarers[1].node_id, 0xB);
        assert_eq!(
            declarers[1].capability_generation, 7,
            "max generation across DECLARING entries only"
        );
        assert!(
            declarers[1].tags.iter().any(|t| t == "mesh.relay"),
            "tag union spans all of the declarer's entries"
        );
        assert_eq!(declarers[0].entity_root, None);
        assert_eq!(declarers[1].entity_root, Some(owner));
    }

    #[test]
    fn route_estimate_ladder() {
        let me = graph_node_id(0x1);
        let graph = ProximityGraph::new(me, ProximityConfig::default());
        // Rung 1: self.
        assert_eq!(proximity_route_estimate(&graph, 0x1), Duration::ZERO);
        // Rung 5: nothing known.
        assert_eq!(
            proximity_route_estimate(&graph, 0x99),
            UNKNOWN_ROUTE_ESTIMATE
        );
        // Rung 2: measured edge, undirected minimum.
        graph.test_insert_edge(me, graph_node_id(0x2), 9_000);
        graph.test_insert_edge(graph_node_id(0x2), me, 5_000);
        assert_eq!(
            proximity_route_estimate(&graph, 0x2),
            Duration::from_micros(5_000)
        );
        // Rung 3: an edge that exists but is unmeasured (the
        // session-setup zero placeholder).
        graph.test_insert_edge(me, graph_node_id(0x3), 0);
        assert_eq!(proximity_route_estimate(&graph, 0x3), HOP_FALLBACK_ESTIMATE);
        // Rung 3 does not mask a real reading on the other
        // orientation.
        graph.test_insert_edge(graph_node_id(0x3), me, 2_000);
        assert_eq!(
            proximity_route_estimate(&graph, 0x3),
            Duration::from_micros(2_000)
        );
        // Rung 4: no direct edge, but a two-hop BFS path via 0x2.
        graph.test_insert_edge(graph_node_id(0x2), graph_node_id(0x4), 1_000);
        assert_eq!(
            proximity_route_estimate(&graph, 0x4),
            HOP_FALLBACK_ESTIMATE.saturating_mul(2)
        );
    }
}
