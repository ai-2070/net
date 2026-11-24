//! Owner-private candidate projection for the provider-free org leader (LS-1).
//!
//! The legacy leader resolves a provider-free interest against the PLAINTEXT
//! capability fold under the §4.10 entity-root gate. For an organization interest
//! that is the wrong universe in both directions (leader plan §1): owner-private
//! providers are invisible, and public/foreign declarers are admissible. This
//! module builds the right universe — the floor-current owner-scope records of
//! the indexed private-discovery store — in two steps that never overlap:
//!
//! 1. [`capture_org_candidate_bases`] runs UNDER the scoped-state lock and does
//!    only an indexed bucket lookup, the fresh expiry/floor filter the store
//!    already applies, and a clone of a few owned fields. No descriptor decode,
//!    no topology read, no sort, no allocation of candidate objects.
//! 2. [`project_org_candidates`] runs AFTER the lock is released and joins each
//!    base row with this node's topology view (pin equality, direct sessions,
//!    the routing table and the proximity estimate) once, producing ONE
//!    route-ranked [`OrgCapabilityCandidates`] shared by every selector and
//!    result mode for the capability.
//!
//! The projection is owner-private state (plan §3 "Confidentiality invariant"):
//! it feeds only org leader interests whose admitted seed names the same owner
//! org, never the legacy snapshot, the plaintext fold, OLB route sets, or any
//! query surface.
//!
//! Field semantics are the plan's §3 table:
//! - `node_id` is the authenticated derivation `EntityId::node_id()` (§4) — the
//!   reverse direction is never inferred from an address or a bare node id;
//! - `authorized` is `true` by construction: only ingest-verified, floor-current
//!   owner-scope members of THIS node's owner org enter;
//! - `reachable` is a direct session whose pin EQUALS the record's provider, or
//!   a routing-table hit for the derived node id. A pin naming a DIFFERENT entity
//!   for that node id fails closed — it never counts as reachability of either
//!   kind, because the session the route would ride belongs to someone else;
//! - `tags` / `groups` are empty in org v1 (Q6): a provider-signed descriptor is
//!   not an organization assertion, and nothing here relabels it as one;
//! - the local node is never a candidate (Q2, D-LS3).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::controller::{
    bound_resolution, CandidatePolicy, CandidateProvider, ResolutionRefusal, ResolvedCandidates,
};
use super::identity::{CapabilityId, ProviderSelector, ResultMode};
use super::org_gate::SensingAuthorityStamp;
use crate::adapter::net::behavior::org::OrgId;
use crate::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use crate::adapter::net::behavior::org_revocation::OrgRevocationState;
use crate::adapter::net::behavior::org_scoped_store::ScopedDiscoveryState;
use crate::adapter::net::identity::EntityId;

/// The most live organization leader interests one node holds (D-LS4). A new
/// interest over the cap is refused before any projection or cache allocation;
/// it also bounds the candidate cache, since distinct demanded capabilities can
/// never exceed live interests.
pub(crate) const MAX_ORG_LEADER_INTERESTS: usize = 512;

/// The storage-level facts one owner-private record contributes, captured under
/// the scoped-state lock. Owned, so the lock is never held across the topology
/// join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrgCandidateBase {
    /// The verified provider entity.
    pub provider: EntityId,
    /// `provider.node_id()` — the authenticated derivation (§4).
    pub node_id: u64,
    /// The scoped announcement generation.
    pub generation: u64,
    /// The record's effective expiry (unix seconds).
    pub expires_at: u64,
}

/// The sensing-capability → scoped-index key mapping (F1): the index keys owner
/// buckets by the canonical capability tag's authority id, exactly the rule the
/// org gate applies to an interest's capability.
pub(crate) fn capability_authority_id(capability: &CapabilityId) -> CapabilityAuthorityId {
    CapabilityAuthorityId::for_tag(capability.as_str())
}

/// Step 1 of the projection: the floor-current owner-scope records of
/// `owner_org` declaring `capability`, minus the local node.
///
/// Runs under the caller's scoped-state lock and performs ONE indexed bucket
/// lookup (`find_owner_private_providers` with `Some(capability)`), which already
/// applies the fresh expiry and revocation-floor filter. The `owner_org` filter
/// is defensive: a node holds one owner partition (`AlreadyOwned`), but the
/// projection asserts it per record rather than trusting that.
pub(crate) fn capture_org_candidate_bases(
    state: &ScopedDiscoveryState,
    capability: &CapabilityId,
    owner_org: &OrgId,
    now_secs: u64,
    floors: &OrgRevocationState,
    local_node_id: u64,
) -> Vec<OrgCandidateBase> {
    let key = capability_authority_id(capability);
    state
        .find_owner_private_providers(Some(&key), now_secs, floors)
        .into_iter()
        .filter(|(record, _)| record.owner_org == *owner_org)
        .filter_map(|(record, provider)| {
            let node_id = provider.node_id();
            (node_id != local_node_id).then_some(OrgCandidateBase {
                provider,
                node_id,
                generation: record.generation,
                expires_at: record.expires_at,
            })
        })
        .collect()
}

