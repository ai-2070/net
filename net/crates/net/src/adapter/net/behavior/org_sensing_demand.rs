//! Retained organization exact-provider sensing demand, owned by a clone-shared
//! family (OLB / design D4–D5).
//!
//! # What this owns
//!
//! One `OrgSensingFamily` is the demand ownership root for one binding, on ONE
//! node. It holds, per capability, the set of exact-provider sensing leases
//! that node retains on the capability's AUTHORIZED providers, and it is the
//! thing whose last owner releasing them retires that demand.
//!
//! ```text
//! OrgSensingFamily (Clone = one Arc bump, NO Drop)
//!   └─ Arc<OrgSensingFamilyInner>            ── Drop HERE: last owner retires
//!        ├─ node: Arc<MeshNode>               (BOUND at mint, never a parameter)
//!        ├─ RoutingFamily                     (the node-minted family identity)
//!        ├─ txn_mu: Mutex<()>                 (ONE convergence at a time)
//!        └─ demand_mu: Mutex<BTreeMap<CapabilityAuthorityId,
//!                                     Arc<OrgSensingCapabilityDemand>>>
//!                                             └─ retained: Vec<RetainedProvider>
//!                                                  └─ SensingLeaseTicket
//! ```
//!
//! `Drop` is on the INNER body and never on the wrapper. A wrapper `Drop` fires
//! on every clone release, so an intermediate clone going away would retire
//! demand that surviving clones still hold. On the `Arc`-shared body it fires
//! exactly once, at last-owner release — which is the required semantics, and
//! the same shape the org SDK's `AudienceLeaseGuard` already uses.
//!
//! # Node binding
//!
//! The node is bound at `OrgSensingFamily::mint` and is NOT a per-call
//! argument. Every ticket, lease key, installation identity and armed refresh
//! record is node-local state: a family that accepted a node per call could
//! carry one node's tickets into another node's registry, where the token
//! values collide with unrelated holders — releasing a stranger's holder there
//! and orphaning its own. Binding removes the shape rather than documenting it.
//!
//! # Shared installations
//!
//! A lease key is SHARED node state: independent families, and the public
//! acquisition API, can all hold the same installation. So retirement is
//! release-then-SETTLE, never disarm-then-release:
//!
//! * the refresh record leaves only when the installation it names is no longer
//!   the live one, so a non-final retirement cannot take a live survivor's
//!   renewal away ([`MeshNode::settle_sensing_refresh`]);
//! * a release the transaction REFUSES leaves a holder that is still live and
//!   still ours. The ticket is handed to the node's refresh worker for paced
//!   retry ([`MeshNode::park_refused_release`]) rather than dropped: the
//!   remaining holders' own releases relax the aggregate, they never
//!   deregister a row this holder keeps referenced, so a dropped ticket is a
//!   permanent leak.
//!
//! # Authority
//!
//! Nothing about the demand is caller-supplied except which capability to
//! sense, and the facts are COUPLED to the view that qualifies them:
//!
//! * the AUDIENCE is derived from this node's own installed organization
//!   authority (`MeshNode::capture_sensing_authority_snapshot` →
//!   `canonical_org_sensing_commitment`). There is no audience argument, so a
//!   caller cannot fabricate one, and there is no legacy fallback: with no live
//!   authority the retention refuses outright;
//! * the POPULATION is derived from verified owner-private discovery
//!   (`MeshNode::org_sensing_authorized_population`) AFTER that capture, and
//!   the captured view's currentness is re-proved once the population is in
//!   hand. A floor revision or rotation in that window makes the retention
//!   re-derive rather than publish a stamp that never qualified its own
//!   population;
//! * every emitted registration is authored by the existing organization lease
//!   leg, which performs its own fresh capture and its own publication fence.
//!
//! # Lock discipline
//!
//! `txn_mu` serializes one whole convergence — capture, population, acquire,
//! publish, release — so two reconciliations of one capability cannot both
//! believe they are establishing it and leak the loser's tickets. It is taken
//! FIRST and outermost; the sensing locks are taken under it, never the other
//! way round.
//!
//! `demand_mu` serializes the map and NOTHING else, so a reader never blocks
//! behind an emission. Under it: integer, pointer and map work only — no
//! destructor, no lease-apply acquisition, no certificate verification, no
//! emission, no `.await`. Every site therefore EXTRACTS superseded/removed
//! `Arc`s into a local declared BEFORE the guard, releases the guard, and only
//! then closes them. A lease release takes `sensing_lease_apply_mu` and can
//! emit, and `parking_lot::Mutex` is not reentrant, so closing a demand inside
//! the guarded section is exactly the hazard this shape removes.
//!
//! # What stays dark
//!
//! Provider-free `OrgCapabilityRegistration` is not lit here: retention is
//! exact-provider only (`ProviderSelector::Node(provider)`). This module is not
//! wired into `OrgClient::call` and adds no public consumer API; it is the
//! internal demand/refresh substrate a later slice binds to.
//!
//! [`MeshNode::settle_sensing_refresh`]: crate::adapter::net::MeshNode
//! [`MeshNode::park_refused_release`]: crate::adapter::net::MeshNode

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::org_grant::CapabilityAuthorityId;
use super::org_routing_registry::RoutingFamily;
use super::scheduler_bridge::project_sensed_candidates;
use super::sensing;
use crate::adapter::net::mesh::{assert_off_sensing_locks, MeshNode, MAX_ORG_SENSING_POPULATION};

/// The most providers ONE capability's demand is ever retained over.
///
/// Re-exported here, and not merely a private truncation, because a caller
/// that reasons about whether a published population AGREES with the
/// population it asked for cannot tell a legitimate cap from a disagreement
/// without knowing the bound. Convergence truncates to this.
pub const MAX_SENSED_POPULATION: usize = MAX_ORG_SENSING_POPULATION;

/// How many distinct capabilities ONE family will retain demand for.
///
/// Derived from the routing family's own handle bound rather than invented:
/// both answer "how many independent demand identities may one binding hold".
pub const MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY: usize =
    super::org_routing_registry::MAX_HANDLES_PER_FAMILY;

/// The FIXED internal sensing policy (design D7.5). None of it is
/// caller-supplied, and none of it is request-relative — a per-call deadline
/// would fork the interest digest and split one lease into many.
const SENSING_WORK_LATENCY: sensing::WorkLatencyEnvelope =
    sensing::WorkLatencyEnvelope::start_within(Duration::from_secs(2));

/// The fixed internal sample cadence retained demand asks for, before it is
/// clamped to the node's own soft-state horizon.
///
/// The clamp is not a second policy: the acquisition path refuses an interval
/// wider than the ttl outright, so an unclamped constant would make retained
/// demand impossible on any node configured with a shorter horizon.
const SENSING_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// How many times a convergence will re-derive its facts when the authority
/// view moves underneath it.
///
/// A retention publishes the stamp its population was derived UNDER, so a view
/// that moves in that window must invalidate the derivation rather than be
/// papered over. Two attempts, then a typed refusal: an unbounded loop would
/// spin against a node whose authority is being republished continuously, and
/// a refusal here retains nothing and releases nothing.
const QUALIFICATION_ATTEMPTS: usize = 2;

/// Why a retention was refused. Internal and deterministic: a refusal retains
/// nothing, releases nothing, and evicts nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgSensingDemandRefused {
    /// This node has no live organization authority to derive an audience from,
    /// or it is poisoned/exhausted. There is no legacy fallback.
    NoAuthority,
    /// This family already retains `MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY`
    /// capabilities. Nothing is evicted to make room.
    FamilyAtCapacity,
    /// The node could not mint a family identity (the routing registry's
    /// family space is bounded and terminal). Mapped, never propagated: a
    /// binding that cannot sense is inert, not broken.
    FamilyUnavailable,
}

/// One retained provider inside one capability's demand.
struct RetainedProvider {
    provider: u64,
    key: sensing::SensingLeaseKey,
    /// The OBSERVATION identity of this provider's branch. Recorded at
    /// acquisition because the interest digest binds `Node(provider)`, so every
    /// retained provider has its OWN digest: the branch key cannot be
    /// re-derived from the capability alone, and re-deriving it per read would
    /// rebuild a spec the acquisition already canonicalized.
    branch: sensing::ProviderInterestKey,
    ticket: sensing::SensingLeaseTicket,
    /// The INSTALLATION this holder joined. Recorded because retirement has to
    /// settle the node's refresh schedule against the identity it actually
    /// released — a lease key is shared, so "my release" and "the row is gone"
    /// are different facts.
    installation_id: sensing::LeaseToken,
}

/// ONE population member's captured readiness row.
///
/// Deliberately carries no deadline, no observation timestamp and no continuity
/// state: freshness was already applied at the captured instant, and exporting
/// it would invite a second, later comparison against a first one's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgSensedRow {
    /// The authorized provider this row describes.
    pub provider: u64,
    /// Its projection at the captured instant. `Unknown` covers missing,
    /// removed, unretained and expired evidence alike.
    pub readiness: sensing::ProjectedReadiness,
    /// The provider-signed start estimate that backed the projection, if any.
    ///
    /// `None` whenever `readiness` is `Unknown`: an expired cell keeps its last
    /// observation until the mutating sweep clears it, and reporting that start
    /// alongside evidence that no longer vouches would hand a caller stale
    /// metadata dressed as current.
    pub estimated_start: Option<Duration>,
}

/// ONE request-relative sensed projection over one capability's authorized
/// population (design D6.5). Plain, immutable, ADVISORY data.
///
/// `rows`, the three buckets and any count taken from them are folds of ONE
/// captured `Vec`, so they cannot disagree. The buckets partition `rows`
/// exactly: every provider appears in exactly one of them, and none is
/// invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgSensedProjection {
    rows: Vec<OrgSensedRow>,
    viable: Vec<u64>,
    potential: Vec<u64>,
    non_viable: Vec<u64>,
}

impl OrgSensedProjection {
    /// Exactly one row per authorized population member, in population order.
    pub fn rows(&self) -> &[OrgSensedRow] {
        &self.rows
    }

    /// Providers whose readiness is viable for THIS request's budget, in sensed
    /// rank order — consumer-local route economics first, provider id as the
    /// deterministic tie-break.
    pub fn viable(&self) -> &[u64] {
        &self.viable
    }

    /// Providers with no viability verdict: `Unknown` evidence, or a `Ready`
    /// proof that does not fit this request's budget. NEVER pruned — a route
    /// change or a fresh beat can make either viable, and absence of evidence
    /// is not evidence of absence.
    pub fn potential(&self) -> &[u64] {
        &self.potential
    }

    /// Providers sensed explicitly NOT ready for this exact interest at the
    /// captured instant. Non-viable for THIS request only: they are ordered
    /// last, never removed, and nothing about discovery, membership or
    /// admission changes.
    pub fn non_viable(&self) -> &[u64] {
        &self.non_viable
    }

    /// The provider a request should try first, or `None` when nothing is
    /// currently viable — in which case the caller's own unsensed order stands.
    pub fn preferred(&self) -> Option<u64> {
        self.viable.first().copied()
    }
}

/// The deterministic candidate ORDER a sensed projection implies, over plain
/// data (design D7.2).
///
/// # Two populations, two different bounds
///
/// * `providers`/`same_org` describe the COMPLETE authorized candidate list
///   `C`, already in the caller's own deterministic order. `C` is **not**
///   bounded by the sensing population cap: SameOrg providers beyond it, and
///   every cross-organization candidate, are in the list too;
/// * `ranked` and `pruned` come from a sensed projection and are bounded by the
///   sensed population `S <= MAX_ORG_SENSING_POPULATION`.
///
/// # What it does
///
/// A stable class-ordered PERMUTATION of the input list: sensed-viable
/// candidates first in SENSED RANK ORDER, then everything with no verdict in
/// input order, then the ones sensed not-ready in input order. It adds no
/// comparison sort over the complete list, no map and no fixed-size array —
/// membership is a linear scan of two slices bounded by `S`.
///
/// # What it cannot do
///
/// It returns indices only, so it can neither invent a candidate nor drop one:
/// the result is always a permutation of `0..providers.len()`. A candidate that
/// was never sensed keeps its place among the unverdicted ones; a not-ready one
/// is ordered last rather than removed. Cross-organization candidates are never
/// sensed and therefore never pruned.
pub fn org_sensed_bucket_permutation(
    same_org: &[bool],
    providers: &[u64],
    ranked: &[u64],
    pruned: &[u64],
) -> Vec<usize> {
    debug_assert_eq!(
        same_org.len(),
        providers.len(),
        "the two candidate slices are parallel views of one list"
    );
    let mut viable: Vec<usize> = Vec::with_capacity(ranked.len());
    let mut potential: Vec<usize> = Vec::with_capacity(providers.len());
    let mut non_viable: Vec<usize> = Vec::with_capacity(providers.len());

    // Sensed rank order, which is the whole point of sensing: the sensed order
    // drives the emission, not the input's own order.
    for wanted in ranked {
        if let Some(index) = providers
            .iter()
            .zip(same_org)
            .position(|(provider, &owned)| owned && provider == wanted)
        {
            viable.push(index);
        }
    }

    // ONE pass over the input in its ORIGINAL order for everything else.
    // Membership in `ranked` IS the "already emitted" test, so no second
    // bookkeeping structure can disagree with the first.
    for (index, (&provider, &owned)) in providers.iter().zip(same_org).enumerate() {
        if owned && ranked.contains(&provider) {
            continue;
        }
        if owned && pruned.contains(&provider) {
            non_viable.push(index);
        } else {
            potential.push(index);
        }
    }

    viable.extend(potential);
    viable.extend(non_viable);
    viable
}

/// One capability's retained demand for ONE clone family.
///
/// `population` is an IMMUTABLE input snapshot and `retained` is a subset of
/// it, so a reconciliation's inputs can never be mutated underneath it.
pub struct OrgSensingCapabilityDemand {
    node: Arc<MeshNode>,
    /// The authority view the retention was derived against. A later
    /// reconciliation compares it and re-derives when it is no longer current.
    authority_epoch: sensing::SensingAuthorityStamp,
    /// Derived from this node's own owner organization — never supplied.
    audience: sensing::AudienceScopeCommitment,
    population: Arc<[u64]>,
    retained: Vec<RetainedProvider>,
}

impl OrgSensingCapabilityDemand {
    /// The authorized providers this demand was derived for.
    pub fn population(&self) -> &Arc<[u64]> {
        &self.population
    }

    /// The providers whose demand is actually retained — a subset of the
    /// population (a member whose acquisition refused is simply not retained).
    pub fn retained_providers(&self) -> Vec<u64> {
        self.retained.iter().map(|r| r.provider).collect()
    }

    /// The organization-derived audience the demand was registered under.
    pub fn audience(&self) -> sensing::AudienceScopeCommitment {
        self.audience
    }