/// The topology facts the off-lock join needs. Implemented by the mesh over its
/// live maps, and by fixtures in tests, so the projection itself stays pure.
pub(crate) trait OrgTopology {
    /// The EntityId pinned for `node_id`'s session, if any.
    fn pinned_entity(&self, node_id: u64) -> Option<EntityId>;
    /// Whether this node holds a direct session to `node_id`.
    fn has_direct_session(&self, node_id: u64) -> bool;
    /// Whether the routing table reaches `node_id`.
    fn routed(&self, node_id: u64) -> bool;
    /// The proximity-plane route estimate toward `node_id`.
    fn route_estimate(&self, node_id: u64) -> Duration;
}

/// One projected candidate: the resolver-facing [`CandidateProvider`] plus the
/// scoped identity it was derived from, retained so a branch can check its
/// record's deadline in O(1) (plan §6 "Scheduler latency cannot extend
/// authority").
#[derive(Clone, Debug)]
pub(crate) struct OrgRankedCandidate {
    /// The resolver-facing view.
    pub candidate: CandidateProvider,
    /// The verified provider entity behind `candidate.node_id`.
    pub provider: EntityId,
    /// The scoped record's expiry (unix seconds).
    pub expires_at: u64,
}

/// ONE route-ranked owner-private candidate set for one capability (plan §3
/// Stage C). Computed once per source vector and shared by every org interest
/// on the capability: selectors filter it in order and never re-sort it.
#[derive(Debug, Default)]
pub(crate) struct OrgCapabilityCandidates {
    ranked: Arc<[OrgRankedCandidate]>,
    by_node: HashMap<u64, usize>,
    next_expiry: Option<u64>,
}

impl OrgCapabilityCandidates {
    /// The candidates in deterministic `(route_estimate, node_id)` order (D-LS2).
    pub(crate) fn ranked(&self) -> &[OrgRankedCandidate] {
        &self.ranked
    }

    /// The candidate for `node_id`, in O(1) — the explicit `Node` selector's
    /// membership test.
    pub(crate) fn get(&self, node_id: u64) -> Option<&OrgRankedCandidate> {
        self.by_node.get(&node_id).map(|&index| &self.ranked[index])
    }

    /// The earliest record expiry in the set, if any.
    pub(crate) fn next_expiry(&self) -> Option<u64> {
        self.next_expiry
    }

    /// Whether every record in the set is still unexpired at `now_secs`. A set
    /// past its earliest deadline must not be served: an expired record would
    /// keep a branch alive past its authority.
    pub(crate) fn is_live_at(&self, now_secs: u64) -> bool {
        self.next_expiry.is_none_or(|deadline| now_secs < deadline)
    }

    /// Number of candidates (reachable or not).
    pub(crate) fn len(&self) -> usize {
        self.ranked.len()
    }

    /// Whether the set is empty.
    #[allow(dead_code)] // the reconciler's empty-set fast path (LS-4)
    pub(crate) fn is_empty(&self) -> bool {
        self.ranked.is_empty()
    }
}

/// Step 2 of the projection: join the captured base rows with `topology`, ONCE,
/// into the shared route-ranked set. Runs with no scoped-state or leader lock
/// held.
pub(crate) fn project_org_candidates(
    bases: Vec<OrgCandidateBase>,
    topology: &impl OrgTopology,
) -> OrgCapabilityCandidates {
    let mut ranked: Vec<OrgRankedCandidate> = bases
        .into_iter()
        .map(|base| {
            let pin = topology.pinned_entity(base.node_id);
            let reachable = match &pin {
                // A pin for this node id that names a DIFFERENT entity: the
                // session (and any route riding it) belongs to someone else.
                Some(pinned) if *pinned != base.provider => false,
                Some(_) => {
                    topology.has_direct_session(base.node_id) || topology.routed(base.node_id)
                }
                None => topology.routed(base.node_id),
            };
            OrgRankedCandidate {
                candidate: CandidateProvider {
                    node_id: base.node_id,
                    capability_generation: base.generation,
                    authorized: true,
                    reachable,
                    route_estimate: topology.route_estimate(base.node_id),
                    tags: Vec::new(),
                    groups: Vec::new(),
                },
                provider: base.provider,
                expires_at: base.expires_at,
            }
        })
        .collect();
    ranked.sort_by_key(|entry| (entry.candidate.route_estimate, entry.candidate.node_id));
    // Two records for one node id would be two entities deriving one id — the
    // collision the mesh's node-id derivation assumes away. Keep the first (the
    // better-ranked) and drop the rest rather than let an index point at either.
    let mut by_node = HashMap::with_capacity(ranked.len());
    let mut deduped = Vec::with_capacity(ranked.len());
    for entry in ranked {
        if by_node.contains_key(&entry.candidate.node_id) {
            continue;
        }
        by_node.insert(entry.candidate.node_id, deduped.len());
        deduped.push(entry);
    }
    let next_expiry = deduped.iter().map(|entry| entry.expires_at).min();
    OrgCapabilityCandidates {
        ranked: deduped.into(),
        by_node,
        next_expiry,
    }
}

/// Resolve an ORGANIZATION-admitted interest against its owner-private
/// projection (plan §5) — the only resolver an org seed ever reaches, at
/// intake and in standing reconciliation alike (Q1). Legacy admissions keep
/// [`resolve_candidates`](super::controller::resolve_candidates) byte-for-byte.
///
/// - `AnyAuthorized`: every reachable projected member, in projection order.
/// - `Node(id)` / `Nodes(ids)`: INTERSECTED with the projection. Unlike the
///   legacy resolver there is no `Node` short-circuit: a named id outside the
///   projection opens NO branch. No new refusal distinguishes "not authorized"
///   from "not yet discovered", "expired" or "unreachable" — that would be an
///   existence oracle — so the empty result takes the ordinary
///   `AllBranchesRefused` path at the leader.
/// - `Tags` / `Group`: resolve EMPTY in org v1 (Q6). A provider-signed
///   descriptor tag is not an organization assertion, and the projection
///   carries none.
///
/// The projection is already ranked (D-LS2), so this filters in order and never
/// sorts. The result-mode bounds are the legacy resolver's, shared through
/// [`bound_resolution`](super::controller::bound_resolution).
pub(crate) fn resolve_org_candidates(
    selector: &ProviderSelector,
    result_mode: ResultMode,
    candidates: &OrgCapabilityCandidates,
    policy: &CandidatePolicy,
) -> Result<ResolvedCandidates, ResolutionRefusal> {
    let eligible =
        |entry: &&OrgRankedCandidate| entry.candidate.authorized && entry.candidate.reachable;
    let node = |entry: &OrgRankedCandidate| entry.candidate.node_id;
    let matched: Vec<u64> = match selector {
        ProviderSelector::AnyAuthorized => candidates
            .ranked()
            .iter()
            .filter(eligible)
            .map(node)
            .collect(),
        ProviderSelector::Node(id) => candidates
            .get(*id)
            .filter(eligible)
            .map(node)
            .into_iter()
            .collect(),
        ProviderSelector::Nodes(ids) => candidates
            .ranked()
            .iter()
            .filter(eligible)
            .filter(|entry| ids.contains(&entry.candidate.node_id))
            .map(node)
            .collect(),
        ProviderSelector::Tags(_) | ProviderSelector::Group(_) => Vec::new(),
    };
    bound_resolution(&matched, result_mode, policy)
}

#[cfg(test)]
impl OrgCapabilityCandidates {
    /// Test-only: a projection of reachable, authorized members at the given
    /// node ids and route estimates — for leader witnesses that exercise the
    /// org path without building a scoped store.
    pub(crate) fn for_test(rows: &[(u64, Duration)]) -> Self {
        let bases = rows
            .iter()
            .map(|&(node_id, _)| OrgCandidateBase {
                provider: EntityId::from_bytes([(node_id & 0xFF) as u8; 32]),
                node_id,
                generation: 1,
                expires_at: u64::MAX,
            })
            .collect();
        struct AllRouted(HashMap<u64, Duration>);
        impl OrgTopology for AllRouted {
            fn pinned_entity(&self, _: u64) -> Option<EntityId> {
                None
            }
            fn has_direct_session(&self, _: u64) -> bool {
                false
            }
            fn routed(&self, _: u64) -> bool {
                true
            }
            fn route_estimate(&self, node_id: u64) -> Duration {
                self.0[&node_id]
            }
        }
        project_org_candidates(bases, &AllRouted(rows.iter().copied().collect()))
    }
}

/// The org leader's operational counters (plan §6 "Required metrics"). Shared
/// by the intake transaction and the node's reconciler; read through
/// [`OrgLeaderCounters::snapshot`].
#[derive(Default, Debug)]
pub(crate) struct OrgLeaderCounters {
    /// A new org interest refused at the D-LS4 cap, before any projection.
    pub interest_over_cap: std::sync::atomic::AtomicU64,
    /// Candidate projections built (cold intake + reconciliation misses).
    pub projection_builds: std::sync::atomic::AtomicU64,
    /// Reconciliation projections served from the bounded cache.
    pub projection_cache_hits: std::sync::atomic::AtomicU64,
    /// Plans (or plan entries) discarded because their source moved before
    /// apply: a stale authority stamp at the fence, or a moved interest version.
    pub projection_discarded: std::sync::atomic::AtomicU64,
    /// Reconciliation passes run by the node's reconciler.
    pub reconcile_passes: std::sync::atomic::AtomicU64,
    /// Apply batches (at most 64 interests each).
    pub reconcile_batches: std::sync::atomic::AtomicU64,
    /// Capabilities requeued for one trailing pass.
    pub reconcile_requeued: std::sync::atomic::AtomicU64,
    /// Demanded capabilities the most recent pass reconciled (a gauge).
    pub dirty_capabilities: std::sync::atomic::AtomicU64,
    /// Org intakes refused because a Leader row would not be admitted
    /// (all-or-none preflight).
    pub intake_preflight_refused: std::sync::atomic::AtomicU64,
    /// Reconciliation fills withdrawn because their Leader row was refused
    /// (D-LS10).
    pub fill_withdrawn: std::sync::atomic::AtomicU64,
}