    /// The retained holders as `(provider, ticket, installation)` triples
    /// (fixtures/tests only).
    ///
    /// A witness needs the EXACT ownership this demand holds — the token, not
    /// a resemblance of it — to act on it directly: releasing a held ticket
    /// out from under a demand is how "the installation still matches but the
    /// token is no longer a holder" is reached at all.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn retained_holders_for_test(
        &self,
    ) -> Vec<(u64, sensing::SensingLeaseTicket, sensing::LeaseToken)> {
        self.retained
            .iter()
            .map(|held| (held.provider, held.ticket, held.installation_id))
            .collect()
    }

    /// Whether every retained holder is STILL the installation it was
    /// committed with.
    ///
    /// `retained_providers` enumerates recorded tickets, which is HISTORY: a
    /// ticket can stop being a holder after a successful convergence (a
    /// refused restoration under an older authority invalidates the
    /// installation, for instance), and neither the provider list nor the
    /// authority stamp shows that. This is the same predicate the convergence
    /// transaction itself applies before carrying a ticket (the convergence
    /// transaction's own carry validation), exposed non-mutatingly
    /// so a caller deciding whether to reconcile asks the question core would
    /// answer rather than assuming the answer.
    ///
    /// Cheap and bounded: one registry lookup per retained provider,
    /// `|population| <= 32`, no mutation and no lease-apply acquisition.
    pub fn holders_are_live(&self) -> bool {
        self.retained.iter().all(|held| {
            self.node.sensing_lease_holder_installation(&held.ticket) == Some(held.installation_id)
        })
    }

    /// Whether the authority view this demand was derived against is STILL the
    /// live one. A replacement, revocation, rotation or poison makes it false,
    /// and a reconciliation then re-derives instead of reusing it.
    pub fn authority_is_current(&self) -> bool {
        self.node
            .sensing_authority_stamp_is_current(&self.authority_epoch)
    }

    /// The retained providers with their OBSERVATION identities
    /// (fixtures/tests only).
    ///
    /// A witness needs the exact branch key the acquisition canonicalized: the
    /// interest digest binds `Node(provider)` and the fixed internal sensing
    /// policy, so rebuilding the spec outside this module would be a
    /// resemblance of the key, not the key.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn retained_branches_for_test(&self) -> Vec<(u64, sensing::ProviderInterestKey)> {
        self.retained
            .iter()
            .map(|held| (held.provider, held.branch.clone()))
            .collect()
    }

    /// ONE request-relative readiness projection over this demand's authorized
    /// population, evaluated at ONE caller-captured instant (design D6.5).
    ///
    /// # The phases, and what runs where
    ///
    /// * **Phase 0** — off every sensing lock: the caller's `now` and `budget`,
    ///   and this demand's immutable `population`. Nothing is derived from live
    ///   state here, so the population a projection reports on can never be
    ///   mutated underneath it;
    /// * **Phase 1** — ONE `sensing_observations` critical section
    ///   ([`MeshNode::org_sensed_branch_snapshot`]): exactly one row per
    ///   population member, in population order, clamped to this demand's
    ///   retained branch keys. Missing, removed, expired and unretained
    ///   evidence all resolve `Unknown`, and no entry outside those keys is
    ///   read — sensing cannot contribute a provider;
    /// * **Phase 2** — off every sensing lock: ONE proximity pass;
    /// * **Phase 3** — off every sensing lock, pure: request-relative
    ///   classification and the sensed rank order.
    ///
    /// # What this is, and what it is NOT
    ///
    /// It is a coherent capture of THIS node's consumer cells at `now`. It is
    /// not linearized against the proximity plane's own updates, and no
    /// cross-plane or distributed linearizability is claimed. The result is
    /// ADVISORY plain data: it carries no lease, mints no grant, reserves
    /// nothing, and cannot admit an invocation. Provider churn after the
    /// capture is not a defect of the capture — the value describes the instant
    /// it was taken, and the next projection describes the next one.
    pub fn project_sensed_order(
        &self,
        now: Instant,
        budget: &sensing::ConsumerLatencyBudget,
    ) -> OrgSensedProjection {
        // PHASE 0 - immutable inputs, off every lock.
        let population = Arc::clone(&self.population);
        let retained: BTreeMap<u64, sensing::ProviderInterestKey> = self
            .retained
            .iter()
            .map(|held| (held.provider, held.branch.clone()))
            .collect();

        // PHASE 1 - one critical section; nothing else happens inside it.
        let rows = self
            .node
            .org_sensed_branch_snapshot(&population, &retained, now);

        // PHASE 2 - the proximity pass, off every sensing lock. One estimate
        // per row, in row order. Each estimate reports from its OWN callsite
        // (`MeshNode::sensing_route_estimate`), so the off-lock property is
        // observed at the work rather than beside it, and a projection that
        // stopped consulting the route plane would report nothing at all.
        assert_off_sensing_locks("sensed projection proximity pass");
        self.node
            .observe_sensing_projection_offlock("proximity", rows.len());
        let views: Vec<sensing::BranchView> = rows
            .iter()
            .map(
                |&(provider, projection, estimated_start)| sensing::BranchView {
                    provider,
                    projection,
                    estimated_start,
                    route_estimate: self.node.sensing_route_estimate(provider),
                },
            )
            .collect();

        // PHASE 3 - pure, off every sensing lock. `project_sensed_candidates`
        // is the same classifier the consumer aggregate uses, so a sensed order
        // can never drift from the readiness it was derived from.
        assert_off_sensing_locks("sensed projection classification and ordering");
        self.node
            .observe_sensing_projection_offlock("classification", views.len());
        let delta = project_sensed_candidates(&views, budget);
        // Reported on BOTH sides of the classifier: a lock taken for its
        // duration is visible at the second point even if the first one was
        // clean, and the counts prove the boundary had real work to do.
        assert_off_sensing_locks("sensed projection ranked order");
        self.node.observe_sensing_projection_offlock(
            "ranked",
            delta.viable.len() + delta.potential.len() + delta.non_viable.len(),
        );
        OrgSensedProjection {
            rows: rows
                .into_iter()
                .map(|(provider, readiness, estimated_start)| OrgSensedRow {
                    provider,
                    readiness,
                    estimated_start,
                })
                .collect(),
            viable: delta.viable,
            potential: delta.potential,
            non_viable: delta.non_viable,
        }
    }

    /// RETIRE every retained provider. The lease key's LAST holder release is
    /// what emits `Deregister`.
    ///
    /// Runs with `demand_mu` RELEASED — releasing takes
    /// `sensing_lease_apply_mu` and may emit, neither of which may happen under
    /// the family lock.
    fn close(&self) {
        for retained in &self.retained {
            release_retained(&self.node, retained);
        }
    }
}

/// Release ONE retained provider and settle its refresh record.
///
/// The order is load-bearing in both directions:
///
/// * RELEASE FIRST, settle after. Disarming first takes the cadence away
///   before anything knows whether this release even retires the installation
///   — a second family, or a public-API holder, may still own it — and a
///   refused release must leave the schedule exactly as it was;
/// * a REFUSED release means the transaction rolled back and this holder is
///   still live and still ours. The ticket is handed to the node's refresh
///   worker for paced retry instead of being dropped. Dropping it leaks a
///   holder nothing can reach: the surviving holders' own releases relax the
///   aggregate, they never deregister a row this holder keeps referenced, so
///   the row and its interest would outlive every owner.
fn release_retained(node: &Arc<MeshNode>, retained: &RetainedProvider) {
    if let Err(refused) = node.try_release_sensing_interest_lease(retained.ticket) {
        let outcome = MeshNode::park_refused_release(
            node,
            retained.ticket,
            retained.installation_id,
            retained.provider,
        );
        tracing::warn!(
            provider = format!("{:#x}", retained.provider),
            reason = %refused.reason,
            ?outcome,
            "org sensing demand: release refused; the lease keeps its pre-release \
             state and this node keeps owning the holder unless the holder is \
             already gone"
        );
        return;
    }
    node.settle_sensing_refresh(&retained.key, retained.installation_id);
    node.org_sensing_demand_counters().note_released();
}

/// The clone-family body. `Drop` lives here, and only here.
struct OrgSensingFamilyInner {
    /// THE node this family was minted on. Bound once, and never a per-call
    /// argument: every ticket and armed record below is this node's local
    /// state, so accepting a node per call would let one node's tickets be
    /// released against another node's registry.
    node: Arc<MeshNode>,
    /// The node-minted family identity. Held for the lifetime of the demand so
    /// the routing registry's family bound accounts for this binding.
    _family: RoutingFamily,
    /// ONE convergence at a time, for the WHOLE transaction: capture,
    /// population, acquire, publish, release. Outermost — the sensing locks are
    /// taken under it.
    ///
    /// Without it two reconciliations of one capability can both read "no prior
    /// entry", both acquire, and publish in turn: the loser's tickets are then
    /// owned by a container nothing holds, and final retirement releases only
    /// the winner's.
    txn_mu: parking_lot::Mutex<()>,
    /// THE serializing lock for this family's demand map, and nothing else, so
    /// a reader never blocks behind an emission.
    demand_mu: parking_lot::Mutex<BTreeMap<CapabilityAuthorityId, Arc<OrgSensingCapabilityDemand>>>,
}

impl Drop for OrgSensingFamilyInner {
    fn drop(&mut self) {
        // EXTRACT under the lock, CLOSE after it. `extracted` is declared
        // before the guard deliberately: relying on temporary-drop order here
        // is what would put a lease release — and therefore
        // `sensing_lease_apply_mu` — inside the family section.
        let mut extracted: Vec<Arc<OrgSensingCapabilityDemand>> = Vec::new();
        {
            let mut map = self.demand_mu.lock();
            extracted.extend(std::mem::take(&mut *map).into_values());
        }
        for demand in &extracted {
            demand.close();
        }
        drop(extracted);
    }
}

/// The clone-family owner. `Clone` is one `Arc` bump and has NO `Drop`: only
/// the shared body's destructor retires demand, so an intermediate clone going
/// away retires nothing.
///
/// `#[doc(hidden)]`: an internal ownership handle, not application API and not
/// semver-covered.
#[doc(hidden)]
#[derive(Clone)]
pub struct OrgSensingFamily {
    inner: Arc<OrgSensingFamilyInner>,
}