/// A point-in-time copy of [`OrgLeaderCounters`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrgLeaderCountersSnapshot {
    pub interest_over_cap: u64,
    pub projection_builds: u64,
    pub projection_cache_hits: u64,
    pub projection_discarded: u64,
    pub reconcile_passes: u64,
    pub reconcile_batches: u64,
    pub reconcile_requeued: u64,
    pub dirty_capabilities: u64,
    pub intake_preflight_refused: u64,
    pub fill_withdrawn: u64,
}

impl OrgLeaderCounters {
    /// Bump one counter.
    pub(crate) fn bump(counter: &std::sync::atomic::AtomicU64) {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Copy every counter.
    pub(crate) fn snapshot(&self) -> OrgLeaderCountersSnapshot {
        let read = |c: &std::sync::atomic::AtomicU64| c.load(std::sync::atomic::Ordering::Relaxed);
        OrgLeaderCountersSnapshot {
            interest_over_cap: read(&self.interest_over_cap),
            projection_builds: read(&self.projection_builds),
            projection_cache_hits: read(&self.projection_cache_hits),
            projection_discarded: read(&self.projection_discarded),
            reconcile_passes: read(&self.reconcile_passes),
            reconcile_batches: read(&self.reconcile_batches),
            reconcile_requeued: read(&self.reconcile_requeued),
            dirty_capabilities: read(&self.dirty_capabilities),
            intake_preflight_refused: read(&self.intake_preflight_refused),
            fill_withdrawn: read(&self.fill_withdrawn),
        }
    }
}

/// The reconciler's bounded route-refresh tick (D-LS5): every demanded
/// capability is re-ranked at least this often, so route estimates and
/// reachability cannot go stale indefinitely with no topology wake seam.
pub(crate) const ORG_LEADER_ROUTE_REFRESH: Duration = Duration::from_secs(5);

/// The most org interests one apply batch commits before the reconciler yields
/// (plan §6), so a 512-interest leader cannot monopolize the runtime.
pub(crate) const ORG_LEADER_APPLY_BATCH: usize = 64;

/// The complete source vector a cached projection was built under (D-LS1). A
/// cached set is reused only when EVERY component still matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OrgCandidateSource {
    /// The scoped store's OWNER visible-set generation.
    pub owner_revision: u64,
    /// The sensing authority security view the floors came from.
    pub authority: SensingAuthorityStamp,
    /// The peer-session generation (`None` once the space is exhausted).
    pub session_generation: Option<u64>,
    /// The reconciler's route-refresh epoch (D-LS5).
    pub route_epoch: u64,
}

impl OrgCandidateSource {
    /// Whether a projection built under `self` may be served under `current`.
    /// An exhausted session generation or a non-current authority stamp never
    /// matches, on either side.
    fn serves(&self, current: &OrgCandidateSource) -> bool {
        self.owner_revision == current.owner_revision
            && self.authority.is_current(&current.authority)
            && self.session_generation.is_some()
            && self.session_generation == current.session_generation
            && self.route_epoch == current.route_epoch
    }
}

struct CachedCandidates {
    source: OrgCandidateSource,
    candidates: Arc<OrgCapabilityCandidates>,
}

/// The bounded per-capability projection cache (D-LS1). At most one entry per
/// demanded capability, and at most [`MAX_ORG_LEADER_INTERESTS`] entries.
pub(crate) struct OrgCandidateCache {
    entries: HashMap<CapabilityId, CachedCandidates>,
    capacity: usize,
}

impl Default for OrgCandidateCache {
    fn default() -> Self {
        Self::with_capacity(MAX_ORG_LEADER_INTERESTS)
    }
}