impl OrgSensingFamily {
    /// Mint a family BOUND to `node`. Fallible because the routing registry's
    /// family identity space is bounded and terminal.
    ///
    /// The bound node is the only node this family will ever touch. There is
    /// deliberately no way to re-point it: see the module's node-binding note.
    pub fn mint(node: &Arc<MeshNode>) -> Result<Self, OrgSensingDemandRefused> {
        let family = node
            .org_routing_family()
            .map_err(|_| OrgSensingDemandRefused::FamilyUnavailable)?;
        Ok(Self {
            inner: Arc::new(OrgSensingFamilyInner {
                node: Arc::clone(node),
                _family: family,
                txn_mu: parking_lot::Mutex::new(()),
                demand_mu: parking_lot::Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// The node this family is bound to.
    pub fn node(&self) -> &Arc<MeshNode> {
        &self.inner.node
    }

    /// How many owners share this family's body. `1` means the next release
    /// retires its demand.
    pub fn owners(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    /// The capabilities this family currently retains demand for.
    pub fn capabilities(&self) -> Vec<CapabilityAuthorityId> {
        self.inner.demand_mu.lock().keys().copied().collect()
    }

    /// The current demand for `capability`, if any.
    pub fn demand(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<Arc<OrgSensingCapabilityDemand>> {
        self.inner.demand_mu.lock().get(capability).cloned()
    }

    /// RETAIN (or RECONCILE) demand for the capability `tag` over the
    /// currently authorized population, on this family's own node.
    ///
    /// One TRANSACTION, start to finish: the authority capture, the population
    /// derivation, its currentness re-proof, the acquisitions, the publication
    /// and the departed releases all happen under this family's transaction
    /// lock, so two concurrent convergences of one capability serialize instead
    /// of both establishing it and leaking the loser's tickets.
    ///
    /// Within the transaction it converges:
    ///
    /// 1. the population is NARROWED first, so a departed provider is gone
    ///    before anything reads the new snapshot and the snapshot is always its
    ///    own immutable input;
    /// 2. additions are ACQUIRED — a per-provider refusal is skipped, not
    ///    fatal, so one unavailable provider cannot cost the rest their demand;
    /// 3. removals are RELEASED, after the map has been updated and the guard
    ///    released.
    ///
    /// Live holders survive churn: a retained provider that is still authorized
    /// keeps its EXISTING ticket and its existing refresh arming, so its
    /// installation identity — and therefore the provider's soft state — is not
    /// disturbed by a reconciliation that only adds or removes neighbours.
    pub fn retain(
        &self,
        tag: &str,
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        self.converge(tag, None)
    }

    /// [`Self::retain`] against an EXPLICIT population — a WITNESS seam.
    ///
    /// Gated to test/`fixtures` builds deliberately: `doc(hidden)` documents a
    /// caller's obligation, it does not enforce one, and production has no
    /// reason to hand in a population that discovery did not authorize. The
    /// churn orderings are driven through here; the audience is still derived
    /// from live authority and every registration is still authored and fenced
    /// by the organization lease leg.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn reconcile(
        &self,
        tag: &str,
        population: &[u64],
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        self.converge(tag, Some(population))
    }

    /// The convergence transaction. `explicit` is the witness seam's
    /// population; `None` derives it from verified owner-private discovery.
    fn converge(
        &self,
        tag: &str,
        explicit: Option<&[u64]>,
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        let capability = CapabilityAuthorityId::for_tag(tag);
        let node = &self.inner.node;
        // ONE convergence at a time for this family, taken OUTERMOST.
        let _txn = self.inner.txn_mu.lock();
        for _attempt in 0..QUALIFICATION_ATTEMPTS {
            // CAPTURE FIRST, then derive under the captured view. Deriving
            // first would let a provider-floor revision invalidate the
            // population while local membership stayed valid, and the demand
            // would then publish a stamp that never qualified its own facts.
            let snapshot = node
                .capture_sensing_authority_snapshot()
                .map_err(|_| OrgSensingDemandRefused::NoAuthority)?;
            let population = match explicit {
                Some(explicit) => explicit.to_vec(),
                None => node.org_sensing_authorized_population(&capability),
            };
            node.fire_sensing_population_seam();
            if !node.sensing_authority_snapshot_current(&snapshot) {
                // The qualifying view moved. Re-derive; do not publish facts
                // and a stamp that never went together.
                continue;
            }
            return self.converge_under(&capability, tag, &snapshot, population);
        }
        // The attempts are exhausted, not the authority: every pass captured a
        // live view and then watched it move. Counted as its own class so a
        // contended qualification is not read as a missing membership.
        node.org_sensing_demand_counters().note_view_moved();
        Err(OrgSensingDemandRefused::NoAuthority)
    }

    /// The convergence itself, under a view already proved current for the
    /// population it qualifies. Runs inside the family transaction.
    fn converge_under(
        &self,
        capability: &CapabilityAuthorityId,
        tag: &str,
        snapshot: &sensing::SensingAuthoritySnapshot,
        population: Vec<u64>,
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        let node = &self.inner.node;
        let audience =
            sensing::canonical_org_sensing_commitment(&snapshot.authority_view().owner_org);
        let authority_epoch = *snapshot.stamp();
        // Bound the sensed subset. The derivation helper already counts its own
        // truncation; an explicit population is clamped here so both entry
        // points obey one bound.
        let mut wanted = population;
        wanted.sort_unstable();
        wanted.dedup();
        wanted.truncate(MAX_ORG_SENSING_POPULATION);

        // Bound check and previous-state read, under the map lock. Map work
        // only.
        let previous = {
            let map = self.inner.demand_mu.lock();
            match map.get(capability) {
                Some(existing) => Some(Arc::clone(existing)),
                None if map.len() >= MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY => {
                    node.org_sensing_demand_counters().note_at_capacity();
                    return Err(OrgSensingDemandRefused::FamilyAtCapacity);
                }
                None => None,
            }
        };

        // PHASE 1 — off the map lock. Carry FORWARD every still-authorized
        // holder whose ownership is STILL REAL, then acquire the additions.
        //
        // "Its provider is still wanted" is not enough to keep a ticket.
        // Production invalidates whole INSTALLATIONS: a refused tightening
        // whose surviving-holder restoration cannot be authored under the moved
        // authority view drops the registry entry rather than leave it claiming
        // a row that no longer exists. Every holder of that key — including
        // this demand's — is then dead. Copying the ticket forward because the
        // provider is still authorized made the demand report a provider as
        // retained with no registry installation behind it, forever: refresh
        // answers `Absent` and stops re-arming, and every later unchanged
        // convergence copied the same corpse.
        // Ownership is therefore validated against ACTUAL HOLDER MEMBERSHIP:
        // is this exact token still a registration of this exact key, and if
        // so, which installation does it belong to? Both come from ONE
        // registry read.
        //
        // One read is the point, not a convenience. Asking "is it a holder"
        // and then "which installation is live" separately is two observations
        // at two instants: an independent public tightening whose restoration
        // current authority refuses invalidates the row between them, and the
        // second observation then contradicts the first about a ticket that
        // was perfectly valid when it was committed. That is a property of the
        // observation, not of the ticket, and asserting on it panicked
        // debug-assertion builds on legitimate movement.
        //
        // Ownership being real is still not enough on its own. A ticket also
        // has to have been minted under the audience this convergence just
        // derived. `SensingLeaseKey::ExactProvider` is audience-keyed, but the
        // authorized population is NOT: it is derived from the capability tag,
        // expiry and revocation floors projected through the TOFU pin map
        // (`owner_private_capability_providers`), with no owner-organization
        // predicate anywhere on that path. So an A->B owner-org rotation
        // leaves `wanted` byte-identical while every retained key still names
        // A. Carrying those forward made the demand simultaneously BELIEVED
        // CONVERGED by the SDK (the providers report as retained under the new
        // stamp, so the reconciler stops) and UNREFRESHABLE by the node
        // (`prepare_org_egress` cannot author an A-keyed egress under a B
        // view, so every renewal answers `AuthorityUnavailable`) - a
        // permanently dark demand that only `retire(tag)` could clear.
        //
        // The ticket's own key carries the audience, so this is a comparison,
        // not a second observation, and it is counted apart from ownership
        // invalidation because the two mean different things to an operator.
        let mut carried: Vec<RetainedProvider> = Vec::new();
        let mut departed: Vec<RetainedProvider> = Vec::new();
        let mut invalidated = 0u64;
        let mut rotated = 0u64;
        if let Some(previous) = &previous {
            for retained in &previous.retained {
                let moved = RetainedProvider {
                    provider: retained.provider,
                    key: retained.key,
                    branch: retained.branch.clone(),
                    ticket: retained.ticket,
                    installation_id: retained.installation_id,
                };
                let live = node.sensing_lease_holder_installation(&moved.ticket);
                node.fire_sensing_carry_validated_seam();
                if live != Some(moved.installation_id) {
                    // Nothing to carry and nothing to release: this token is
                    // not a holder of the installation it was committed with
                    // any more. A wanted provider falls through to a FRESH
                    // acquisition below; an unwanted one simply disappears.
                    invalidated += 1;
                    continue;
                }
                if moved.key.audience() != audience {
                    // The owner organization moved under this demand. Ownership
                    // is real - the check above just proved it - but it is
                    // ownership of the FORMER audience's key, which this node
                    // can no longer renew.
                    //
                    // RELEASED, not merely dropped. A dropped reference would
                    // leak the holder and its lease budget for good, since no
                    // later convergence would ever see the ticket again. The
                    // release is also the leg most likely to succeed: it needs
                    // organization authority only for a surviving-holder
                    // `Reregister`, so the ordinary single-holder case is a
                    // `Deregister`, which tears the row down with no membership
                    // claim at all. A refusal parks the ticket for paced retry
                    // rather than losing it.
                    rotated += 1;
                    departed.push(moved);
                    continue;
                }
                if wanted.binary_search(&retained.provider).is_ok() {
                    carried.push(moved);
                } else {
                    departed.push(moved);
                }
            }
        }
        if invalidated > 0 {
            node.org_sensing_demand_counters()
                .note_ownership_invalidated(invalidated);
        }
        if rotated > 0 {
            node.org_sensing_demand_counters()
                .note_audience_rotated(rotated);
        }
        let already: Vec<u64> = carried.iter().map(|r| r.provider).collect();
        let mut retained = carried;
        for provider in &wanted {
            if already.contains(provider) {
                continue;
            }
            if let Some(fresh) = Self::acquire_provider(node, tag, audience, *provider) {
                retained.push(fresh);
            }
        }

        let demand = Arc::new(OrgSensingCapabilityDemand {
            node: Arc::clone(node),
            authority_epoch,
            audience,
            population: Arc::from(wanted.into_boxed_slice()),
            retained,
        });

        // PHASE 2 — publish the new demand under the map lock, EXTRACTING the
        // superseded entry. The superseded `Arc` must not be dropped here: its
        // own `close` would take the lease-apply guard under this one.
        let superseded = {
            let mut map = self.inner.demand_mu.lock();
            map.insert(*capability, Arc::clone(&demand))
        };
        // PHASE 3 — guard released. The superseded demand's carried-forward
        // tickets were MOVED into the new demand, so only the departed ones are
        // released; the superseded container itself just drops.
        drop(superseded);
        for gone in &departed {
            release_retained(node, gone);
        }
        Ok(demand)
    }

    /// RETIRE the demand for capability `tag` entirely. Serialized against
    /// convergence by the same family transaction lock, so a retirement cannot
    /// interleave with an acquisition that is about to publish.
    pub fn retire(&self, tag: &str) {
        let capability = CapabilityAuthorityId::for_tag(tag);
        let _txn = self.inner.txn_mu.lock();
        let mut extracted: Vec<Arc<OrgSensingCapabilityDemand>> = Vec::new();
        {
            let mut map = self.inner.demand_mu.lock();
            extracted.extend(map.remove(&capability));
        }
        for demand in &extracted {
            demand.close();
        }
        drop(extracted);
    }

    /// Acquire one provider's exact-provider demand, and arm its refresh.
    ///
    /// The spec is DERIVED here and never supplied: `Node(provider)` exactly,
    /// so one interest is one provider and the digest binds the provider (a
    /// provider-free selector would corrupt the merge accounting and re-key
    /// every lease on churn). A per-provider refusal returns `None` — the
    /// population member is simply not retained.
    fn acquire_provider(
        node: &Arc<MeshNode>,
        tag: &str,
        audience: sensing::AudienceScopeCommitment,
        provider: u64,
    ) -> Option<RetainedProvider> {
        let spec = sensing::InterestSpec {
            capability_id: sensing::CapabilityId::new(tag),
            constraints: sensing::CanonicalConstraints::default(),
            work_latency: SENSING_WORK_LATENCY,
            providers: sensing::ProviderSelector::Node(provider),
            result_mode: sensing::ResultMode::Any,
            disclosure_class: sensing::DisclosureClass::Owner,
            audience,
        };
        // Clamp the fixed cadence to the node's own soft-state horizon: the
        // acquisition path refuses an interval wider than the ttl outright, so
        // an unclamped constant would make retained demand impossible on a node
        // configured with a shorter horizon.
        let interval = SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl());
        // ONE FACT from inside the acquisition's own transaction: the ticket,
        // the installation that holder actually joined, and whether this
        // acquisition (re-)registered the wire row.
        //
        // Neither of the last two may be re-derived afterwards by a key read.
        // A ticket paired with a separately sampled installation can name two
        // different incarnations — invalidate the row and let a public holder
        // re-establish it in the gap, and the pair describes a holder that
        // never existed, while every later validation of it agrees with itself.
        // Freshness inferred from a before/after pair of reads fails the same
        // way: a rival establishing in the gap makes a COALESCING acquisition
        // look establishing, and an arbitrarily old row is then armed a full
        // period out and expires before its first renewal.
        // In-crate witness seam: exactly where a pre-acquisition key sample
        // would sit. A witness establishes the row publicly here, so a caller
        // that went back to inferring freshness from a before/after pair of
        // samples reports `Established` for a join and is caught.
        node.fire_sensing_pre_acquire_seam();
        let acquired = match node.acquire_sensing_interest_lease_owned(&spec, provider, interval) {
            Ok(acquired) => acquired,
            Err(error) => {
                // Counted by the refusal's OWN class. A full lease table, an
                // interest at its holder bound and a cadence below a provider's
                // cached floor are not "this node has no organization
                // authority", and an operator reading that counter cannot act
                // on a label that covers every failure alike.
                node.org_sensing_demand_counters()
                    .note_acquisition_refused(&error);
                tracing::debug!(
                    provider = format!("{:#x}", provider),
                    %error,
                    "org sensing demand: provider not retained"
                );
                return None;
            }
        };
        let key = acquired.ticket.key;
        node.org_sensing_demand_counters().note_retained();
        // ARM the refresh for THIS installation. `ttl/2` on the node's own
        // soft-state horizon, as an absolute deadline on the node's single
        // worker — no timer per lease and no whole-second rounding.
        //
        // The provenance grounds the FIRST deadline: a row this acquisition
        // registered is fresh, a row it merely joined has unknown age and is
        // adopted by renewing at once unless somebody is already renewing it.
        // Arming an installation another holder already armed keeps the EARLIER
        // deadline either way: joining renews nothing, so it must never
        // postpone the renewal.
        MeshNode::arm_sensing_refresh(
            node,
            key,
            acquired.installation_id,
            node.sensing_refresh_period(),
            acquired.provenance,
        );
        Some(RetainedProvider {
            provider,
            key,
            // The observation identity of the interest this acquisition just
            // registered, from the SAME canonical spec the digest came from.
            branch: sensing::ProviderInterestKey::new(
                sensing::CapabilityInterestKey::for_spec(&spec),
                provider,
            ),
            ticket: acquired.ticket,
            installation_id: acquired.installation_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use crate::adapter::net::behavior::org_authority::NodeAuthority;
    use crate::adapter::net::mesh::{
        RefusedReleaseOutcome, SensingArmDecision, SensingArmProvenance, SensingRefreshOutcome,
        SensingRegistrationError, MIN_SENSING_REFRESH_PERIOD,
    };
    use crate::adapter::net::{EntityKeypair, MeshNodeConfig};
    use crate::adapter::Adapter;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

    const TAG: &str = "nrpc:gpu.infer";

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
    }

    fn other_org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x77u8; 32])
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, AtomicOrdering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "net-org-sensing-demand-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn adopt(node: &MeshNode, keys: &OrgKeypair, tag: &str) -> Arc<NodeAuthority> {
        let entity = node.entity_id().clone();
        let cert = OrgMembershipCert::try_issue(keys, entity.clone(), 1, 3600).expect("cert");
        Arc::new(NodeAuthority::adopt(&scratch(tag), cert, &entity, 0, None).expect("adopt"))
    }

    /// A sensing-enabled node with its OWN organization authority installed and
    /// a short soft-state horizon, so the refresh period is small enough to
    /// observe without a long test.
    async fn demand_node(tag: &str, ttl: Duration) -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let node = Arc::new(
            MeshNode::new(
                EntityKeypair::generate(),
                MeshNodeConfig::new(addr, [0x31u8; 32])
                    .with_sensing_coalescing(true)
                    .with_sensing_interest_ttl(ttl),
            )
            .await
            .expect("MeshNode::new"),
        );
        node.install_node_authority(adopt(&node, &org(), tag))
            .expect("install org authority");
        node
    }

    /// The audience retained demand derives — recomputed from this node's own
    /// authority, exactly as production does.
    fn audience_of(node: &Arc<MeshNode>) -> sensing::AudienceScopeCommitment {
        sensing::canonical_org_sensing_commitment(
            &node.node_authority().expect("authority").owner_org(),
        )
    }

    /// The spec retained demand derives for `provider` — the same fixed policy
    /// production uses, so a test can read the row and the lease key without
    /// the demand handing either over.
    fn spec_for(node: &Arc<MeshNode>, provider: u64) -> sensing::InterestSpec {
        sensing::InterestSpec {
            capability_id: sensing::CapabilityId::new(TAG),
            constraints: sensing::CanonicalConstraints::default(),
            work_latency: SENSING_WORK_LATENCY,
            providers: sensing::ProviderSelector::Node(provider),
            result_mode: sensing::ResultMode::Any,
            disclosure_class: sensing::DisclosureClass::Owner,
            audience: audience_of(node),
        }
    }

    fn key_for(node: &Arc<MeshNode>, provider: u64) -> sensing::ProviderInterestKey {
        sensing::ProviderInterestKey::new(spec_for(node, provider).key(), provider)
    }

    fn lease_key_for(node: &Arc<MeshNode>, provider: u64) -> sensing::SensingLeaseKey {
        let spec = spec_for(node, provider);
        sensing::SensingLeaseKey::ExactProvider {
            audience: spec.audience,
            interest_digest: spec.interest_digest(),
            provider,
        }
    }

    fn row_present(node: &Arc<MeshNode>, provider: u64) -> bool {
        node.sensing_downstream_entry(&key_for(node, provider), sensing::DownstreamId::LeasedLocal)
            .is_some()
    }

    /// Live holder count for a lease key, straight out of the registry.
    fn holders(node: &Arc<MeshNode>, key: &sensing::SensingLeaseKey) -> Option<usize> {
        node.sensing_interest_leases_for_test()
            .entry_for_test(key)
            .map(|(holders, _)| holders)
    }

    /// Poll `probe` until it holds, or fail with the demand state.
    async fn until(node: &Arc<MeshNode>, within: Duration, what: &str, probe: impl Fn() -> bool) {
        let deadline = Instant::now() + within;
        loop {
            if probe() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{what} did not happen within {within:?}: {:?}",
                node.org_sensing_demand_state_for_test()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // ---- OWNERSHIP -------------------------------------------------------

    /// An INTERMEDIATE clone releasing retires nothing; the LAST owner
    /// releasing retires everything.
    ///
    /// This is the whole point of putting `Drop` on the `Arc`-shared body: a
    /// wrapper `Drop` would fire on every clone release and take away demand
    /// that surviving clones still hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_last_family_owner_retires_retained_demand() {
        let node = demand_node("last-owner", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let providers = [
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
        ];

        let demand = family
            .reconcile(TAG, &providers)
            .expect("retention succeeds under live authority");
        assert_eq!(demand.retained_providers(), providers.to_vec());
        for provider in providers {
            assert!(row_present(&node, provider), "provider {provider:#x} row");
        }
        assert_eq!(node.org_sensing_demand_state_for_test().retained, 2);

        // An INTERMEDIATE clone goes away.
        let clone = family.clone();
        assert_eq!(family.owners(), 2);
        drop(clone);
        assert_eq!(family.owners(), 1);
        for provider in providers {
            assert!(
                row_present(&node, provider),
                "an intermediate clone's release retired demand a live owner still holds"
            );
        }
        assert_eq!(
            node.org_sensing_demand_state_for_test().released,
            0,
            "and released nothing"
        );

        // THE LAST owner goes away.
        drop(demand);
        drop(family);
        for provider in providers {
            assert!(
                !row_present(&node, provider),
                "the last owner's release must retire every retained provider"
            );
        }
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.released, 2, "{state:?}");
        assert_eq!(
            state.armed, 0,
            "and every refresh must be settled: {state:?}"
        );
        assert!(
            node.sensing_interest_leases_for_test().is_empty(),
            "no lease may survive its family"
        );
    }

    /// A NON-FINAL retirement leaves the shared installation refreshing.
    ///
    /// A lease key is shared node state: two independent families can hold the
    /// same installation. Retirement therefore releases and then SETTLES
    /// against the registry's own post-release truth. The pre-repair shape
    /// disarmed unconditionally, so the first family to go away took the live
    /// survivor's renewal with it — the holder stayed legitimate, the row
    /// stayed installed, and nothing would ever refresh it again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_nonfinal_family_retirement_keeps_the_shared_installation_refreshing() {
        let node = demand_node("shared-install", Duration::from_secs(30)).await;
        let first = OrgSensingFamily::mint(&node).expect("mint first");
        let second = OrgSensingFamily::mint(&node).expect("mint second");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first_demand = first.reconcile(TAG, &[provider]).expect("first retains");
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        let second_demand = second.reconcile(TAG, &[provider]).expect("second retains");
        assert_eq!(
            node.sensing_refresh_installation(&key),
            Some(installation),
            "precondition: the second family JOINED one installation"
        );
        assert_eq!(holders(&node, &key), Some(2), "two independent holders");
        let armed_before = node.sensing_refresh_arm_for_test(&key).expect("armed");

        // The FIRST family retires. It is not the last holder.
        drop(first_demand);
        drop(first);

        assert_eq!(
            holders(&node, &key),
            Some(1),
            "the survivor must still hold the installation"
        );
        assert!(row_present(&node, provider), "and the row must survive");
        assert_eq!(
            node.sensing_refresh_arm_for_test(&key),
            Some(armed_before),
            "the shared installation's refresh record was disarmed by a retirement that \
             did not retire it — the survivor keeps a row nothing will ever renew"
        );
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::Renewed,
            "and it must still be renewable"
        );

        // The LAST holder retires: now everything goes.
        drop(second_demand);
        drop(second);
        assert!(!row_present(&node, provider));
        assert_eq!(node.org_sensing_demand_state_for_test().armed, 0);
        assert!(node.sensing_interest_leases_for_test().is_empty());
    }

    /// A retirement release the TRANSACTION REFUSES does not lose its holder.
    ///
    /// The refusal means nothing moved: the holder is still live and still
    /// ours. The surviving holders' own releases only relax the aggregate —
    /// they never deregister a row this holder keeps referenced — so a dropped
    /// ticket is a permanent leak. Ownership moves to the node's refresh worker
    /// and the release is retried on its cadence.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_refused_retirement_release_keeps_the_holder_and_recovers_it() {
        // The horizon must exceed the fixed internal cadence so a SECOND
        // holder can sit at a looser interval: the family's release then has to
        // re-register the relaxed aggregate, which is the transaction that
        // current authority can refuse.
        let node = demand_node("refused-release", Duration::from_millis(2500)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        // A LOOSER independent holder, so the family's release relaxes the
        // installed cadence instead of being a no-op.
        let slow = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, provider),
                provider,
                Duration::from_millis(2500),
            )
            .expect("the second holder acquires");
        assert_eq!(holders(&node, &key), Some(2), "precondition");

        // No live authority: the organization release cannot be re-authored,
        // so the transaction refuses and nothing moves.
        node.clear_node_authority_for_test();
        drop(demand);
        family.retire(TAG);

        assert_eq!(
            holders(&node, &key),
            Some(2),
            "precondition: the release was refused transactionally"
        );
        let refused = node.org_sensing_demand_state_for_test();
        assert_eq!(
            refused.refused_release_parked, 1,
            "the refused release must be RETAINED, not logged and dropped: {refused:?}"
        );
        assert_eq!(refused.refused_release_outstanding, 1, "{refused:?}");
        assert_eq!(
            refused.armed, 1,
            "and a refused release must leave the cadence exactly as it was: {refused:?}"
        );

        // Authority returns. The worker's paced retry discharges the holder we
        // never stopped owning.
        node.install_node_authority(adopt(&node, &org(), "refused-release-again"))
            .expect("reinstall org authority");
        until(
            &node,
            Duration::from_secs(10),
            "the refused release recovered",
            || {
                node.org_sensing_demand_state_for_test()
                    .refused_release_recovered
                    == 1
            },
        )
        .await;
        assert_eq!(
            holders(&node, &key),
            Some(1),
            "exactly the still-live independent holder remains"
        );
        assert_eq!(node.org_sensing_demand_state_for_test().released, 1);

        // And that holder's own release now really does deregister.
        node.try_release_sensing_interest_lease(slow)
            .expect("the surviving holder releases");
        assert!(!row_present(&node, provider));
        assert!(
            node.sensing_interest_leases_for_test().is_empty(),
            "the pre-repair shape left an orphan here that nothing could ever release"
        );
        assert_eq!(
            node.sensing_refresh_installation(&key),
            None,
            "and the installation {installation:?} is gone"
        );
    }

    /// A family touches ONLY the node it was minted on.
    ///
    /// Ownership is bound at mint and there is no node parameter to pass a
    /// different one to. Tickets, lease keys and installation identities are
    /// node-local: carrying them into another node's registry released a
    /// stranger's holder there and orphaned the one left behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_family_only_touches_the_node_it_was_minted_for() {
        let a = demand_node("bound-a", Duration::from_secs(30)).await;
        let b = demand_node("bound-b", Duration::from_secs(30)).await;
        let family_a = OrgSensingFamily::mint(&a).expect("mint on a");
        let family_b = OrgSensingFamily::mint(&b).expect("mint on b");
        assert!(Arc::ptr_eq(family_a.node(), &a), "bound to a");
        assert!(Arc::ptr_eq(family_b.node(), &b), "bound to b");

        // The SAME provider id on both nodes, so a cross-node ticket would
        // collide on equal token/key values exactly as it used to.
        let demand_a = family_a.reconcile(TAG, &[42]).expect("a retains");
        let demand_b = family_b.reconcile(TAG, &[42]).expect("b retains");
        assert_eq!(a.sensing_interest_leases_for_test().len(), 1);
        assert_eq!(b.sensing_interest_leases_for_test().len(), 1);

        // Every later convergence goes to a's registry and only a's.
        let again = family_a.reconcile(TAG, &[42, 43]).expect("a reconciles");
        assert_eq!(again.retained_providers(), vec![42, 43]);
        assert_eq!(a.sensing_interest_leases_for_test().len(), 2);
        assert_eq!(
            b.sensing_interest_leases_for_test().len(),
            1,
            "a's family reached into b's registry"
        );