impl OrgCandidateCache {
    /// An empty cache holding at most `capacity` capabilities.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
        }
    }

    /// The cached projection for `capability`, iff it was built under a source
    /// vector that still serves `current` and no record in it has expired.
    pub(crate) fn lookup(
        &self,
        capability: &CapabilityId,
        current: &OrgCandidateSource,
        now_secs: u64,
    ) -> Option<Arc<OrgCapabilityCandidates>> {
        let entry = self.entries.get(capability)?;
        (entry.source.serves(current) && entry.candidates.is_live_at(now_secs))
            .then(|| Arc::clone(&entry.candidates))
    }

    /// Cache `candidates` for `capability` under `source`, replacing any older
    /// entry. A NEW capability past capacity is served but not retained, so the
    /// cache can never grow past its bound.
    pub(crate) fn store(
        &mut self,
        capability: &CapabilityId,
        source: OrgCandidateSource,
        candidates: OrgCapabilityCandidates,
    ) -> Arc<OrgCapabilityCandidates> {
        let candidates = Arc::new(candidates);
        if self.entries.contains_key(capability) || self.entries.len() < self.capacity {
            self.entries.insert(
                capability.clone(),
                CachedCandidates {
                    source,
                    candidates: Arc::clone(&candidates),
                },
            );
        }
        candidates
    }

    /// Drop every entry whose capability is no longer demanded by a live org
    /// interest (plan §6 trigger 6).
    pub(crate) fn retain_demanded(&mut self, demanded: impl Fn(&CapabilityId) -> bool) {
        self.entries.retain(|capability, _| demanded(capability));
    }

    /// Cached capabilities.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilitySet;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgRevocationBundle};
    use crate::adapter::net::behavior::org_scoped_ingest::{
        CapabilityAudienceScope, PreparedScopedCapability, VerifiedScopedCapability,
    };
    use crate::adapter::net::behavior::org_scoped_store::{NoConsumerGrants, ScopedStoreOutcome};
    use std::collections::BTreeMap;

    const CERT_GEN: u32 = 5;
    const CAP: &str = "nrpc:leader-echo";

    fn org_kp() -> OrgKeypair {
        OrgKeypair::from_bytes([7u8; 32])
    }

    fn provider(seed: u8) -> EntityId {
        EntityId::from_bytes([seed; 32])
    }

    fn descriptor(tags: &[&str]) -> Vec<u8> {
        let mut caps = CapabilitySet::new();
        for t in tags {
            caps = caps.add_tag(*t);
        }
        caps.to_bytes_compact()
    }

    fn owner_record(
        org: OrgId,
        seed: u8,
        expires_at: u64,
        tags: &[&str],
    ) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Owner {
                org_id: org,
                audience_handle: [0x11; 32],
            },
            provider(seed),
            org,
            1,
            expires_at,
            CERT_GEN,
            None,
            descriptor(tags),
        )
    }

    fn grant_record(seed: u8, expires_at: u64, tags: &[&str]) -> VerifiedScopedCapability {
        VerifiedScopedCapability::for_test(
            CapabilityAudienceScope::Grant {
                grant_id: [0x44; 32],
                audience_handle: [0x22; 32],
            },
            provider(seed),
            org_kp().org_id(),
            1,
            expires_at,
            CERT_GEN,
            Some([0x5A; 64]),
            descriptor(tags),
        )
    }

    fn ingest(state: &mut ScopedDiscoveryState, record: VerifiedScopedCapability) {
        let outcome = state.ingest(
            PreparedScopedCapability::prepare(record),
            0,
            &NoConsumerGrants,
        );
        assert_eq!(outcome, ScopedStoreOutcome::Inserted);
    }

    fn no_floors() -> OrgRevocationState {
        OrgRevocationState::empty()
    }

    fn floor_above(member: &EntityId) -> OrgRevocationState {
        let mut floors = BTreeMap::new();
        floors.insert(member.clone(), CERT_GEN + 1);
        let bundle = OrgRevocationBundle::try_issue(&org_kp(), &floors).expect("issue bundle");
        let mut state = OrgRevocationState::empty();
        state.merge_bundle(&bundle);
        state
    }

    /// A topology fixture: explicit pins, direct sessions, routes and estimates.
    #[derive(Default)]
    struct Topology {
        pins: HashMap<u64, EntityId>,
        direct: Vec<u64>,
        routed: Vec<u64>,
        estimates: HashMap<u64, Duration>,
    }

    impl OrgTopology for Topology {
        fn pinned_entity(&self, node_id: u64) -> Option<EntityId> {
            self.pins.get(&node_id).cloned()
        }
        fn has_direct_session(&self, node_id: u64) -> bool {
            self.direct.contains(&node_id)
        }
        fn routed(&self, node_id: u64) -> bool {
            self.routed.contains(&node_id)
        }
        fn route_estimate(&self, node_id: u64) -> Duration {
            self.estimates
                .get(&node_id)
                .copied()
                .unwrap_or(Duration::from_secs(3600))
        }
    }

    /// Every base for CAP in a store holding: two current owner members, one
    /// grant-partition record, one record of a FOREIGN owner org, and one expired
    /// owner record.
    fn mixed_store() -> ScopedDiscoveryState {
        let org = org_kp().org_id();
        let mut state = ScopedDiscoveryState::new();
        ingest(&mut state, owner_record(org, 1, 5_000, &[CAP]));
        ingest(
            &mut state,
            owner_record(org, 2, 5_000, &[CAP, "nrpc:other"]),
        );
        ingest(&mut state, owner_record(org, 3, 5_000, &["nrpc:other"]));
        ingest(&mut state, grant_record(4, 5_000, &[CAP]));
        let foreign = OrgKeypair::from_bytes([9u8; 32]).org_id();
        ingest(&mut state, owner_record(foreign, 5, 5_000, &[CAP]));
        ingest(&mut state, owner_record(org, 6, 1_500, &[CAP]));
        state
    }

    fn providers_of(bases: &[OrgCandidateBase]) -> Vec<EntityId> {
        let mut out: Vec<EntityId> = bases.iter().map(|b| b.provider.clone()).collect();
        out.sort_by_key(|entity| *entity.as_bytes());
        out
    }

    /// §8 dark-slice row 1: the projection admits ONLY floor-current owner-scope
    /// records of THIS owner org declaring the capability — the grant partition,
    /// a foreign org, an expired record, and a non-declaring member are absent.
    #[test]
    fn projection_admits_only_current_owner_scope_declarers() {
        let state = mixed_store();
        let org = org_kp().org_id();
        let bases = capture_org_candidate_bases(
            &state,
            &CapabilityId::new(CAP),
            &org,
            2_000,
            &no_floors(),
            0,
        );
        assert_eq!(providers_of(&bases), vec![provider(1), provider(2)]);
    }

    /// §8 dark-slice row 1 (revoked): a floor raised above a member's admitted
    /// certificate generation removes that member at the next projection.
    #[test]
    fn a_floor_raise_removes_the_revoked_member() {
        let state = mixed_store();
        let org = org_kp().org_id();
        let bases = capture_org_candidate_bases(
            &state,
            &CapabilityId::new(CAP),
            &org,
            2_000,
            &floor_above(&provider(1)),
            0,
        );
        assert_eq!(providers_of(&bases), vec![provider(2)]);
    }

    /// §8 dark-slice row 2: `node_id` is the authenticated derivation.
    #[test]
    fn node_id_is_the_entity_derivation() {
        let state = mixed_store();
        let bases = capture_org_candidate_bases(
            &state,
            &CapabilityId::new(CAP),
            &org_kp().org_id(),
            2_000,
            &no_floors(),
            0,
        );
        for base in &bases {
            assert_eq!(base.node_id, base.provider.node_id());
        }
    }

    /// Q2 / D-LS3: the local node is never a candidate, even when its own record
    /// is in the store.
    #[test]
    fn the_local_node_is_never_a_candidate() {
        let state = mixed_store();
        let local = provider(1).node_id();
        let bases = capture_org_candidate_bases(
            &state,
            &CapabilityId::new(CAP),
            &org_kp().org_id(),
            2_000,
            &no_floors(),
            local,
        );
        assert_eq!(providers_of(&bases), vec![provider(2)]);
    }

    fn base(seed: u8, expires_at: u64) -> OrgCandidateBase {
        OrgCandidateBase {
            provider: provider(seed),
            node_id: provider(seed).node_id(),
            generation: 1,
            expires_at,
        }
    }

    /// §8 dark-slice row 2 (reachability): a direct session counts only when its
    /// pin EQUALS the record's provider; a mismatching pin fails closed even when
    /// the routing table reaches the node id; an unpinned node is reachable only
    /// by route.
    #[test]
    fn reachability_requires_pin_equality_and_never_trusts_a_mismatch() {
        let matched = base(1, 9_000);
        let mismatched = base(2, 9_000);
        let routed_only = base(3, 9_000);
        let unreachable = base(4, 9_000);
        let mut topology = Topology::default();
        topology.pins.insert(matched.node_id, provider(1));
        topology.direct.push(matched.node_id);
        topology.pins.insert(mismatched.node_id, provider(99));
        topology.direct.push(mismatched.node_id);
        topology.routed.push(mismatched.node_id);
        topology.routed.push(routed_only.node_id);
        let set = project_org_candidates(
            vec![
                matched.clone(),
                mismatched.clone(),
                routed_only.clone(),
                unreachable.clone(),
            ],
            &topology,
        );
        let reach = |b: &OrgCandidateBase| set.get(b.node_id).expect("present").candidate.reachable;
        assert!(reach(&matched));
        assert!(!reach(&mismatched), "a mismatching pin must never count");
        assert!(reach(&routed_only));
        assert!(!reach(&unreachable));
        assert_eq!(
            set.len(),
            4,
            "unreachable candidates stay visible to the resolver"
        );
    }

    /// §8 dark-slice rows 3–4: org projections carry no tags or groups, and
    /// `authorized` is never false — even for a provider whose own descriptor
    /// carries tags.
    #[test]
    fn projection_emits_no_tags_and_is_always_authorized() {
        let set =
            project_org_candidates(vec![base(1, 9_000), base(2, 9_000)], &Topology::default());
        for entry in set.ranked() {
            assert!(entry.candidate.authorized);
            assert!(entry.candidate.tags.is_empty());
            assert!(entry.candidate.groups.is_empty());
        }
    }

    /// D-LS2: the order is `(route_estimate, node_id)`, computed once, and
    /// `get` indexes it.
    #[test]
    fn ranking_is_route_then_node_and_indexed() {
        let bases = vec![base(1, 9_000), base(2, 9_000), base(3, 9_000)];
        let mut topology = Topology::default();
        topology
            .estimates
            .insert(bases[0].node_id, Duration::from_millis(30));
        topology
            .estimates
            .insert(bases[1].node_id, Duration::from_millis(10));
        topology
            .estimates
            .insert(bases[2].node_id, Duration::from_millis(30));
        let set = project_org_candidates(bases.clone(), &topology);
        let order: Vec<u64> = set.ranked().iter().map(|e| e.candidate.node_id).collect();
        let (low, high) = if bases[0].node_id < bases[2].node_id {
            (bases[0].node_id, bases[2].node_id)
        } else {
            (bases[2].node_id, bases[0].node_id)
        };
        assert_eq!(order, vec![bases[1].node_id, low, high]);
        for (index, entry) in set.ranked().iter().enumerate() {
            assert_eq!(
                set.get(entry.candidate.node_id)
                    .map(|e| e.candidate.node_id),
                Some(order[index])
            );
        }
        assert!(set.get(12345).is_none());
    }

    /// The set carries its earliest record deadline and stops being live there.
    #[test]
    fn a_set_is_not_live_past_its_earliest_record_expiry() {
        let set =
            project_org_candidates(vec![base(1, 4_000), base(2, 3_000)], &Topology::default());
        assert_eq!(set.next_expiry(), Some(3_000));
        assert!(set.is_live_at(2_999));
        assert!(!set.is_live_at(3_000));
    }

    fn source(
        owner_revision: u64,
        store_generation: u64,
        session: Option<u64>,
        epoch: u64,
    ) -> OrgCandidateSource {
        OrgCandidateSource {
            owner_revision,
            authority: SensingAuthorityStamp::for_test(1, 2, store_generation, 1, false),
            session_generation: session,
            route_epoch: epoch,
        }
    }

    /// D-LS1: a cached projection is served only while EVERY source-vector
    /// component still matches and no record has expired.
    #[test]
    fn the_cache_serves_only_an_exactly_current_source_vector() {
        let cap = CapabilityId::new(CAP);
        let mut cache = OrgCandidateCache::with_capacity(4);
        let built = source(3, 7, Some(2), 0);
        cache.store(
            &cap,
            built,
            project_org_candidates(vec![base(1, 9_000)], &Topology::default()),
        );
        assert!(cache.lookup(&cap, &built, 100).is_some());
        assert!(
            cache.lookup(&cap, &source(4, 7, Some(2), 0), 100).is_none(),
            "owner revision"
        );
        assert!(
            cache.lookup(&cap, &source(3, 8, Some(2), 0), 100).is_none(),
            "authority"
        );
        assert!(
            cache.lookup(&cap, &source(3, 7, Some(3), 0), 100).is_none(),
            "session"
        );
        assert!(
            cache.lookup(&cap, &source(3, 7, Some(2), 1), 100).is_none(),
            "route epoch"
        );
        assert!(
            cache.lookup(&cap, &built, 9_000).is_none(),
            "expired record"
        );
        let exhausted = source(3, 7, None, 0);
        cache.store(
            &cap,
            exhausted,
            project_org_candidates(Vec::new(), &Topology::default()),
        );
        assert!(
            cache.lookup(&cap, &exhausted, 100).is_none(),
            "an exhausted session generation never serves"
        );
    }

    /// D-LS1 / D-LS4: the cache never grows past its bound, and an undemanded
    /// capability is evicted.
    #[test]
    fn the_cache_is_bounded_and_evicts_undemanded_capabilities() {
        let mut cache = OrgCandidateCache::with_capacity(2);
        let src = source(1, 1, Some(1), 0);
        for name in ["nrpc:a", "nrpc:b", "nrpc:c"] {
            cache.store(
                &CapabilityId::new(name),
                src,
                OrgCapabilityCandidates::default(),
            );
        }
        assert_eq!(cache.len(), 2);
        assert!(cache
            .lookup(&CapabilityId::new("nrpc:c"), &src, 0)
            .is_none());
        cache.retain_demanded(|cap| cap.as_str() == "nrpc:a");
        assert_eq!(cache.len(), 1);
        assert!(cache
            .lookup(&CapabilityId::new("nrpc:a"), &src, 0)
            .is_some());
    }

    // ---- LS-2: the org-only resolver --------------------------------------

    use crate::adapter::net::behavior::sensing::controller::resolve_candidates;
    use crate::adapter::net::behavior::sensing::identity::{
        AudienceScopeCommitment, GroupRef, TagMatch,
    };

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn three() -> OrgCapabilityCandidates {
        OrgCapabilityCandidates::for_test(&[(0xB1, ms(30)), (0xB2, ms(10)), (0xB3, ms(20))])
    }

    fn resolve(
        selector: ProviderSelector,
        mode: ResultMode,
    ) -> Result<ResolvedCandidates, ResolutionRefusal> {
        resolve_org_candidates(&selector, mode, &three(), &CandidatePolicy::default())
    }

    /// `AnyAuthorized` takes the projection in its precomputed route order, under
    /// the shared result-mode bounds.
    #[test]
    fn any_authorized_takes_the_projection_in_route_order() {
        let resolved =
            resolve(ProviderSelector::AnyAuthorized, ResultMode::TopK(3)).expect("resolves");
        assert_eq!(resolved.active, vec![0xB2, 0xB3, 0xB1]);
        let resolved = resolve(ProviderSelector::AnyAuthorized, ResultMode::Any).expect("resolves");
        assert_eq!(resolved.active, vec![0xB2]);
        assert_eq!(resolved.standby, vec![0xB3]);
    }

    /// §8 / Q1: an org `Node(id)` OUTSIDE the projection opens no branch, with
    /// no new refusal. RED coupling: the legacy `Node` short-circuit returns the
    /// named id unresolved — asserted here against the legacy resolver itself,
    /// so the two can never be confused.
    #[test]
    fn an_org_node_outside_the_projection_opens_no_branch() {
        let outsider = ProviderSelector::Node(0xEE);
        let resolved = resolve(outsider.clone(), ResultMode::Any).expect("no new refusal");
        assert!(resolved.active.is_empty() && resolved.standby.is_empty());
        let legacy = resolve_candidates(
            &outsider,
            ResultMode::Any,
            &[],
            &AudienceScopeCommitment::from_bytes([0; 32]),
            &CandidatePolicy::default(),
        )
        .expect("legacy resolves");
        assert_eq!(
            legacy.active,
            vec![0xEE],
            "the legacy short-circuit this resolver must not take"
        );
        assert_eq!(
            resolve(ProviderSelector::Node(0xB3), ResultMode::Any)
                .expect("resolves")
                .active,
            vec![0xB3]
        );
    }

    /// An org `Node(id)` inside the projection but unreachable opens no branch.
    #[test]
    fn an_unreachable_member_is_not_resolved_by_name() {
        let bases = vec![OrgCandidateBase {
            provider: EntityId::from_bytes([0xB1; 32]),
            node_id: 0xB1,
            generation: 1,
            expires_at: u64::MAX,
        }];
        let set = project_org_candidates(bases, &Topology::default());
        let resolved = resolve_org_candidates(
            &ProviderSelector::Node(0xB1),
            ResultMode::Any,
            &set,
            &CandidatePolicy::default(),
        )
        .expect("resolves");
        assert!(resolved.active.is_empty());
    }

    /// `Nodes` intersects with the projection and keeps projection order.
    #[test]
    fn nodes_intersect_with_the_projection() {
        let resolved = resolve(
            ProviderSelector::nodes(vec![0xB1, 0xEE, 0xB3]),
            ResultMode::TopK(3),
        )
        .expect("resolves");
        assert_eq!(resolved.active, vec![0xB3, 0xB1]);
    }

    /// Q6: `Tags` and `Group` resolve empty for an org interest.
    #[test]
    fn tags_and_groups_resolve_empty_for_an_org_interest() {
        let tags = ProviderSelector::tags(vec![TagMatch {
            key: "zone".into(),
            value: "eu".into(),
        }]);
        assert!(resolve(tags, ResultMode::Each)
            .expect("resolves")
            .active
            .is_empty());
        let group = ProviderSelector::Group(GroupRef::from_bytes([3; 32]));
        assert!(resolve(group, ResultMode::Each)
            .expect("resolves")
            .active
            .is_empty());
    }

    /// The `Each` amplification guard is the shared one.
    #[test]
    fn each_mode_keeps_the_amplification_guard() {
        let policy = CandidatePolicy {
            each_mode_max_providers: 2,
            ..CandidatePolicy::default()
        };
        let refused = resolve_org_candidates(
            &ProviderSelector::AnyAuthorized,
            ResultMode::Each,
            &three(),
            &policy,
        );
        assert_eq!(
            refused,
            Err(ResolutionRefusal::SelectorTooBroad { matched: 3, cap: 2 })
        );
    }

    /// Where the semantics overlap (`AnyAuthorized`, every candidate reachable
    /// and authorized, distinct route estimates) the org resolver returns
    /// exactly what the legacy resolver returns over the same candidates.
    #[test]
    fn org_resolution_matches_legacy_where_semantics_overlap() {
        let set = three();
        let snapshot: Vec<CandidateProvider> =
            set.ranked().iter().map(|e| e.candidate.clone()).collect();
        for mode in [
            ResultMode::Any,
            ResultMode::TopK(2),
            ResultMode::Quorum(2),
            ResultMode::Each,
        ] {
            let org = resolve_org_candidates(
                &ProviderSelector::AnyAuthorized,
                mode,
                &set,
                &CandidatePolicy::default(),
            );
            let legacy = resolve_candidates(
                &ProviderSelector::AnyAuthorized,
                mode,
                &snapshot,
                &AudienceScopeCommitment::from_bytes([0; 32]),
                &CandidatePolicy::default(),
            );
            assert_eq!(org, legacy, "{mode:?}");
        }
    }
}