        drop(demand_a);
        drop(again);
        drop(family_a);
        assert!(
            a.sensing_interest_leases_for_test().is_empty(),
            "a's family must retire a's own leases"
        );
        assert_eq!(
            b.sensing_interest_leases_for_test().len(),
            1,
            "and must not have released b's unrelated holder"
        );
        assert_eq!(demand_b.retained_providers(), vec![42]);
        drop(demand_b);
        drop(family_b);
        assert!(b.sensing_interest_leases_for_test().is_empty());
    }

    /// Two CONCURRENT convergences of one capability leak nothing.
    ///
    /// A convergence is one transaction: capture, population, acquire, publish,
    /// release. Without that, both can read "no prior entry", both acquire, and
    /// only the winner's container is published — the loser's tickets are then
    /// owned by nothing, and final retirement releases only the winner's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_convergences_of_one_capability_leak_nothing() {
        let node = demand_node("concurrent-converge", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let (first, second) = (
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
        );

        // CONTENTION, not merely concurrency: park the first convergence INSIDE
        // its transaction (the population seam fires under the family
        // transaction lock), then start the second and prove it cannot finish
        // while the first holds the boundary. Spawning two operations and
        // hoping they overlap proves nothing about the lock that serializes
        // them.
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let release_rx = Arc::new(parking_lot::Mutex::new(release_rx));
        let parked = Arc::new(AtomicBool::new(false));
        {
            let parked = parked.clone();
            node.set_sensing_population_seam_for_test(Arc::new(move || {
                if parked.swap(true, AtomicOrdering::SeqCst) {
                    return;
                }
                let _ = entered_tx.send(());
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
            }));
        }

        // DISJOINT populations, so a lost transaction leaves a distinct
        // orphaned key rather than being absorbed by the winner's.
        let left = {
            let family = family.clone();
            tokio::task::spawn_blocking(move || family.reconcile(TAG, &[first]))
        };
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the first convergence must reach the parked transaction");
        let right = {
            let family = family.clone();
            tokio::task::spawn_blocking(move || family.reconcile(TAG, &[second]))
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !right.is_finished(),
            "the second convergence ran to completion while the first was still \
             inside its transaction — the two are not serialized at all"
        );
        assert!(!left.is_finished(), "and the first is still parked");
        let _ = release_tx.send(());
        let left = left.await.expect("join left").expect("left converges");
        let right = right.await.expect("join right").expect("right converges");
        node.clear_sensing_population_seam_for_test();

        // One of the two is the published demand; the other's providers were
        // handed over or released, never abandoned.
        let published = family
            .demand(&CapabilityAuthorityId::for_tag(TAG))
            .expect("one demand is published");
        assert_eq!(
            node.sensing_interest_leases_for_test().len(),
            published.retained_providers().len(),
            "the registry must hold exactly the published demand's leases"
        );

        drop(left);
        drop(right);
        drop(published);
        family.retire(TAG);
        assert!(
            node.sensing_interest_leases_for_test().is_empty(),
            "retirement must reach every ticket both transactions acquired"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.retained, state.released,
            "every retained provider must be accounted for: {state:?}"
        );
        assert_eq!(state.armed, 0, "{state:?}");
    }

    /// Reconciliation over a changed population keeps the SURVIVING holder's
    /// installation identity, adds the newcomer, and releases only the
    /// departed one.
    ///
    /// Identity preservation is the load-bearing half: re-acquiring a survivor
    /// would give it a fresh installation id, which resets its refresh and
    /// makes the provider's soft state churn for a change that did not concern
    /// it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconciliation_preserves_the_surviving_holders_installation() {
        let node = demand_node("reconcile", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let (a, b, c) = (
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
            node.node_id().wrapping_add(3),
        );

        let first = family.reconcile(TAG, &[a, b]).expect("retain a,b");
        assert_eq!(first.retained_providers(), vec![a, b]);
        let b_key = lease_key_for(&node, b);
        let b_installation = node
            .sensing_refresh_installation(&b_key)
            .expect("b is installed");
        let b_armed = node.sensing_refresh_arm_for_test(&b_key).expect("b armed");

        let second = family.reconcile(TAG, &[b, c]).expect("retain b,c");
        assert_eq!(second.retained_providers(), vec![b, c]);
        assert_eq!(
            second.population().as_ref(),
            &[b, c],
            "the population is the reconciliation's own immutable input"
        );
        assert_eq!(
            node.sensing_refresh_installation(&b_key),
            Some(b_installation),
            "the surviving holder was re-acquired instead of carried forward — its \
             installation identity moved, so its refresh was silently reset"
        );
        assert_eq!(
            node.sensing_refresh_arm_for_test(&b_key),
            Some(b_armed),
            "and its armed record must be untouched, not postponed"
        );
        assert!(
            !row_present(&node, a),
            "the departed provider must be released"
        );
        assert!(row_present(&node, b), "the survivor keeps its row");
        assert!(row_present(&node, c), "the newcomer gains one");
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.retained, 3, "a, b once, and c: {state:?}");
        assert_eq!(state.released, 1, "only the departed one: {state:?}");
        assert_eq!(state.armed, 2, "b and c stay armed: {state:?}");

        drop(first);
        drop(second);
        drop(family);
    }

    /// One provider whose acquisition refuses costs the others nothing.
    ///
    /// Driven through the REAL refusal path: the terminal holder-identity space
    /// is parked one short of its end, so exactly one acquisition can be named
    /// and the rest refuse with `IdentityExhausted`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_provider_does_not_cost_the_others_their_demand() {
        let node = demand_node("partial", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let providers = [
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
            node.node_id().wrapping_add(3),
        ];
        node.sensing_interest_leases_for_test()
            .seed_token_space_for_test(sensing::SensingInterestLeases::token_space_end() - 1);

        let demand = family
            .reconcile(TAG, &providers)
            .expect("a partial retention is still a retention");
        assert_eq!(
            demand.retained_providers().len(),
            1,
            "exactly one identity was left, so exactly one provider is retained"
        );
        assert_eq!(
            demand.population().len(),
            3,
            "the POPULATION is what discovery authorized, not what was retained"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.retained, 1, "{state:?}");
        assert_eq!(
            state.refused_identity_exhausted, 2,
            "both refusals must be counted, under the class they actually had: \
             {state:?}"
        );
        assert_eq!(
            state.refused_no_authority, 0,
            "an exhausted identity space is not a missing organization \
             authority - that mislabel is the defect this counter split \
             removed: {state:?}"
        );
        drop(demand);
        drop(family);
    }

    /// With no live organization authority there is no audience to derive, so
    /// the retention refuses outright. There is no legacy fallback and no
    /// caller-supplied audience to fall back TO.
    #[tokio::test]
    async fn a_retention_without_live_authority_retains_nothing() {
        let node = demand_node("no-authority", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        node.clear_node_authority_for_test();

        assert_eq!(
            family
                .reconcile(TAG, &[node.node_id().wrapping_add(1)])
                .err(),
            Some(OrgSensingDemandRefused::NoAuthority)
        );
        assert!(family.capabilities().is_empty(), "and retained nothing");
        assert!(node.sensing_interest_leases_for_test().is_empty());
        assert_eq!(node.org_sensing_demand_state_for_test().retained, 0);
    }

    /// The family's capability bound refuses fail-closed and evicts nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_family_capability_bound_refuses_and_evicts_nothing() {
        let node = demand_node("family-bound", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        for index in 0..MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY {
            family
                .reconcile(&format!("nrpc:cap-{index}"), &[provider])
                .expect("within the bound");
        }
        assert_eq!(
            family.capabilities().len(),
            MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY
        );

        assert_eq!(
            family.reconcile("nrpc:one-too-many", &[provider]).err(),
            Some(OrgSensingDemandRefused::FamilyAtCapacity)
        );
        assert_eq!(
            family.capabilities().len(),
            MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY,
            "a refusal must evict nothing"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test().refused_at_capacity,
            1
        );
        drop(family);
    }

    // ---- AUTHORITY PROVENANCE -------------------------------------------

    /// A population derived under a view that MOVES is re-derived, and a view
    /// that keeps moving refuses.
    ///
    /// The demand publishes the stamp its population was derived under, so the
    /// two have to have gone together. Deriving the population first — and
    /// capturing afterwards — let a provider-floor revision or a rotation
    /// invalidate the facts while the published stamp still read "current".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_population_derived_under_a_moved_view_is_re_derived() {
        let node = demand_node("stale-view", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");

        // ONE-SHOT: the view moves in exactly the window between the derivation
        // and its currentness re-proof. The retention must re-derive.
        let fired = Arc::new(AtomicU64::new(0));
        {
            let node = node.clone();
            let fired = fired.clone();
            let seam = node.clone();
            seam.set_sensing_population_seam_for_test(Arc::new(move || {
                if fired.fetch_add(1, AtomicOrdering::SeqCst) > 0 {
                    return;
                }
                node.install_node_authority(adopt(&node, &org(), "stale-view-rotate"))
                    .expect("rotate authority");
            }));
        }
        let demand = family
            .retain(TAG)
            .expect("a moved view must be re-derived, not refused outright");
        assert_eq!(
            fired.load(AtomicOrdering::SeqCst),
            2,
            "the retention must have derived its population TWICE"
        );
        assert!(
            demand.authority_is_current(),
            "the published stamp must be the one that qualified the population"
        );
        node.clear_sensing_population_seam_for_test();
        drop(demand);
        family.retire(TAG);

        // The view keeps moving: the convergence refuses rather than publishing
        // facts and a stamp that never went together.
        {
            let node = node.clone();
            let seam = node.clone();
            seam.set_sensing_population_seam_for_test(Arc::new(move || {
                node.install_node_authority(adopt(&node, &org(), "stale-view-again"))
                    .expect("rotate authority");
            }));
        }
        assert_eq!(
            family.retain(TAG).err(),
            Some(OrgSensingDemandRefused::NoAuthority),
            "a view that never settles must refuse"
        );
        node.clear_sensing_population_seam_for_test();
        assert!(family.capabilities().is_empty(), "and retain nothing");
        assert!(node.sensing_interest_leases_for_test().is_empty());
        drop(family);
    }

    // ---- REFRESH ---------------------------------------------------------

    /// A refresh RENEWS the installation and acquires no holder.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refresh_renews_without_acquiring_a_holder() {
        let node = demand_node("refresh-renew", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        let holders_before = holders(&node, &key);

        for _ in 0..8 {
            assert_eq!(
                node.refresh_sensing_interest_lease(&key, installation),
                SensingRefreshOutcome::Renewed
            );
        }
        assert_eq!(
            holders(&node, &key),
            holders_before,
            "a refresh minted a holder; eight of them would walk the lease to its \
             holder bound and then stop the final release from deregistering"
        );
        assert_eq!(
            node.sensing_refresh_installation(&key),
            Some(installation),
            "and the installation identity must not move"
        );
        assert_eq!(node.org_sensing_demand_state_for_test().refresh_renewed, 8);
        assert!(row_present(&node, provider));
        drop(demand);
        drop(family);
    }

    /// A rival acquisition INSIDE a refresh's own window is legitimate churn,
    /// not a corrupted transaction.
    ///
    /// The no-mutation baseline has to come from inside the transaction that
    /// holds it. A Phase 0 count could be changed by a holder joining the very
    /// same installation — identity unchanged — and the debug assertion then
    /// killed the node's ONLY refresh worker on a legal interleaving.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_rival_acquisition_inside_a_refresh_window_is_not_a_violation() {
        let node = demand_node("refresh-rival", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");

        // ONE-SHOT rival, fired inside the refresh's pre-apply window.
        let joined = Arc::new(parking_lot::Mutex::new(None));
        let fired = Arc::new(AtomicBool::new(false));
        {
            let node = node.clone();
            let joined = joined.clone();
            let fired = fired.clone();
            let seam = node.clone();
            seam.set_sensing_refresh_pre_apply_seam_for_test(Arc::new(move || {
                if fired.swap(true, AtomicOrdering::SeqCst) {
                    return;
                }
                let ticket = node
                    .acquire_sensing_interest_lease(
                        &spec_for(&node, provider),
                        provider,
                        SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl()),
                    )
                    .expect("the rival joins the same installation");
                *joined.lock() = Some(ticket);
            }));
        }

        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::Renewed,
            "a holder joining mid-refresh must not stop the renewal"
        );
        assert!(fired.load(AtomicOrdering::SeqCst), "the seam must have run");
        assert_eq!(
            holders(&node, &key),
            Some(2),
            "the rival really did join the installation being refreshed"
        );
        assert_eq!(
            node.sensing_refresh_installation(&key),
            Some(installation),
            "and its identity did not move"
        );
        node.clear_sensing_refresh_pre_apply_seam_for_test();

        let rival = joined.lock().take().expect("the rival's ticket");
        node.try_release_sensing_interest_lease(rival)
            .expect("the rival releases");
        drop(demand);
        drop(family);
    }

    /// A refresh armed for a RETIRED installation renews nothing, and a refresh
    /// armed for a SUPERSEDED one cannot touch its successor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refresh_never_resurrects_retired_or_superseded_demand() {
        let node = demand_node("refresh-retired", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(TAG, &[provider]).expect("retain");
        let retired = node.sensing_refresh_installation(&key).expect("installed");
        drop(first);
        family.retire(TAG);
        assert!(!row_present(&node, provider), "precondition: retired");

        assert_eq!(
            node.refresh_sensing_interest_lease(&key, retired),
            SensingRefreshOutcome::Absent
        );
        assert!(
            !row_present(&node, provider),
            "a refresh RESURRECTED retired demand"
        );

        // Re-establish: a fresh installation identity for the same key.
        let second = family.reconcile(TAG, &[provider]).expect("re-retain");
        let successor = node.sensing_refresh_installation(&key).expect("installed");
        assert!(
            successor > retired,
            "a re-established installation must be a strictly newer identity"
        );
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, retired),
            SensingRefreshOutcome::Superseded,
            "the stale record renewed the SUCCESSOR"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.refresh_absent, 1, "{state:?}");
        assert_eq!(state.refresh_superseded, 1, "{state:?}");
        assert_eq!(state.refresh_renewed, 0, "{state:?}");
        drop(second);
        drop(family);
    }

    /// A STALE re-arm cannot displace its own successor's schedule.
    ///
    /// The worker takes a record out before firing and re-arms after, so its
    /// re-arm can land after the installation was retired, replaced and armed
    /// again. Installation identities are minted from one monotone allocator,
    /// so an older one is refused rather than allowed to overwrite the newer
    /// record — which used to leave the successor with no armed record at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stale_rearm_cannot_displace_its_successor() {
        let node = demand_node("stale-rearm", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(TAG, &[provider]).expect("retain");
        let old = node.sensing_refresh_installation(&key).expect("installed");
        drop(first);
        family.retire(TAG);
        let second = family.reconcile(TAG, &[provider]).expect("re-retain");
        let new = node.sensing_refresh_installation(&key).expect("installed");
        assert!(new > old, "precondition: a strictly newer installation");
        let successor_arm = node.sensing_refresh_arm_for_test(&key).expect("armed");

        // The stale worker's re-arm arrives late.
        assert!(
            !MeshNode::arm_sensing_refresh(
                &node,
                key,
                old,
                Duration::from_millis(10),
                SensingArmProvenance::Established
            ),
            "an older installation must not be armable over a newer one"
        );
        assert_eq!(
            node.sensing_refresh_arm_for_test(&key),
            Some(successor_arm),
            "the stale re-arm replaced the successor's record; the next firing would \
             report Superseded and leave the successor with nothing armed"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test().refresh_arm_stale,
            1
        );
        drop(second);
        drop(family);
    }

    /// A holder JOINING a live installation cannot postpone its renewal.
    ///
    /// Joining renews nothing, so resetting the deadline to `now + period` on
    /// every join walks the renewal past the row's own expiry: at ttl 30 s and
    /// period 15 s, joins at 10 s and 20 s move it to 25 s then 35 s while the
    /// row expires at 30 s. The EARLIEST deadline for an installation wins.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_joining_holder_cannot_postpone_a_live_renewal() {
        let node = demand_node("no-postpone", Duration::from_secs(30)).await;
        let first = OrgSensingFamily::mint(&node).expect("mint first");
        let second = OrgSensingFamily::mint(&node).expect("mint second");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first_demand = first.reconcile(TAG, &[provider]).expect("first retains");
        let armed = node.sensing_refresh_arm_for_test(&key).expect("armed");
        tokio::time::sleep(Duration::from_millis(60)).await;

        let second_demand = second.reconcile(TAG, &[provider]).expect("second joins");
        assert_eq!(
            holders(&node, &key),
            Some(2),
            "precondition: the second family JOINED the installation"
        );
        assert_eq!(
            node.sensing_refresh_arm_for_test(&key),
            Some(armed),
            "the joining holder POSTPONED the live renewal; another join or two and it \
             lands after the row's own expiry"
        );

        // An EARLIER deadline is still allowed — that is deadline precision,
        // not postponement.
        //
        // FIVE SECONDS, not a millisecond: an arm the real worker can already
        // consume before the read below turns this assertion into a race
        // against the schedule it is trying to observe (exact-head CI failed
        // exactly there). Five seconds is strictly earlier than the 15 s
        // cadence and strictly later than this test.
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        assert!(MeshNode::arm_sensing_refresh(
            &node,
            key,
            installation,
            Duration::from_secs(5),
            SensingArmProvenance::Established
        ));
        let earlier = node
            .sensing_refresh_arm_for_test(&key)
            .expect("the earlier arm must still be armed, not consumed");
        assert!(
            earlier.0 < armed.0,
            "an earlier deadline must be accepted: {earlier:?} vs {armed:?}"
        );
        drop(first_demand);
        drop(second_demand);
        drop(first);
        drop(second);
    }

    /// An authority ROTATION renews; a FOREIGN owner refuses without
    /// downgrading.
    ///
    /// A same-organization rotation is a legitimate replacement of the live
    /// authority: the refresh must still author on its own plane, and the
    /// demand's recorded epoch must stop being current (the stamp is a view
    /// identity, not an organization name). A DIFFERENT owner organization is
    /// not this lease's audience, so its refresh refuses rather than
    /// re-emitting the row under a legacy frame.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_authority_rotation_renews_and_a_foreign_owner_refuses() {
        // A THIRTY-SECOND horizon, deliberately. The damper's minimum gap is
        // `min(SENSING_UPSTREAM_MIN_GAP, ttl/2)` = 100 ms here, so a live
        // refresh still has to step outside it for the emission control to
        // mean anything — but the node's own worker is not due for 15 s, so it
        // cannot renew first and leave this call's direct refresh legitimately
        // damped. Exact-head CI failed on precisely that race (a 200 ms horizon
        // armed the worker at 100 ms, inside the test's own sleeps).
        let node = demand_node("refresh-authority", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let emissions = Arc::new(AtomicU64::new(0));
        {
            let seen = emissions.clone();
            node.set_sensing_phase_two_seam_for_test(Arc::new(move || {
                seen.fetch_add(1, AtomicOrdering::SeqCst);
            }));
        }
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        assert!(demand.authority_is_current(), "precondition");
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            1,
            "the retention itself must author exactly one organization frame"
        );
        let armed = node.sensing_refresh_arm_for_test(&key).expect("armed");

        // (a) REPLACE the live authority with a same-organization rotation.
        node.install_node_authority(adopt(&node, &org(), "refresh-authority-rotated"))
            .expect("rotate the live authority");
        assert!(
            !demand.authority_is_current(),
            "a rotation must move the recorded epoch"
        );
        // Step outside the damper's minimum gap, then author exactly once.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::Renewed,
            "a rotation is a legitimate replacement, not a fence"
        );
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            2,
            "and it must author exactly one more frame"
        );
        assert_eq!(
            node.sensing_refresh_arm_for_test(&key),
            Some(armed),
            "precondition for the counts above: the node's own worker never fired \
             during this test, so every emission is one this test caused"
        );
        // (b) A FOREIGN owner organization. Reachable only by relinquishing
        // first — the one-owner rule refuses a direct cross-org install.
        node.clear_node_authority_for_test();
        node.install_node_authority(adopt(&node, &other_org(), "refresh-authority-foreign"))
            .expect("install a foreign owner");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::AuthorityUnavailable,
            "this lease's audience is not the new owner's organization"
        );

        // (c) And with NO authority at all.
        node.clear_node_authority_for_test();
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::AuthorityUnavailable
        );
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            2,
            "a refresh this node cannot author emitted a frame — an organization \
             installation must never be renewed under a legacy downgrade"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .refresh_authority_refused,
            2
        );
        node.clear_sensing_phase_two_seam_for_test();
        drop(demand);
        drop(family);
    }

    /// An owner-organization ROTATION must not carry retained tickets forward.
    ///
    /// The authorized population is derived from the capability tag, expiry and
    /// revocation floors through the TOFU pin map — there is no owner-org
    /// predicate anywhere on that path — so an A→B rotation leaves the wanted
    /// set BYTE-IDENTICAL while every retained key still names A. A carry
    /// predicate that asks only "is this token still a live holder of its own
    /// installation" therefore says yes, and the provider is skipped by the
    /// acquisition loop.
    ///
    /// The witness reads the retained KEY'S AUDIENCE, not the retained provider
    /// list: `retained_providers()` is satisfied under the bug too. That is the
    /// whole difficulty — the demand reported itself converged under the new
    /// stamp while the node could never renew the lease again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_owner_org_rotation_reacquires_under_the_new_audience() {
        let node = demand_node("carry-audience-rotation", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);

        let before = family.reconcile(TAG, &[provider]).expect("retain");
        let audience_a = audience_of(&node);
        let key_a = lease_key_for(&node, provider);
        assert_eq!(before.audience(), audience_a);
        assert_eq!(before.retained.len(), 1);
        assert_eq!(before.retained[0].key.audience(), audience_a);
        assert_eq!(
            node.sensing_lease_holder_installation(&before.retained[0].ticket),
            Some(before.retained[0].installation_id),
            "precondition: the A-audience ticket is a LIVE holder of its own              installation — exactly what made the membership-only predicate              carry it across the rotation"
        );
        assert_eq!(holders(&node, &key_a), Some(1));
        drop(before);

        // ROTATE the owner organization. Reachable only by relinquishing first:
        // the one-owner rule refuses a direct cross-org install.
        node.clear_node_authority_for_test();
        node.install_node_authority(adopt(&node, &other_org(), "carry-audience-rotated"))
            .expect("install a foreign owner");
        let audience_b = audience_of(&node);
        assert_ne!(audience_a, audience_b, "precondition: the audience moved");

        let after = family.reconcile(TAG, &[provider]).expect("retain under B");
        assert_eq!(
            after.retained_providers(),
            vec![provider],
            "the population carries no owner-org predicate, so the provider is              still wanted — the bug and the fix agree on exactly this much"
        );
        assert_eq!(after.audience(), audience_b);
        assert_eq!(
            after.retained[0].key.audience(),
            audience_b,
            "THE property: the retained holder is registered under the audience              the demand reports. Carrying the A-keyed ticket forward satisfied              `retained_providers()` while leaving the lease unrenewable forever"
        );

        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.audience_rotated, 1,
            "the drop is counted as a ROTATION — 'the authority I registered              under is no longer mine' is a different operational story from              'somebody invalidated my installation'"
        );
        assert_eq!(
            state.ownership_invalidated, 0,
            "and it must not be charged to the ownership class"
        );

        // The A-keyed holder is RELEASED, not leaked: this demand was its only
        // holder, so the release previews `Deregister`, which needs no
        // membership claim and therefore succeeds under the foreign owner.
        assert_eq!(
            holders(&node, &key_a),
            None,
            "the former audience's lease must not outlive the rotation holding              this node's lease budget with nothing able to release it"
        );

        // And the fresh B-audience holder really renews — which is precisely
        // what the carried ticket could never do.
        let key_b = lease_key_for(&node, provider);
        assert_eq!(holders(&node, &key_b), Some(1));
        let installation_b = node
            .sensing_refresh_installation(&key_b)
            .expect("installed under B");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            node.refresh_sensing_interest_lease(&key_b, installation_b),
            SensingRefreshOutcome::Renewed,
            "the point of re-acquiring: this demand can actually be kept alive"
        );
        drop(after);
        drop(family);
    }

    /// A renewal that changed NOTHING on the wire must retry inside the row's
    /// remaining life — and must still park between attempts.
    ///
    /// `Refused` and `AuthorityUnavailable` both return before any egress is
    /// authored, so when the worker re-arms, the row's last registration is
    /// already one period old and it expires one period from now (the period is
    /// `ttl/2`). Re-arming as `Established` put the retry exactly on that
    /// expiry with zero margin. Re-arming as `Adopted` would be worse: the
    /// worker REMOVES a record before firing it, so a `now` deadline is
    /// immediately due again and a standing authority outage becomes a spin
    /// loop. `Unrenewed` is the only grounding that is both early enough and
    /// bounded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_renewal_retries_inside_the_rows_remaining_life() {
        // 400 ms horizon: period 200 ms, so a worker pass lands well inside a
        // short test without a nanosecond-scale schedule.
        let node = demand_node("unrenewed-rearm", Duration::from_millis(400)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let period = node.sensing_refresh_period();

        let decisions: Arc<parking_lot::Mutex<Vec<SensingArmDecision>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        {
            let decisions = Arc::clone(&decisions);
            node.set_sensing_arm_seam_for_test(Arc::new(move |decision| {
                decisions.lock().push(decision);
            }));
        }

        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        // Make every later renewal unauthorable on this lease's own plane
        // without retiring the demand: a FOREIGN owner organization.
        node.clear_node_authority_for_test();
        node.install_node_authority(adopt(&node, &other_org(), "unrenewed-foreign"))
            .expect("install a foreign owner");

        until(&node, Duration::from_secs(5), "a refused renewal", || {
            node.org_sensing_demand_state_for_test()
                .refresh_authority_refused
                > 0
        })
        .await;
        until(&node, Duration::from_secs(5), "the re-arm after it", || {
            decisions
                .lock()
                .iter()
                .any(|d| d.provenance == SensingArmProvenance::Unrenewed)
        })
        .await;

        let rearm = *decisions
            .lock()
            .iter()
            .find(|d| d.provenance == SensingArmProvenance::Unrenewed)
            .expect("the refused renewal must re-arm as unrenewed");
        let out = rearm.deadline.saturating_duration_since(rearm.armed_at);
        assert!(
            out < period,
            "an unrenewed retry must land strictly inside the row's remaining \
             life: this one is {out:?} out against a {period:?} period, which is \
             exactly when the provider's sweep drops the interest"
        );
        assert!(
            !out.is_zero(),
            "and it must still PARK: a zero deadline is immediately due again, \
             and because the worker removes a record before firing it, that \
             turns a standing authority outage into a spin loop"
        );
        node.clear_sensing_arm_seam_for_test();
        drop(demand);
        drop(family);
    }
    // ---- WORKER LIFECYCLE ------------------------------------------------

    /// END TO END at the internal boundary: the node's ONE refresh worker fires
    /// on its own schedule and renews EACH retained installation, with no timer
    /// per lease and no holder growth. Then shutdown closes the schedule.
    ///
    /// Per-installation evidence is the point: an aggregate renewal count is
    /// satisfied by one provider renewing twice. The armed record's `seq` is
    /// minted per arm, so a record that fired and re-armed is observable
    /// individually.
    ///
    /// This is a fixture proof of the demand/refresh substrate. It is NOT a
    /// production sensed `org.call` proof — nothing here is wired into call
    /// planning.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_single_worker_refreshes_every_retained_installation() {
        // A 300 ms horizon means a 150 ms refresh period — sub-second, so this
        // also proves the schedule arms at `Instant` precision rather than
        // rounding to a whole second, which would never fire here at all.
        let node = demand_node("worker", Duration::from_millis(300)).await;
        assert_eq!(node.sensing_refresh_period(), Duration::from_millis(150));
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let providers = [
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
        ];
        let demand = family.reconcile(TAG, &providers).expect("retain");
        let keys: Vec<_> = providers
            .iter()
            .map(|provider| lease_key_for(&node, *provider))
            .collect();
        let installations: Vec<_> = keys
            .iter()
            .map(|key| node.sensing_refresh_installation(key).expect("installed"))
            .collect();
        let held: Vec<_> = keys.iter().map(|key| holders(&node, key)).collect();
        let armed: Vec<_> = keys
            .iter()
            .map(|key| node.sensing_refresh_arm_for_test(key).expect("armed"))
            .collect();

        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.armed, 2, "both installations armed: {state:?}");
        assert!(state.worker_started, "on ONE worker: {state:?}");

        // EACH installation's own record must fire and re-arm — one period plus
        // slack, not a ten-second window a broken schedule could pass in.
        for (index, key) in keys.iter().enumerate() {
            let key = *key;
            let before = armed[index];
            let node = node.clone();
            until(
                &node.clone(),
                Duration::from_secs(3),
                "the worker renewed this installation",
                move || {
                    node.sensing_refresh_arm_for_test(&key)
                        .is_some_and(|(_, seq)| seq > before.1)
                },
            )
            .await;
        }
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(
                node.sensing_refresh_installation(key),
                Some(installations[index]),
                "a worker refresh moved an installation identity"
            );
            assert_eq!(
                holders(&node, key),
                held[index],
                "a worker refresh acquired a holder"
            );
        }
        let state = node.org_sensing_demand_state_for_test();
        assert!(state.refresh_renewed >= 2, "{state:?}");
        assert_eq!(state.armed, 2, "still exactly two armed: {state:?}");
        assert_eq!(state.refresh_absent, 0, "{state:?}");
        assert_eq!(state.refresh_superseded, 0, "{state:?}");

        node.shutdown().await.expect("shutdown");
        let closed = node.org_sensing_demand_state_for_test();
        assert!(closed.terminal, "the schedule must be terminal: {closed:?}");
        assert_eq!(closed.armed, 0, "and armed nothing: {closed:?}");
        assert_eq!(
            node.sensing_refresh_settled_for_test(),
            Some(true),
            "and a completed shutdown must observe SETTLEMENT: {closed:?}"
        );
        assert!(
            !MeshNode::arm_sensing_refresh(
                &node,
                keys[0],
                installations[0],
                Duration::from_millis(20),
                SensingArmProvenance::Established
            ),
            "nothing may be armed after the schedule is terminal"
        );
        drop(demand);
        drop(family);
    }

    /// An EARLIER deadline armed while the worker is parked shortens the park.
    ///
    /// One worker with absolute deadlines is only correct if an arm inside the
    /// current park is not simply missed until the old deadline elapses.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_earlier_arm_shortens_a_live_park() {
        // A 30 s horizon parks the worker 15 s out — far past this test.
        let node = demand_node("earlier-arm", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        // Let the worker actually reach its park.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            node.org_sensing_demand_state_for_test().refresh_renewed,
            0,
            "precondition: nothing is due yet"
        );

        assert!(MeshNode::arm_sensing_refresh(
            &node,
            key,
            installation,
            Duration::from_millis(20),
            SensingArmProvenance::Established
        ));
        until(
            &node,
            Duration::from_secs(3),
            "the parked worker woke",
            || node.org_sensing_demand_state_for_test().refresh_renewed >= 1,
        )
        .await;
        assert_eq!(
            node.sensing_refresh_installation(&key),
            Some(installation),
            "and it renewed the live installation"
        );
        drop(demand);
        drop(family);
    }

    /// A CANCELLED refresh close leaves recoverable ownership, and CONCURRENT
    /// closes both observe one settlement.
    ///
    /// The pre-repair shape moved the sole `JoinHandle` into the closer's own
    /// future before awaiting: a concurrent closer then found `None` and
    /// returned without settlement, and cancelling the first detached the task
    /// with no handle left to join or abort. This is the class the ordered
    /// egress already fixed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancelled_refresh_close_leaves_a_recoverable_worker() {
        // A 300 ms horizon fires the worker within 150 ms, so the park below
        // is entered by the REAL worker rather than simulated.
        let node = demand_node("close-cancel", Duration::from_millis(300)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);

        // PARK the worker inside a refresh, so the close below is provably
        // cancelled mid-join instead of racing a worker that already exited.
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let release_rx = Arc::new(parking_lot::Mutex::new(release_rx));
        let parked = Arc::new(AtomicBool::new(false));
        {
            let parked = parked.clone();
            node.set_sensing_refresh_pre_apply_seam_for_test(Arc::new(move || {
                if parked.swap(true, AtomicOrdering::SeqCst) {
                    return;
                }
                let _ = entered_tx.send(());
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
            }));
        }

        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        assert!(node.org_sensing_demand_state_for_test().worker_started);
        assert_eq!(node.sensing_refresh_settled_for_test(), Some(false));
        entered_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker must reach the parked refresh");

        // CANCEL a closer that has already taken teardown ownership. Hand-driven
        // with a biased `select!` rather than a timeout: the parked worker is
        // holding a runtime thread, so this must not depend on the timer.
        {
            let mut closing = std::pin::pin!(node.close_sensing_refresh_for_test());
            tokio::select! {
                biased;
                () = &mut closing => panic!(
                    "the first closer must be cancelled mid-join for this to prove anything"
                ),
                () = std::future::ready(()) => {}
            }
            // `closing` is dropped here: cancelled after taking teardown
            // ownership and before publishing settlement.
        }
        assert_eq!(
            node.sensing_refresh_settled_for_test(),
            Some(false),
            "a cancelled closer must not publish settlement"
        );
        assert_eq!(
            node.sensing_refresh_handle_owned_for_test(),
            Some(true),
            "the cancelled closer took the worker handle into its own future and \
             detached it on cancellation: nothing can join or abort the task now, and \
             any later close would publish settlement VACUOUSLY"
        );

        // CONCURRENT closers, both still BLOCKED at the repaired boundary: the
        // worker is parked inside its effect, so one closer waits on the join
        // and the other on teardown ownership. Closing after settlement would
        // prove nothing about either.
        let (left, right) = {
            let a = node.clone();
            let b = node.clone();
            (
                tokio::spawn(async move { a.close_sensing_refresh_for_test().await }),
                tokio::spawn(async move { b.close_sensing_refresh_for_test().await }),
            )
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !left.is_finished() && !right.is_finished(),
            "a closer completed while the worker was still inside its effect, so \
             neither reached the contested boundary"
        );
        assert_eq!(
            node.sensing_refresh_settled_for_test(),
            Some(false),
            "and neither may publish settlement before the join"
        );

        // Let the worker finish. Both closers then return, and exactly one
        // settlement stands.
        let _ = release_tx.send(());
        left.await.expect("join left closer");
        right.await.expect("join right closer");
        assert_eq!(
            node.sensing_refresh_settled_for_test(),
            Some(true),
            "the cancelled closer detached the worker: no later close can join or \
             abort it, so no shutdown ever observes settlement"
        );
        assert_eq!(
            node.sensing_refresh_handle_owned_for_test(),
            Some(false),
            "and a real settlement clears the slot it joined"
        );

        // A closer arriving AFTER settlement is a no-op, not a second teardown.
        node.close_sensing_refresh_for_test().await;
        assert_eq!(node.sensing_refresh_settled_for_test(), Some(true));
        let state = node.org_sensing_demand_state_for_test();
        assert!(state.terminal, "{state:?}");
        assert_eq!(state.armed, 0, "{state:?}");
        node.clear_sensing_refresh_pre_apply_seam_for_test();
        drop(demand);
        drop(family);
    }

    // ---- OWNERSHIP VALIDITY ---------------------------------------------

    /// A node whose SELF-provider emitter has a cadence floor, so a refused
    /// tightening can partition the shared row destructively — the production
    /// path that invalidates a whole installation.
    async fn emitter_demand_node(tag: &str, ttl: Duration, floor: Duration) -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let node = Arc::new(
            MeshNode::new(
                EntityKeypair::generate(),
                MeshNodeConfig::new(addr, [0x31u8; 32])
                    .with_sensing_coalescing(true)
                    .with_sensing_interest_ttl(ttl)
                    .with_sensing_incarnation(sensing::Incarnation::new(1))
                    .with_attestation_cadence_floor(floor),
            )
            .await
            .expect("MeshNode::new"),
        );
        node.install_node_authority(adopt(&node, &org(), tag))
            .expect("install org authority");
        node
    }

    /// INVALIDATE `provider`'s installation through the production recovery
    /// path: a tightening below the emitter floor partitions the shared row,
    /// and a REAL revocation-floor publication inside the partition-to-
    /// restoration window makes current authority refuse the restoration, so
    /// the whole entry is dropped rather than left claiming a row that is gone.
    fn invalidate_installation(node: &Arc<MeshNode>, provider: u64) {
        use crate::adapter::net::behavior::org::OrgRevocationBundle;
        use crate::adapter::net::identity::EntityId;
        use std::collections::BTreeMap;

        let invalidated_before = node
            .sensing_interest_leases_for_test()
            .installations_invalidated();
        let fired = Arc::new(AtomicU64::new(0));
        {
            let fired = fired.clone();
            let node2 = node.clone();
            node.set_sensing_acquire_pre_restore_seam_for_test(Arc::new(move || {
                if fired.fetch_add(1, AtomicOrdering::SeqCst) > 0 {
                    return;
                }
                let mut floors = BTreeMap::new();
                floors.insert(
                    EntityId::from_bytes([0x77u8; 32]),
                    5u32 + fired.load(AtomicOrdering::SeqCst) as u32,
                );
                let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("bundle");
                node2
                    .org_revocation_store()
                    .expect("store")
                    .apply_bundle(&bundle)
                    .expect("the floor raise must publish");
            }));
        }
        let refused = node
            .acquire_sensing_interest_lease(
                &spec_for(node, provider),
                provider,
                Duration::from_millis(50),
            )
            .expect_err("a tightening below the emitter floor must be refused");
        assert!(
            matches!(refused, SensingRegistrationError::RefusedByFloor { .. }),
            "got {refused:?}"
        );
        node.clear_sensing_acquire_pre_restore_seam_for_test();
        assert_eq!(
            fired.load(AtomicOrdering::SeqCst),
            1,
            "the partition-to-restoration window never ran, so nothing was invalidated"
        );
        assert_eq!(
            node.sensing_interest_leases_for_test()
                .installations_invalidated(),
            invalidated_before + 1,
            "precondition: current authority refused the restoration, so the whole \
             installation is invalidated"
        );
    }

    /// An INVALIDATED installation leaves no armed refresh record behind.
    ///
    /// The registry entry and the refresh schedule are separate state. The
    /// invalidation drops every holder of the key at once, so the armed record
    /// names a row that no longer exists - it is not a cadence any more, only
    /// occupancy in the schedule and in the armed count the capacity check
    /// reads, until its old deadline came round for a renewal that could only
    /// answer `Absent`. It is reclaimed at the transition that killed it.
    ///
    /// Identity, not truncation: the reclamation feeds the armed record's own
    /// installation id through the same rule a retirement uses, so a successor
    /// that established in the window keeps its cadence - which the
    /// re-convergence below then observes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invalidated_installation_leaves_no_armed_refresh() {
        let node = emitter_demand_node(
            "invalidated-arm",
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id();
        let key = lease_key_for(&node, provider);

        let _demand = family.reconcile(TAG, &[provider]).expect("retain");
        assert!(
            node.sensing_refresh_arm_for_test(&key).is_some(),
            "precondition: the retention armed a refresh for this key"
        );
        let armed_before = node.org_sensing_demand_state_for_test().armed;
        assert!(armed_before >= 1, "precondition: {armed_before}");

        invalidate_installation(&node, provider);
        assert_eq!(
            node.sensing_refresh_installation(&key),
            None,
            "precondition: the installation is gone"
        );
        assert_eq!(
            node.sensing_refresh_arm_for_test(&key),
            None,
            "the dead installation's refresh record must be reclaimed at the \
             invalidating transition, not left occupying the schedule until its \
             old deadline"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test().armed,
            armed_before - 1,
            "and the armed count the capacity check reads must drop with it"
        );

        // A successor arms its OWN record, which the reclamation cannot touch.
        let _again = family.reconcile(TAG, &[provider]).expect("re-converge");
        assert!(
            node.sensing_refresh_arm_for_test(&key).is_some(),
            "the re-convergence arms the fresh installation"
        );
    }

    /// A convergence must not carry ownership that is no longer REAL.
    ///
    /// Production invalidates whole installations: a refused tightening whose
    /// surviving-holder restoration cannot be authored under the moved
    /// authority view drops the registry entry. Copying a ticket forward
    /// because its provider is still authorized then made the demand report a
    /// provider as retained with nothing behind it — permanently, since every
    /// later unchanged convergence copied the same corpse and refresh answered
    /// `Absent` and stopped re-arming.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invalidated_installation_is_reacquired_not_carried_forward() {
        // Floor 1 s: the family's fixed 2 s cadence sits above it and a 50 ms
        // public tightening below it, so the tightening is refused AFTER its
        // partition removed the shared row.
        let node = emitter_demand_node(
            "invalidated",
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        // SELF provider: the only branch with a local emitter to refuse.
        let provider = node.node_id();
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(TAG, &[provider]).expect("retain");
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        assert!(row_present(&node, provider), "precondition: the row exists");

        invalidate_installation(&node, provider);
        assert_eq!(
            node.sensing_refresh_installation(&key),
            None,
            "precondition: the family's installation is gone"
        );
        assert!(!row_present(&node, provider));
        assert_eq!(
            first.retained_providers(),
            vec![provider],
            "the published container is immutable, so it still names the provider"
        );

        // The next convergence must REPAIR the ownership, not copy it.
        let second = family.reconcile(TAG, &[provider]).expect("re-converge");
        assert_eq!(second.retained_providers(), vec![provider]);
        let fresh = node.sensing_refresh_installation(&key).expect(
            "the convergence carried the DEAD ticket forward: the demand reports a \
             retained provider with no registry installation behind it, and no later \
             unchanged convergence ever repairs it",
        );
        assert!(
            fresh > installation,
            "and it must be a strictly newer installation: {fresh:?} vs {installation:?}"
        );
        assert!(row_present(&node, provider), "with a live row again");
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .ownership_invalidated,
            1,
            "the dropped ownership must be counted, not silent"
        );
        assert!(
            node.sensing_refresh_arm_for_test(&key).is_some(),
            "and the fresh installation must be armed"
        );
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, fresh),
            SensingRefreshOutcome::Renewed,
            "a repaired installation renews instead of answering Absent forever"
        );

        drop(first);
        drop(second);
        drop(family);
    }

    /// A STALE retained release cannot hold capacity against a LIVE one.
    ///
    /// A pending entry whose installation was invalidated owns nothing: its
    /// token can never be a holder again. Counting it against the bound is how
    /// a genuinely live refused ticket got rejected and abandoned while the
    /// registry was nowhere near its own capacity.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stale_retained_release_cannot_hold_capacity_against_a_live_one() {
        let node = emitter_demand_node(
            "stale-capacity",
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await;
        // ONE slot, so the saturation decision is reachable without fabricating
        // sixteen thousand real refusals.
        node.set_refused_release_cap_for_test(1);
        let stale_owner = OrgSensingFamily::mint(&node).expect("mint stale owner");
        let live_owner = OrgSensingFamily::mint(&node).expect("mint live owner");
        // The self provider is the one whose installation production can
        // invalidate; the second provider is an ordinary neighbour.
        let (aged, fresh) = (node.node_id(), node.node_id().wrapping_add(1));
        let aged_key = lease_key_for(&node, aged);
        let fresh_key = lease_key_for(&node, fresh);

        let stale_demand = stale_owner.reconcile(TAG, &[aged]).expect("retain aged");
        let live_demand = live_owner.reconcile(TAG, &[fresh]).expect("retain fresh");
        // LOOSER independent holders, so each family release has to re-register
        // a relaxed aggregate — the transaction current authority can refuse.
        let slow_aged = node
            .acquire_sensing_interest_lease(&spec_for(&node, aged), aged, Duration::from_secs(4))
            .expect("the aged key's second holder acquires");
        let slow_fresh = node
            .acquire_sensing_interest_lease(&spec_for(&node, fresh), fresh, Duration::from_secs(4))
            .expect("the fresh key's second holder acquires");

        // (1) Fill the single slot with a refused release.
        node.clear_node_authority_for_test();
        drop(stale_demand);
        stale_owner.retire(TAG);
        let parked = node.org_sensing_demand_state_for_test();
        assert_eq!(parked.refused_release_outstanding, 1, "{parked:?}");

        // (2) Make that pending entry own NOTHING, through production.
        node.install_node_authority(adopt(&node, &org(), "stale-capacity-again"))
            .expect("reinstall org authority");
        invalidate_installation(&node, aged);
        assert_eq!(node.sensing_refresh_installation(&aged_key), None);

        // (3) A genuinely LIVE refused release must be admitted: the dead entry
        // is reclaimed rather than defended.
        node.clear_node_authority_for_test();
        drop(live_demand);
        live_owner.retire(TAG);
        let after = node.org_sensing_demand_state_for_test();
        assert_eq!(
            after.refused_release_reclaimed, 1,
            "the stale pending entry must be reclaimed at saturation: {after:?}"
        );
        assert_eq!(
            after.refused_release_unowned, 0,
            "a LIVE refused ticket was abandoned because a dead one held the only \
             slot: {after:?}"
        );
        assert_eq!(
            after.refused_release_outstanding, 1,
            "and the live one is what the node now owns: {after:?}"
        );
        assert_eq!(
            holders(&node, &fresh_key),
            Some(2),
            "precondition: the live refused release really did not move"
        );

        node.set_refused_release_cap_for_test(usize::MAX);
        let _ = node.try_release_sensing_interest_lease(slow_aged);
        let _ = node.try_release_sensing_interest_lease(slow_fresh);
        drop(stale_owner);
        drop(live_owner);
    }

    /// A retry that fails puts the ticket BACK even at capacity.
    ///
    /// The retry loop extracts the whole retention set, so refusing to
    /// reinstate an already-owned ticket is the same lost-ownership defect the
    /// retention exists to prevent. Reinstatement is capacity-exempt by
    /// construction; only a terminal node declines.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_retry_reinstates_its_ticket_even_at_capacity() {
        // The horizon has to exceed the fixed 2 s cadence so a looser second
        // holder exists, and its half is the retry pace this test waits on.
        let node = demand_node("retry-capacity", Duration::from_millis(2500)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let slow = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, provider),
                provider,
                Duration::from_millis(2500),
            )
            .expect("the second holder acquires");

        node.clear_node_authority_for_test();
        drop(demand);
        family.retire(TAG);
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .refused_release_outstanding,
            1
        );

        // ZERO slots from here on. The retry below still fails (no authority),
        // so the only question is whether the node keeps what it owns.
        node.set_refused_release_cap_for_test(0);
        until(
            &node,
            Duration::from_secs(10),
            "the worker retried the refused release",
            || {
                node.org_sensing_demand_state_for_test()
                    .refused_release_retried
                    >= 2
            },
        )
        .await;
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refused_release_outstanding, 1,
            "a failed retry DISCARDED the live ticket it had just extracted: {state:?}"
        );
        assert_eq!(
            state.refused_release_unowned, 0,
            "and it must not be counted as unowned — it is owned: {state:?}"
        );
        assert_eq!(state.refused_release_recovered, 0, "{state:?}");
        assert_eq!(
            holders(&node, &key),
            Some(2),
            "the holder it owns is still live"
        );

        node.set_refused_release_cap_for_test(usize::MAX);
        let _ = node.try_release_sensing_interest_lease(slow);
        drop(family);
    }

    /// ADOPTING an installation nobody is renewing renews it AT ONCE.
    ///
    /// A public acquisition can establish an identical organization
    /// installation and arms nothing. A later family's coalescing acquisition
    /// re-registers neither table nor wire, so grounding its first deadline in
    /// join time puts the first renewal after the row's own expiry whenever the
    /// row is already older than a period. Its age is unknown here, so the
    /// adoption renews immediately and the cadence starts from that renewal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn adopting_an_unarmed_installation_renews_it_at_once() {
        // Fifteen-second period: an arm grounded in join time could not
        // possibly renew inside this test, so a renewal here is proof the
        // adoption was recognized rather than a timing coincidence.
        let node = demand_node("adopt", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let (adopted, established) = (
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
        );
        let adopted_key = lease_key_for(&node, adopted);
        let established_key = lease_key_for(&node, established);

        // A PUBLIC holder establishes the row at the same cadence the family
        // will ask for, so the family's acquisition coalesces and changes
        // nothing on the wire.
        let public = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, adopted),
                adopted,
                SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl()),
            )
            .expect("the public holder establishes");
        assert!(
            node.sensing_refresh_arm_for_test(&adopted_key).is_none(),
            "precondition: the public API arms nothing"
        );

        let demand = family
            .reconcile(TAG, &[adopted, established])
            .expect("retain both");
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refresh_adopted, 1,
            "exactly the joined installation is adopted: {state:?}"
        );
        // Wait for the SETTLED state, not for an effect counter. The worker
        // takes the record out, increments `refresh_renewed`, and only then
        // re-arms: an observer that treats the counter as a completion barrier
        // can look between those two steps and find no record at all.
        until(
            &node,
            Duration::from_secs(5),
            "the adoption renewed and re-armed on a grounded cadence",
            || {
                node.sensing_refresh_arm_for_test(&adopted_key)
                    .is_some_and(|(deadline, _)| {
                        deadline > Instant::now() + Duration::from_secs(10)
                    })
            },
        )
        .await;
        assert!(
            node.org_sensing_demand_state_for_test().refresh_renewed >= 1,
            "and the renewal itself must have happened"
        );
        // The ESTABLISHED key is fresh by construction: a full period away, and
        // deliberately NOT renewed immediately.
        let untouched = node
            .sensing_refresh_arm_for_test(&established_key)
            .expect("armed");
        assert!(
            untouched.0 > Instant::now() + Duration::from_secs(10),
            "an established installation must not be adopted: {untouched:?}"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test().refresh_adopted,
            1,
            "and the adoption count must not include it"
        );

        let _ = node.try_release_sensing_interest_lease(public);
        drop(demand);
        drop(family);
    }

    /// A continuously-due schedule cannot starve the runtime.
    ///
    /// `sensing_interest_ttl` accepts nanoseconds, and an unfloored `ttl/2`
    /// made the armed deadline elapse before the arm returned: the worker's
    /// loop never reached a park, and on a single-threaded executor an
    /// unrelated 20 ms timer never fired at all. This runs on exactly that
    /// executor — `#[tokio::test]` is current-thread.
    #[tokio::test]
    async fn a_nanosecond_horizon_cannot_starve_the_runtime() {
        let node = demand_node("starvation", Duration::from_nanos(1)).await;
        assert_eq!(
            node.sensing_refresh_period(),
            MIN_SENSING_REFRESH_PERIOD,
            "the period floor is the contract this test rests on"
        );
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        assert_eq!(demand.retained_providers(), vec![provider]);
        assert_eq!(node.org_sensing_demand_state_for_test().armed, 1);

        // An UNRELATED timer on the same single-threaded runtime.
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::time::sleep(Duration::from_millis(20)),
        )
        .await
        .expect(
            "the refresh worker starved the runtime: a continuously-due schedule \
                 spins inside one poll and no other task on this executor ever runs",
        );
        // And shutdown still completes on that same executor.
        node.shutdown().await.expect("shutdown");
        assert!(node.org_sensing_demand_state_for_test().terminal);
        drop(demand);
        drop(family);
    }

    /// PRODUCTION retention over a NONEMPTY verified discovery population, and
    /// what floor movement does to it.
    ///
    /// The population is not an argument here: it comes from a real
    /// owner-scoped announcement admitted by `verify_scoped_ingest` and
    /// projected onto a node id through the TOFU pin map. That chain is what
    /// `retain` depends on, so it has to be exercised with a record in it —
    /// stamp movement over an empty query says nothing about provenance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_verified_discovery_population_retains_and_follows_floor_movement() {
        let node = demand_node("discovery-population", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let capability = CapabilityAuthorityId::for_tag(TAG);

        // Nothing announced: the authorized population is genuinely empty, and
        // a retention over it retains nothing.
        assert!(node
            .org_sensing_authorized_population(&capability)
            .is_empty());
        let empty = family.retain(TAG).expect("retain over an empty population");
        assert!(empty.population().is_empty(), "no discovery, no population");
        assert!(empty.retained_providers().is_empty());

        // A REAL owner-scoped announcement from a same-organization provider,
        // through the production verified-ingest path.
        let provider_entity = announce_owner_provider(&node);
        assert_eq!(
            node.owner_private_capability_providers(&capability).len(),
            1,
            "the verified ingest must have admitted and stored the record"
        );

        // Discovery is NOT reachability: with no session pin there is no node
        // id to sense, so the population stays empty.
        assert!(
            node.org_sensing_authorized_population(&capability)
                .is_empty(),
            "an unpinned provider must not appear in the population"
        );
        let provider_node = node.node_id().wrapping_add(7);
        node.pin_peer_entity_for_test(provider_node, provider_entity.clone());
        assert_eq!(
            node.org_sensing_authorized_population(&capability),
            vec![provider_node]
        );

        // PRODUCTION `retain`: the population is the verified record's.
        let retained = family.retain(TAG).expect("retain over real discovery");
        assert_eq!(retained.population().as_ref(), &[provider_node]);
        assert_eq!(retained.retained_providers(), vec![provider_node]);
        assert!(
            row_present(&node, provider_node),
            "and the discovered provider really is sensed"
        );
        assert!(retained.authority_is_current());

        // FLOOR MOVEMENT: raise the provider's revocation floor above the
        // generation its certificate carries. The record stops being current,
        // so the next production retention drops the provider — the same
        // qualifying view that admitted it is what retires it.
        raise_provider_floor(&node, &provider_entity);
        assert!(
            node.owner_private_capability_providers(&capability)
                .is_empty(),
            "a raised floor must retire the verified record"
        );

        let after = family.retain(TAG).expect("retain after the floor moved");
        assert!(
            after.population().is_empty(),
            "the population must follow the floor: {:?}",
            after.population()
        );
        assert!(after.retained_providers().is_empty());
        assert!(
            !row_present(&node, provider_node),
            "and the provider's row must be released"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.retained, 1, "{state:?}");
        assert_eq!(state.released, 1, "{state:?}");
        assert_eq!(state.armed, 0, "{state:?}");

        drop(empty);
        drop(retained);
        drop(after);
        drop(family);
    }

    // ---- ACQUISITION-GROUNDED FACTS -------------------------------------

    /// The ticket, its installation and its freshness all come from ONE
    /// acquisition transaction.
    ///
    /// Sampling the installation by key after the acquisition returned could
    /// pair a ticket with a DIFFERENT incarnation — invalidate the row and let
    /// a public holder re-establish it in the gap, and the pair describes a
    /// holder that never existed while every later validation of it agrees
    /// with itself. Inferring freshness from a before/after pair of key reads
    /// fails the same way in the opposite direction: a rival establishing in
    /// the gap makes a coalescing acquisition look establishing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_acquisition_reports_its_own_installation_and_freshness() {
        let node = demand_node("acquired-fact", Duration::from_secs(30)).await;
        let provider = node.node_id().wrapping_add(1);
        let spec = spec_for(&node, provider);
        let interval = SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl());

        // ESTABLISHING: the row goes on the wire here and now, and the holder
        // token IS the installation identity.
        let first = node
            .acquire_sensing_interest_lease_owned(&spec, provider, interval)
            .expect("the establishing acquisition succeeds");
        assert_eq!(
            first.provenance,
            SensingArmProvenance::Established,
            "an acquisition that registered the row is fresh by construction"
        );
        assert!(
            node.holds_sensing_lease_token(&first.ticket),
            "the returned ticket must be a live holder of the key it names"
        );
        assert_eq!(
            node.sensing_refresh_installation(&first.ticket.key),
            Some(first.installation_id),
            "and the installation it reports is the live one"
        );

        // COALESCING: the same cadence changes neither table nor wire, so the
        // acquisition reports the EXISTING installation and unknown freshness.
        let second = node
            .acquire_sensing_interest_lease_owned(&spec, provider, interval)
            .expect("the coalescing acquisition succeeds");
        assert_eq!(
            second.provenance,
            SensingArmProvenance::Adopted,
            "a coalescing acquisition renewed nothing, so its row's age is unknown"
        );
        assert_eq!(
            second.installation_id, first.installation_id,
            "it joined the existing installation"
        );
        assert_ne!(
            second.ticket.token, second.installation_id,
            "and its own holder token is NOT that installation — the pair has to \
             come from the commit, not from two reads that happen to agree"
        );
        assert!(node.holds_sensing_lease_token(&second.ticket));

        // TIGHTENING: re-registers the row, so it is fresh again.
        let third = node
            .acquire_sensing_interest_lease_owned(&spec, provider, interval / 2)
            .expect("the tightening acquisition succeeds");
        assert_eq!(
            third.provenance,
            SensingArmProvenance::Established,
            "a tightening re-registered the row"
        );

        node.try_release_sensing_interest_lease(third.ticket)
            .expect("release");
        node.try_release_sensing_interest_lease(second.ticket)
            .expect("release");
        node.try_release_sensing_interest_lease(first.ticket)
            .expect("release");
    }

    /// Carry validation is ACTUAL MEMBERSHIP, not installation resemblance.
    ///
    /// The discriminating case: the installation the demand recorded is still
    /// the live one, but the demand's token is no longer one of its holders.
    /// An identity comparison passes there and carries ownership that does not
    /// exist; membership cannot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_carried_ticket_is_validated_by_membership_not_identity() {
        let node = demand_node("membership", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(TAG, &[provider]).expect("retain");
        let holders_before = first.retained_holders_for_test();
        assert_eq!(holders_before.len(), 1);
        let (_, ticket, installation) = holders_before[0];
        assert!(node.holds_sensing_lease_token(&ticket));

        // A public holder keeps the INSTALLATION alive at the same cadence.
        let public = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, provider),
                provider,
                SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl()),
            )
            .expect("the public holder joins");
        // Now retire the demand's own holder out from under it. The
        // installation survives; the demand's token does not.
        node.try_release_sensing_interest_lease(ticket)
            .expect("the demand's holder is released");
        assert_eq!(
            node.sensing_refresh_installation(&key),
            Some(installation),
            "precondition: the recorded installation is STILL the live one"
        );
        assert!(
            !node.holds_sensing_lease_token(&ticket),
            "precondition: but the recorded token is no longer a holder"
        );

        // The next convergence must re-acquire rather than carry it.
        let second = family.reconcile(TAG, &[provider]).expect("re-converge");
        let holders_after = second.retained_holders_for_test();
        assert_eq!(holders_after.len(), 1);
        assert_ne!(
            holders_after[0].1.token, ticket.token,
            "the convergence carried a ticket that owns nothing: the installation \
             matched, so an identity check agreed with itself"
        );
        assert!(
            node.holds_sensing_lease_token(&holders_after[0].1),
            "and the replacement must be a real holder"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .ownership_invalidated,
            1,
            "the dropped ownership must be counted"
        );
        assert!(row_present(&node, provider));

        let _ = node.try_release_sensing_interest_lease(public);
        drop(first);
        drop(second);
        drop(family);
    }

    /// ADOPTING a row that is already older than a period renews it before its
    /// own expiry.
    ///
    /// This is the aged case rather than the merely unarmed one: the public row
    /// is left to age past three quarters of its horizon before the family
    /// joins, so an arm grounded in join time would schedule the first renewal
    /// after the existing soft state had already expired.
    ///
    /// The assertion is on the DECISION, captured by the arm seam under the
    /// schedule guard that installed it. Neither of the two post-hoc
    /// observations works, because neither identifies what it sampled:
    ///
    /// * an observed RENEWAL INSTANT also carries the join's own cost, the
    ///   probe's poll granularity and the runner's sleep overshoot — on a
    ///   loaded CI runner those alone pushed a correct schedule past a 200 ms
    ///   horizon;
    /// * a later read of `armed` can return the SUCCESSOR record. The worker
    ///   legitimately dequeues the due-now adoption arm, renews, and re-arms
    ///   `Established` a full period out; that deadline is neither due now nor
    ///   inside the original horizon, and rejecting it rejects progress. `None`
    ///   is equally ambiguous: the record is removed BEFORE the renewal is
    ///   performed and counted, so absence does not mean renewed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn adopting_an_aged_installation_renews_before_it_expires() {
        // 2 s horizon: period 1 s, expiry 2 s after the public registration.
        let ttl = Duration::from_secs(2);
        let node = demand_node("adopt-aged", ttl).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let interval = SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl());
        let period = node.sensing_refresh_period();

        // Record every arm decision, in order, as the schedule took it.
        let decisions: Arc<parking_lot::Mutex<Vec<SensingArmDecision>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        {
            let decisions = Arc::clone(&decisions);
            node.set_sensing_arm_seam_for_test(Arc::new(move |decision| {
                decisions.lock().push(decision);
            }));
        }

        let public = node
            .acquire_sensing_interest_lease(&spec_for(&node, provider), provider, interval)
            .expect("the public holder establishes");
        let established_at = Instant::now();
        assert!(
            decisions.lock().is_empty(),
            "precondition: the public acquisition arms nothing"
        );

        // AGE it: 1.5 s of a 2 s horizon is already gone when the family joins.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let joined_at = Instant::now();
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        assert!(
            joined_at + period >= established_at + ttl,
            "precondition: the row must be aged enough that a full-period arm \
             would miss its expiry - {:?} of a {ttl:?} horizon is gone and the \
             period is {period:?}",
            joined_at.duration_since(established_at)
        );

        // THE DECISION. The first record is the join's own arm, whatever the
        // worker did next.
        let first = *decisions
            .lock()
            .first()
            .expect("the coalescing join must arm the row it joined");
        assert_eq!(
            first.provenance,
            SensingArmProvenance::Adopted,
            "a join that changed neither table nor wire is an adoption: {first:?}"
        );
        assert!(
            first.deadline <= first.armed_at,
            "an adopted arm is grounded in the row's unknown freshness, not in \
             join time: this one is {:?} out, where a full-period arm would be \
             {period:?} out and land after the row's own expiry",
            first.deadline.saturating_duration_since(first.armed_at)
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refresh_adopted, 1,
            "and it must be counted exactly once: {state:?}"
        );

        // The renewal really lands on that schedule, and its re-arm is a
        // DISTINCT, later record - the successor a post-hoc sample would have
        // mistaken for this decision.
        //
        // Wait for the successor's own PUBLICATION, not for the renewal's
        // effect counter: that counter is incremented inside the refresh,
        // before the worker returns and arms the next period, so the interval
        // between them is a real one in which no successor record exists yet.
        let successor = {
            let decisions = Arc::clone(&decisions);
            let published = move || {
                decisions
                    .lock()
                    .iter()
                    .find(|decision| decision.seq > first.seq)
                    .copied()
            };
            until(
                &node,
                Duration::from_secs(5),
                "the aged row was renewed and re-armed",
                || published().is_some(),
            )
            .await;
            published().expect("the successor decision, just observed")
        };
        assert!(
            node.org_sensing_demand_state_for_test().refresh_renewed >= 1,
            "a successor arm implies the renewal it follows was counted first"
        );
        assert!(row_present(&node, provider));
        assert_eq!(
            successor.provenance,
            SensingArmProvenance::Established,
            "a renewal re-registered the row, so its next period is grounded in \
             the renewal: {successor:?}"
        );
        assert!(
            successor.deadline > successor.armed_at,
            "and it is a full period out, unlike the adoption: {successor:?}"
        );
        assert_eq!(
            successor.installation_id, first.installation_id,
            "the renewal renews the installation the join adopted - a successor \
             naming another one would be a different row's cadence, not this \
             decision's: {successor:?} vs {first:?}"
        );

        node.clear_sensing_arm_seam_for_test();
        let _ = node.try_release_sensing_interest_lease(public);
        drop(demand);
        drop(family);
    }

    /// A COMPLETED renewal and re-arm is not the adoption decision.
    ///
    /// This drives the exact schedule that a post-hoc `armed` sample cannot
    /// survive: the join installs the due-now adoption arm, the real worker
    /// dequeues it, renews, and re-arms `Established` a full period out - all
    /// before the observer reads. The record then names the SUCCESSOR, whose
    /// deadline is legitimately in the future and outside the joined row's
    /// original horizon.
    ///
    /// Production is untouched here; only the observation point differs. The
    /// decision the schedule took is still exactly one due-now adoption.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_completed_rearm_is_not_the_adoption_decision() {
        let ttl = Duration::from_secs(2);
        let node = demand_node("adopt-rearm", ttl).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);
        let interval = SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl());

        let decisions: Arc<parking_lot::Mutex<Vec<SensingArmDecision>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        {
            let decisions = Arc::clone(&decisions);
            node.set_sensing_arm_seam_for_test(Arc::new(move |decision| {
                decisions.lock().push(decision);
            }));
        }

        let public = node
            .acquire_sensing_interest_lease(&spec_for(&node, provider), provider, interval)
            .expect("the public holder establishes");
        let established_at = Instant::now();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let demand = family.reconcile(TAG, &[provider]).expect("retain");

        // WAIT for the successor decision to be PUBLISHED. This is the
        // interleaving an observer descheduled after the join sees, and the
        // record itself is the barrier - the renewal's effect counter moves
        // before the successor arm exists, and a mutable re-read of `armed`
        // could name an even later tick.
        let (first, successor) = {
            let decisions = Arc::clone(&decisions);
            let pair = move || {
                let all = decisions.lock();
                let first = *all.first()?;
                let successor = all.iter().find(|next| next.seq > first.seq).copied()?;
                Some((first, successor))
            };
            until(
                &node,
                Duration::from_secs(5),
                "the worker renewed and re-armed",
                || pair().is_some(),
            )
            .await;
            pair().expect("the pair, just observed")
        };

        // The successor is what ANY later sample of the schedule names: a
        // strictly later record, whose deadline was already in the future when
        // it was taken and lies past the joined row's own horizon. Asserting
        // "due now" or "inside the original ttl" on it rejects real progress,
        // which is exactly why the decision under test is captured instead.
        assert!(
            successor.deadline > successor.armed_at && successor.deadline >= established_at + ttl,
            "precondition: the successor's deadline is legitimately ahead and \
             past the horizon: {:?} ahead of its own arm, {:?} after the \
             public establishment of a {ttl:?} row",
            successor
                .deadline
                .saturating_duration_since(successor.armed_at),
            successor.deadline.saturating_duration_since(established_at)
        );
        // A live sample of the map is corroboration only, never the evidence:
        // it may be taken during a later tick's own dequeue-to-re-arm interval,
        // when no record exists at all. When one IS there it can only be a
        // record at or after the successor, never the adoption again.
        if let Some((_, sampled_seq)) = node.sensing_refresh_arm_for_test(&key) {
            assert!(
                sampled_seq >= successor.seq,
                "a sample of the schedule never names the adoption record again \
                 - seq {sampled_seq} against the successor's {} and the \
                 decision's {}",
                successor.seq,
                first.seq
            );
        }

        // And the DECISION is still exactly one due-now adoption.
        assert_eq!(first.provenance, SensingArmProvenance::Adopted, "{first:?}");
        assert!(first.deadline <= first.armed_at, "{first:?}");
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refresh_adopted, 1,
            "the completed renewal must not be counted as a second adoption: {state:?}"
        );
        assert!(row_present(&node, provider));

        node.clear_sensing_arm_seam_for_test();
        let _ = node.try_release_sensing_interest_lease(public);
        drop(demand);
        drop(family);
    }

    /// An INVALIDATION discharges the retained releases it killed, at the
    /// transition itself.
    ///
    /// That is what keeps the retention set a ledger of live ownership, so an
    /// admission decision never has to prove liveness off-lock — and can never
    /// reject a live ticket against a state it did not examine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invalidation_discharges_the_releases_it_killed() {
        let node =
            emitter_demand_node("discharge", Duration::from_secs(30), Duration::from_secs(1)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id();
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let slow = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, provider),
                provider,
                Duration::from_secs(4),
            )
            .expect("the looser holder acquires");

        node.clear_node_authority_for_test();
        drop(demand);
        family.retire(TAG);
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .refused_release_outstanding,
            1,
            "precondition: the refused release is retained"
        );

        node.install_node_authority(adopt(&node, &org(), "discharge-again"))
            .expect("reinstall org authority");
        invalidate_installation(&node, provider);

        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refused_release_outstanding, 0,
            "the invalidation must discharge ownership it killed: {state:?}"
        );
        assert_eq!(state.refused_release_reclaimed, 1, "{state:?}");
        let _ = node.try_release_sensing_interest_lease(slow);
        drop(family);
    }

    /// A fresh admission at the ceiling KEEPS the ticket.
    ///
    /// The retention set is an ownership ledger, not a budget: its size is
    /// bounded by the registry's own live-holder capacity. A capacity rejection
    /// would have to be justified against the state it was decided on, and a
    /// check-then-lock pair cannot do that — two admissions can both observe
    /// `N - 1`, one appends, and the other abandons the sole release capability
    /// of a live holder. So admission never rejects; going over the derived
    /// ceiling is counted loudly instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fresh_admission_at_the_ceiling_keeps_its_live_ticket() {
        let node = demand_node("ceiling", Duration::from_millis(2500)).await;
        // ONE slot, so the ceiling decision is reachable with two real
        // refusals instead of sixteen thousand.
        node.set_refused_release_cap_for_test(1);
        let (first_owner, second_owner) = (
            OrgSensingFamily::mint(&node).expect("mint first"),
            OrgSensingFamily::mint(&node).expect("mint second"),
        );
        let (first, second) = (
            node.node_id().wrapping_add(1),
            node.node_id().wrapping_add(2),
        );
        let (first_key, second_key) = (lease_key_for(&node, first), lease_key_for(&node, second));
        let first_demand = first_owner.reconcile(TAG, &[first]).expect("retain first");
        let second_demand = second_owner
            .reconcile(TAG, &[second])
            .expect("retain second");
        // LOOSER independent holders, so both family releases must re-register
        // a relaxed aggregate — the transaction current authority can refuse.
        let slow_first = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, first),
                first,
                Duration::from_millis(2500),
            )
            .expect("acquire");
        let slow_second = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, second),
                second,
                Duration::from_millis(2500),
            )
            .expect("acquire");

        node.clear_node_authority_for_test();
        drop(first_demand);
        drop(second_demand);
        first_owner.retire(TAG);
        second_owner.retire(TAG);

        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refused_release_unowned, 0,
            "a live refused ticket was abandoned at the ceiling: {state:?}"
        );
        assert_eq!(
            state.refused_release_outstanding, 2,
            "both live holders must still have an owner: {state:?}"
        );
        assert_eq!(
            state.refused_release_overflow, 1,
            "and going over the derived ceiling must be loud, not silent: {state:?}"
        );
        assert_eq!(holders(&node, &first_key), Some(2), "neither release moved");
        assert_eq!(holders(&node, &second_key), Some(2));

        node.set_refused_release_cap_for_test(usize::MAX);
        let _ = node.try_release_sensing_interest_lease(slow_first);
        let _ = node.try_release_sensing_interest_lease(slow_second);
        drop(first_owner);
        drop(second_owner);
    }

    // ---- DISCOVERY PROVENANCE HELPERS -----------------------------------

    /// Announce a REAL owner-scoped capability for a fresh same-organization
    /// provider through the production verified-ingest path, and pin it onto a
    /// node id exactly as a session would. Returns the provider's entity.
    fn announce_owner_provider(node: &Arc<MeshNode>) -> crate::adapter::net::identity::EntityId {
        use crate::adapter::net::behavior::capability::CapabilitySet;
        use crate::adapter::net::behavior::org::current_timestamp;
        use crate::adapter::net::behavior::org_scoped_ann::ScopedCapabilityAnnouncement;

        let provider_kp = EntityKeypair::generate();
        let provider_entity = provider_kp.entity_id().clone();
        let authority = node.node_authority().expect("authority");
        let cert = OrgMembershipCert::try_issue(&org(), provider_entity.clone(), 1, 3600)
            .expect("provider cert");
        let descriptor = CapabilitySet::new().add_tag(TAG).to_bytes_compact();
        let envelope = ScopedCapabilityAnnouncement::build_owner(
            &provider_kp,
            org().org_id(),
            cert,
            authority.audience.audience_handle,
            authority.audience.discovery_key(),
            1,
            current_timestamp() + 3600,
            &descriptor,
        )
        .expect("owner envelope");
        node.ingest_scoped_announcement_for_test(&envelope.to_bytes());
        provider_entity
    }

    /// Raise `provider`'s revocation floor above the generation its
    /// certificate carries, through the production store.
    fn raise_provider_floor(
        node: &Arc<MeshNode>,
        provider: &crate::adapter::net::identity::EntityId,
    ) {
        use crate::adapter::net::behavior::org::OrgRevocationBundle;
        use std::collections::BTreeMap;

        let mut floors = BTreeMap::new();
        floors.insert(provider.clone(), 5u32);
        let bundle = OrgRevocationBundle::try_issue(&org(), &floors).expect("bundle");
        node.org_revocation_store()
            .expect("store")
            .apply_bundle(&bundle)
            .expect("the floor raise must publish");
    }

    /// A NONEMPTY population invalidated INSIDE the capture/query/currentness
    /// window is re-derived, and the published population is the re-derived
    /// one.
    ///
    /// The empty-population version of this witness cannot discriminate: an
    /// implementation that queried once outside the retry loop while still
    /// redoing capture and currentness each attempt would pass it, because
    /// every attempt's population is `[]` either way. Here attempt one sees a
    /// real provider and attempt two must not, so the returned population is
    /// the discriminating observation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_nonempty_population_is_rederived_when_the_view_moves_in_window() {
        let node = demand_node("in-window-rederive", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let capability = CapabilityAuthorityId::for_tag(TAG);

        let provider_entity = announce_owner_provider(&node);
        let provider_node = node.node_id().wrapping_add(7);
        node.pin_peer_entity_for_test(provider_node, provider_entity.clone());
        assert_eq!(
            node.org_sensing_authorized_population(&capability),
            vec![provider_node],
            "precondition: a real verified provider is in the population"
        );

        // ONE-SHOT, inside the window between the population derivation and
        // the captured view's currentness re-proof: retire the provider's
        // record with a real floor raise. That both moves the qualifying view
        // AND empties the population, so the two attempts see different
        // populations.
        let fired = Arc::new(AtomicU64::new(0));
        {
            let node = node.clone();
            let fired = fired.clone();
            let provider_entity = provider_entity.clone();
            let seam = node.clone();
            seam.set_sensing_population_seam_for_test(Arc::new(move || {
                if fired.fetch_add(1, AtomicOrdering::SeqCst) > 0 {
                    return;
                }
                raise_provider_floor(&node, &provider_entity);
            }));
        }

        let retained = family
            .retain(TAG)
            .expect("retain must re-derive, not refuse");
        node.clear_sensing_population_seam_for_test();
        assert_eq!(
            fired.load(AtomicOrdering::SeqCst),
            2,
            "the population must have been derived TWICE"
        );
        assert!(
            retained.population().is_empty(),
            "the PUBLISHED population is the first attempt's, cached across the \
             re-derivation: {:?}",
            retained.population()
        );
        assert!(
            retained.retained_providers().is_empty(),
            "and nothing may be retained for a provider the moved view retired"
        );
        assert!(
            !row_present(&node, provider_node),
            "so no row exists for it either"
        );
        assert!(
            retained.authority_is_current(),
            "the published stamp is the one that qualified the empty population"
        );
        assert_eq!(node.org_sensing_demand_state_for_test().retained, 0);

        drop(retained);
        drop(family);
    }

    // ---- COHERENT OBSERVATION AND ADMISSION BOUNDARIES ------------------

    /// An independent INVALIDATION during a convergence's carry validation is
    /// legitimate movement, not a broken invariant.
    ///
    /// The validation observes membership and installation in one registry
    /// read. Two separate reads are two instants: a public self-provider
    /// tightening whose restoration current authority refuses invalidates the
    /// row in between, and the second observation then contradicts the first
    /// about a ticket that was valid when it was committed. Asserting on that
    /// panicked debug-assertion builds — dev/test and any build with debug
    /// assertions on, not ordinary optimized release builds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invalidation_during_carry_validation_is_legitimate_movement() {
        let node = emitter_demand_node(
            "carry-window",
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        // SELF provider: the only branch with a local emitter to refuse.
        let provider = node.node_id();
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(TAG, &[provider]).expect("retain");
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        assert!(row_present(&node, provider));

        // ONE-SHOT, inside the carry loop's own window: kill the installation
        // the ticket just validated against, through the production path.
        let fired = Arc::new(AtomicBool::new(false));
        {
            let node = node.clone();
            let fired = fired.clone();
            let seam = node.clone();
            seam.set_sensing_carry_validated_seam_for_test(Arc::new(move || {
                if fired.swap(true, AtomicOrdering::SeqCst) {
                    return;
                }
                invalidate_installation(&node, provider);
            }));
        }

        // This must not PANIC. The observation was valid at its instant, so
        // the ticket is legitimately carried by this pass; the invalidation is
        // movement that happened afterwards.
        let second = family.reconcile(TAG, &[provider]).expect("re-converge");
        node.clear_sensing_carry_validated_seam_for_test();
        assert!(
            fired.load(AtomicOrdering::SeqCst),
            "the carry window seam must have fired, or this witness proves nothing"
        );
        assert_eq!(second.retained_providers(), vec![provider]);
        assert_eq!(
            node.sensing_refresh_installation(&key),
            None,
            "precondition: the invalidation really did land inside the window"
        );

        // And the NEXT convergence repairs it: no permanent successor
        // misattribution survives a legitimately stale carry.
        let third = family
            .reconcile(TAG, &[provider])
            .expect("repairing converge");
        let fresh = node
            .sensing_refresh_installation(&key)
            .expect("a fresh installation");
        assert!(
            fresh > installation,
            "the re-acquisition must be a strictly newer installation"
        );
        assert!(row_present(&node, provider));
        let (_, live_ticket, live_installation) = third.retained_holders_for_test()[0];
        assert_eq!(
            node.sensing_lease_holder_installation(&live_ticket),
            Some(live_installation),
            "and the repaired pair must be a real, committed one"
        );
        assert!(
            node.org_sensing_demand_state_for_test()
                .ownership_invalidated
                >= 1,
            "the dead carry must be counted when it is next observed"
        );

        drop(first);
        drop(second);
        drop(third);
        drop(family);
    }

    /// A ledger admission cannot slip past the discharge that killed its
    /// ticket.
    ///
    /// The liveness check and the append happen under the SAME schedule lock
    /// the invalidation's discharge takes, so either the discharge got there
    /// first and this admission finds a dead ticket, or the admission holds the
    /// lock and the discharge sees the appended entry. A "sample, then lock and
    /// append" pair could leave a stale entry the discharge had already looked
    /// for and not found, and pending state would then scale with caller
    /// concurrency rather than with the registry's live-holder capacity.
    ///
    /// Nothing live is lost either way: a discharged admission means the
    /// holder is already gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_admission_after_its_invalidation_owns_nothing() {
        let node = emitter_demand_node(
            "admission-window",
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id();
        let key = lease_key_for(&node, provider);
        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        let (_, ticket, installation) = demand.retained_holders_for_test()[0];
        // A LOOSER independent holder, so the family's release has to
        // re-register a relaxed aggregate — the transaction authority refuses.
        let slow = node
            .acquire_sensing_interest_lease(
                &spec_for(&node, provider),
                provider,
                Duration::from_secs(4),
            )
            .expect("the looser holder acquires");

        // Kill the installation FIRST, then offer its ticket to the ledger —
        // the state a delayed in-flight admission resumes into.
        invalidate_installation(&node, provider);
        assert!(
            !node.holds_sensing_lease_token(&ticket),
            "precondition: the ticket owns nothing any more"
        );
        let outcome = MeshNode::park_refused_release(&node, ticket, installation, provider);
        assert_eq!(
            outcome,
            RefusedReleaseOutcome::Discharged,
            "an admission whose holder is gone must be discharged, not appended"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refused_release_outstanding, 0,
            "a dead ticket must never enter the ledger: {state:?}"
        );
        assert_eq!(state.refused_release_parked, 0, "{state:?}");
        assert_eq!(
            state.refused_release_unowned, 0,
            "and it is not a loss — there is nothing left to own: {state:?}"
        );
        assert_eq!(state.refused_release_reclaimed, 1, "{state:?}");

        // A LIVE ticket still gets an owner, so the check is not simply
        // refusing everything.
        assert!(node.sensing_refresh_installation(&key).is_none());
        let second = family.reconcile(TAG, &[provider]).expect("re-converge");
        let (_, live_ticket, live_installation) = second.retained_holders_for_test()[0];
        node.clear_node_authority_for_test();
        assert_eq!(
            MeshNode::park_refused_release(&node, live_ticket, live_installation, provider),
            RefusedReleaseOutcome::Retained,
            "a live holder must still be retained"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .refused_release_outstanding,
            1
        );

        let _ = node.try_release_sensing_interest_lease(slow);
        drop(demand);
        drop(second);
        drop(family);
    }

    /// The CALLER consumes the acquisition's returned facts.
    ///
    /// The owned helper being correct is not enough: a caller that went back to
    /// sampling the key before and after the acquisition would still pass the
    /// helper's own witness and the sequential public-first adoption witness.
    /// Here a public holder establishes the row INSIDE the window where that
    /// pre-sample would have run, so a pre-sample would read `None`, call the
    /// coalescing join "establishing", and arm a full period out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_concurrent_establishment_cannot_make_a_join_look_established() {
        let node = demand_node("caller-facts", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);
        let interval = SENSING_SAMPLE_INTERVAL.min(node.sensing_interest_ttl());

        // ONE-SHOT, in the pre-acquisition window: a public holder establishes
        // the row at the cadence the family is about to ask for.
        let public = Arc::new(parking_lot::Mutex::new(None));
        let fired = Arc::new(AtomicBool::new(false));
        {
            let node = node.clone();
            let public = public.clone();
            let fired = fired.clone();
            let seam = node.clone();
            seam.set_sensing_pre_acquire_seam_for_test(Arc::new(move || {
                if fired.swap(true, AtomicOrdering::SeqCst) {
                    return;
                }
                let ticket = node
                    .acquire_sensing_interest_lease(&spec_for(&node, provider), provider, interval)
                    .expect("the public holder establishes");
                *public.lock() = Some(ticket);
            }));
        }

        let demand = family.reconcile(TAG, &[provider]).expect("retain");
        node.clear_sensing_pre_acquire_seam_for_test();
        assert!(
            fired.load(AtomicOrdering::SeqCst),
            "the pre-acquisition window seam must have fired"
        );
        assert_eq!(
            holders(&node, &key),
            Some(2),
            "precondition: the family JOINED the row the seam established"
        );
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(
            state.refresh_adopted, 1,
            "the caller ignored the acquisition's returned freshness: a row \
             established concurrently was called establishing, so an \
             arbitrarily old row would be armed a full period out: {state:?}"
        );
        // And the recorded installation is the one the COMMIT reported, which
        // is the row the family actually joined.
        let (_, ticket, installation) = demand.retained_holders_for_test()[0];
        assert_eq!(
            node.sensing_lease_holder_installation(&ticket),
            Some(installation),
            "the recorded pair must be the committed one"
        );

        let taken = public.lock().take().expect("the public ticket");
        let _ = node.try_release_sensing_interest_lease(taken);
        drop(demand);
        drop(family);
    }
}
