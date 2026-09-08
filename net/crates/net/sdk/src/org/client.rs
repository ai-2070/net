//! OSDK §2 — [`OrgClient`] and the binding verb [`Mesh::org`].
//!
//! Binding is where a credential set stops being data and becomes an operating
//! capability: it pins the set to a durable mesh identity and an installed node
//! authority, and leases the consumer audiences private discovery needs.
//!
//! S0 lands the binding relation and the lease. The call verb (`org.call`)
//! lands in S1.

#[cfg(feature = "cortex")]
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(feature = "cortex")]
use std::time::{Duration, Instant};

#[cfg(feature = "cortex")]
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
#[cfg(feature = "cortex")]
use net::adapter::net::behavior::org_sensing_demand::OrgSensingFamily;
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::MeshNode;

use super::credentials::OrgCredentials;
use super::error::{hex32, OrgCredentialError, OrgSdkError};
use super::lease::AudienceLeaseGuard;
use super::types::{OrgCapabilityGrant, OrgDispatcherGrant, OrgId, OrgMembershipCert};
use crate::mesh::Mesh;

/// This client's organization-sensing acquisition, decided ONCE at bind.
///
/// Two-state by construction. The family mint is fallible — the routing
/// registry's family identity space is bounded and terminal — and a binding
/// that cannot sense is INERT, not broken: it plans in the deterministic
/// unsensed order and calls exactly as it always did. So `bind_node` MAPS the
/// mint result instead of propagating it, and gains no error variant.
///
/// `Active` holds one `Arc`-shared family body plus this binding's own
/// reconciliation schedule. Cloning the binding shares both, so every clone of
/// an [`OrgClient`] shares one acquisition: an intermediate clone's drop
/// retires nothing, and the last one retires everything (the family body's own
/// `Drop`). `Inert` owns nothing, so its drop is a no-op and there is no
/// per-call re-mint to hammer an exhausted space — recovery is a new bind.
///
/// `pub(crate)`: this type is not part of any public signature. What a witness
/// can observe about it is a small set of `#[doc(hidden)]`, test/fixtures-only
/// DATA accessors on [`OrgClient`] — never the cloneable family handle itself.
#[cfg(feature = "cortex")]
#[derive(Clone)]
pub(crate) enum OrgSensingBinding {
    /// The mint succeeded; this client shares one acquisition.
    Active(OrgSensingAcquisition),
    /// The mint was refused. Deterministic unsensed planning, no sensing work.
    Inert,
}

#[cfg(feature = "cortex")]
impl OrgSensingBinding {
    /// The acquisition, when this binding is active.
    pub(crate) fn acquisition(&self) -> Option<&OrgSensingAcquisition> {
        match self {
            Self::Active(acquisition) => Some(acquisition),
            Self::Inert => None,
        }
    }
}

/// One binding's acquisition: the shared family, the shared record of what
/// each capability's demand was last certified as, and the lock that makes a
/// reconciliation decision one section.
#[cfg(feature = "cortex")]
#[derive(Clone)]
pub(crate) struct OrgSensingAcquisition {
    family: OrgSensingFamily,
    schedule: Arc<ConvergenceSchedule>,
}

/// What each capability's demand was last CERTIFIED as.
///
/// Bounded in both directions. An unchanged, fully certified population must
/// not be re-acquired on every call; a changed one — a newly discovered
/// provider, a departed one, a pin that came or went, a holder that died —
/// must not stay frozen. And the record set itself is capped: a caller that
/// walks many capabilities cannot grow it without limit.
///
/// A record only certifies the demand it was taken FROM: it carries that
/// demand's identity and the population core published, so a demand replaced
/// or retired by another clone is never vouched for by an older record.
#[cfg(feature = "cortex")]
#[derive(Default)]
pub(crate) struct ConvergenceSchedule {
    records: parking_lot::Mutex<BTreeMap<CapabilityAuthorityId, ConvergedFor>>,
    /// Serializes decide → converge → record for this binding, so two clones
    /// cannot both decide to converge one change. Held only across synchronous
    /// work.
    reconcile: parking_lot::Mutex<()>,
}

/// The record: what was certified, about which demand, and when.
#[cfg(feature = "cortex")]
struct ConvergedFor {
    /// The expectation the convergence was driven by — this call's own pinned
    /// same-organization candidates. Compared by value, so a discovery or pin
    /// change is the trigger rather than a clock.
    expected: Vec<u64>,
    /// The identity of the demand this record describes. A demand replaced or
    /// retired since then invalidates the record outright.
    demand: usize,
    /// The population core actually published for that demand.
    certified: Vec<u64>,
    /// Whether the certified demand covers the whole expectation AND holds a
    /// holder for every member of its own population. Anything less is
    /// retryable on a floor.
    complete: bool,
    /// When the attempt ran, so an incomplete one is retried on a floor.
    attempted: Instant,
}

/// The most capabilities one binding keeps convergence records for.
///
/// A bound of its own, not core's: it exists so a caller walking many
/// capability names cannot grow this map without limit. Evicting the least
/// recently attempted record costs at most one later convergence, because
/// every reuse decision re-derives its inputs anyway.
#[cfg(feature = "cortex")]
const MAX_CONVERGENCE_RECORDS: usize = 64;

#[cfg(feature = "cortex")]
impl ConvergenceSchedule {
    /// Whether `expected` demands a convergence now, given `installed` — the
    /// demand currently in force for that capability.
    ///
    /// `retry_floor` bounds the only time-driven case: an incomplete
    /// certification, which must be retryable without turning every later call
    /// into an acquisition.
    pub(crate) fn needs_convergence(
        &self,
        capability: &CapabilityAuthorityId,
        expected: &[u64],
        installed: Option<
            &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
        >,
        now: Instant,
        retry_floor: Duration,
    ) -> bool {
        let Some(installed) = installed else {
            return true; // No demand at all: nothing to reuse.
        };
        // The demand's OWN state comes first: neither a moved sensing
        // authority nor a dead holder is visible in any record.
        if !installed.authority_is_current() || !installed.holders_are_live() {
            return true;
        }
        match self.records.lock().get(capability) {
            None => true,
            Some(record) => {
                record.demand != Arc::as_ptr(installed) as usize
                    || record.expected != expected
                    || record.certified != population_of(installed)
                    || (!record.complete
                        && now.saturating_duration_since(record.attempted) >= retry_floor)
            }
        }
    }

    /// Certify `demand` for `capability` under `expected`.
    ///
    /// COMPLETE means two things at once: core published a population that
    /// covers the whole expectation, and every member of that population has a
    /// live holder. A population narrower than the expectation — a discovery
    /// row that expired between this call's capture and core's own query — is
    /// therefore recorded as incomplete and retried, unless a retry already
    /// produced exactly the same population, which is a fixed point rather
    /// than a loop.
    pub(crate) fn certify(
        &self,
        capability: CapabilityAuthorityId,
        expected: Vec<u64>,
        demand: &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
        now: Instant,
    ) {
        let certified = population_of(demand);
        let mut retained = demand.retained_providers();
        retained.sort_unstable();
        retained.dedup();
        let holders_complete = retained == certified;
        let covers_expectation = expected
            .iter()
            .all(|id| certified.binary_search(id).is_ok());
        let settled = {
            let records = self.records.lock();
            records.get(&capability).is_some_and(|previous| {
                previous.expected == expected && previous.certified == certified
            })
        };
        let complete = holders_complete && (covers_expectation || settled);
        self.insert(
            capability,
            ConvergedFor {
                expected,
                demand: Arc::as_ptr(demand) as usize,
                certified,
                complete,
                attempted: now,
            },
        );
    }

    /// Record a REFUSED convergence: nothing new is certified, and the attempt
    /// is floored so a persistent refusal is not retried per call.
    pub(crate) fn record_refusal(
        &self,
        capability: CapabilityAuthorityId,
        expected: Vec<u64>,
        now: Instant,
    ) {
        self.insert(
            capability,
            ConvergedFor {
                expected,
                // No demand is certified by a refusal, and `0` is not a live
                // `Arc` address, so any installed demand invalidates it.
                demand: 0,
                certified: Vec::new(),
                complete: false,
                attempted: now,
            },
        );
    }

    /// Forget `capability`'s record — used when its demand is retired, so
    /// nothing certifies a demand that no longer exists.
    pub(crate) fn forget(&self, capability: &CapabilityAuthorityId) {
        self.records.lock().remove(capability);
    }

    /// Insert, evicting the least recently attempted record at the bound.
    fn insert(&self, capability: CapabilityAuthorityId, record: ConvergedFor) {
        let mut records = self.records.lock();
        if records.len() >= MAX_CONVERGENCE_RECORDS && !records.contains_key(&capability) {
            if let Some(oldest) = records
                .iter()
                .min_by_key(|(_, record)| record.attempted)
                .map(|(capability, _)| *capability)
            {
                records.remove(&oldest);
            }
        }
        records.insert(capability, record);
    }

    /// How many capabilities this binding holds records for (test/fixtures).
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn len(&self) -> usize {
        self.records.lock().len()
    }
}

/// A demand's published population, ascending — the comparison basis for every
/// record.
#[cfg(feature = "cortex")]
fn population_of(
    demand: &net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand,
) -> Vec<u64> {
    let mut population = demand.population().to_vec();
    population.sort_unstable();
    population.dedup();
    population
}

#[cfg(feature = "cortex")]
impl OrgSensingAcquisition {
    pub(crate) fn new(family: OrgSensingFamily) -> Self {
        Self {
            family,
            schedule: Arc::new(ConvergenceSchedule::default()),
        }
    }

    pub(crate) fn family(&self) -> &OrgSensingFamily {
        &self.family
    }

    pub(crate) fn schedule(&self) -> &ConvergenceSchedule {
        &self.schedule
    }

    /// Take the reconciliation section for this binding.
    pub(crate) fn reconcile_lock(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.schedule.reconcile.lock()
    }
}

/// A credential set bound to a live mesh — the caller half of the org facade.
///
/// Obtained from [`Mesh::org`]. Cloning shares one audience lease rather than
/// taking a second reference, so clones are free and dropping a clone never
/// withdraws another clone's ingest authority.
#[derive(Clone)]
pub struct OrgClient {
    /// The node this client calls through. Used by the call verb, which rides
    /// the nRPC surface.
    #[cfg_attr(not(feature = "cortex"), allow(dead_code))]
    pub(crate) node: Arc<MeshNode>,
    /// The mesh's durable identity — signs every proof this client mints.
    pub(crate) caller: Arc<EntityKeypair>,
    pub(crate) membership: OrgMembershipCert,
    pub(crate) dispatcher: OrgDispatcherGrant,
    pub(crate) grants: Vec<OrgCapabilityGrant>,
    pub(crate) acting_org: OrgId,
    /// Clock-skew tolerance from the installed authority — the same tolerance
    /// the provider applies, so local temporal checks agree with remote ones.
    pub(crate) skew_secs: u64,
    /// Dropped with the last clone; releases the consumer-audience references.
    pub(crate) _lease: Arc<AudienceLeaseGuard>,
    /// This client's sensing acquisition, minted once at bind. Shared by every
    /// clone exactly like `_lease`; the last clone's drop retires the demand.
    #[cfg(feature = "cortex")]
    pub(crate) _sensing: OrgSensingBinding,
}

impl OrgClient {
    /// The organization this client acts for.
    pub fn acting_org(&self) -> OrgId {
        self.acting_org
    }

    /// The entity this client calls as (the mesh's durable identity).
    pub fn caller(&self) -> &net::adapter::net::identity::EntityId {
        self.caller.entity_id()
    }

    /// The held cross-org capability grants.
    pub fn grants(&self) -> &[OrgCapabilityGrant] {
        &self.grants
    }

    /// Whether this client's sensing binding is active.
    ///
    /// # The test/fixtures observation seam
    ///
    /// This and the four accessors below are the WHOLE observable surface of
    /// the sensing binding, they are `#[doc(hidden)]` and gated to
    /// test/`fixtures` builds, and they return plain DATA — never the
    /// cloneable family handle, which would hand a caller ownership of this
    /// binding's acquisition. They exist because the binding's contract is
    /// about ownership and reconciliation, and neither is observable from a
    /// reply body. Nothing in production reads them.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_is_active(&self) -> bool {
        self._sensing.acquisition().is_some()
    }

    /// How many owners share this binding's acquisition, or `None` if inert.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_owners(&self) -> Option<usize> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.family().owners())
    }

    /// The capabilities this binding currently retains demand for.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_capabilities(&self) -> Vec<CapabilityAuthorityId> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.family().capabilities())
            .unwrap_or_default()
    }

    /// One capability's retained demand as plain data:
    /// `(population, retained_holders, demand_identity)`.
    ///
    /// The identity is the retained `Arc`'s address, which is how a witness
    /// distinguishes "the same demand was reused" from "an identical one was
    /// re-acquired". It is an opaque number, not a handle.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_demand_state(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<(Vec<u64>, Vec<u64>, usize)> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        let identity = std::sync::Arc::as_ptr(&demand) as usize;
        Some((
            demand.population().to_vec(),
            demand.retained_providers(),
            identity,
        ))
    }

    /// How many capabilities this binding holds convergence records for.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_records(&self) -> Option<usize> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.schedule().len())
    }

    /// Whether core still considers every retained holder of this client's
    /// demand to be the installation it was committed with.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_holders_are_live(&self, capability: &CapabilityAuthorityId) -> Option<bool> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        Some(demand.holders_are_live())
    }

    /// RELEASE the demand's own holder tickets, killing their ownership while
    /// leaving the demand's recorded provider list untouched.
    ///
    /// Test/fixtures only. It is how a witness reaches the state a refused
    /// restoration under an older authority leaves behind — a recorded ticket
    /// that is no longer the holder — through the shipped release verb rather
    /// than by fabricating one.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_release_holders_for_test(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<usize> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        let mut released = 0;
        for (_, ticket, _) in demand.retained_holders_for_test() {
            if self.node.try_release_sensing_interest_lease(ticket).is_ok() {
                released += 1;
            }
        }
        Some(released)
    }

    /// One capability's readiness projection at a caller-supplied instant.
    ///
    /// The instant is the point: freshness is request-relative, so a witness
    /// proves aging by ASKING at a later instant rather than by sleeping.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_projection(
        &self,
        capability: &CapabilityAuthorityId,
        at: Instant,
        budget: &net::adapter::net::behavior::sensing::ConsumerLatencyBudget,
    ) -> Option<net::adapter::net::behavior::org_sensing_demand::OrgSensedProjection> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        Some(demand.project_sensed_order(at, budget))
    }

    /// The membership certificate this client calls under.
    pub fn membership(&self) -> &OrgMembershipCert {
        &self.membership
    }

    /// The dispatcher grant this client calls under.
    pub fn dispatcher(&self) -> &OrgDispatcherGrant {
        &self.dispatcher
    }

    /// Stage-3 of the validity contract: are the credentials that back EVERY
    /// call currently within their validity windows?
    ///
    /// `call` performs this itself (plus the selected grant), so this is for
    /// callers that want to check before committing to work — a long-lived
    /// client crosses expiry, and a bound client is not a permanently valid
    /// one. Uses the installed authority's skew tolerance, so it agrees with
    /// the provider's own window arithmetic.
    pub fn check_current(&self) -> Result<(), OrgCredentialError> {
        // `current_timestamp` is the canonical wall clock the credential family
        // uses; taking our own `SystemTime` here would be a second clock that
        // could disagree with the one the windows were minted against. A call
        // arriving through `plan` uses its capture's instant instead — see
        // [`Self::check_current_at`].
        self.membership
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "membership".to_string(),
                source,
            })?;
        self.dispatcher
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;
        Ok(())
    }

    /// [`Self::check_current`] at the cold plan's CAPTURED instant
    /// (OLB-2B.3d-pre).
    ///
    /// Every call goes through here rather than through the sampling twin: two
    /// independent clock samples let a plan pass the membership window at one
    /// instant and the dispatcher window at another, so a credential set that
    /// was never simultaneously valid could authorize a call. One captured
    /// instant makes that unrepresentable, and it is the SAME instant the grant
    /// windows, the discovery filters and the expiry checks use.
    // Its only consumers are the call verb's two currentness checks in
    // `org/call.rs`, and that module is `#[cfg(feature = "cortex")]`
    // (`org.rs`). `net-aggregator-daemon` is the one workspace build that
    // takes this SDK without `cortex`, and `ffi-clippy` lints it at
    // `-D warnings`, so there the method is genuinely dead. Same shape as
    // the `node` field above and `hex_capability` in `org/error.rs`.
    #[cfg_attr(not(feature = "cortex"), allow(dead_code))]
    pub(crate) fn check_current_at(&self, now_secs: u64) -> Result<(), OrgCredentialError> {
        self.membership
            .is_valid_at_with_skew(now_secs, self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "membership".to_string(),
                source,
            })?;
        self.dispatcher
            .is_valid_at_with_skew(now_secs, self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;
        Ok(())
    }
}

impl std::fmt::Debug for OrgClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgClient")
            .field("acting_org", &self.acting_org)
            .field("caller", self.caller.entity_id())
            .field("grants", &self.grants.len())
            .finish()
    }
}

impl OrgClient {
    /// Bind a credential set to a NODE — the one implementation of the bind
    /// pipeline (OSDK-L N).
    ///
    /// [`Mesh::org`] delegates here, and so does every language binding: the
    /// Node, Python, and Go/C surfaces hold `Arc<MeshNode>` rather than an SDK
    /// [`Mesh`], and neither fabricating a throwaway `Mesh` per bind nor
    /// pinning a permanent one is acceptable — the first makes
    /// `Mesh::from_node_arc` an accidental binding adapter, the second adds a
    /// permanent `Arc<MeshNode>` that blocks shutdown.
    ///
    /// `#[doc(hidden)]` because applications should use [`Mesh::org`]; this is
    /// the binding seam, not a second public way to do the same thing. There is
    /// exactly one authority pipeline and both doors reach it.
    ///
    /// Refuses unless the complete private-discovery identity relation holds:
    ///
    /// 1. the node's identity was EXPLICITLY configured — org membership binds
    ///    to a durable cryptographic entity, never a generated ephemeral
    ///    keypair whose entity id changes on restart;
    /// 2. a node authority is installed — consumer-audience installation and
    ///    owner-private discovery both require it, so binding without one would
    ///    search private state that can never exist;
    /// 3. that authority's owner org is the membership's org;
    /// 4. the membership vouches for THIS node's entity (the provider's TOFU
    ///    member binding would refuse otherwise — fail before signing).
    ///
    /// Then each DISCOVER grant's audience is leased into the node's consumer
    /// registry. A grant that cannot currently be installed (expired, no
    /// discovery binding, conflicting, registry full) fails the bind loudly
    /// rather than leaving a client that silently discovers nothing.
    ///
    /// The lease is released when the last clone of the returned client drops.
    #[doc(hidden)]
    pub fn bind_node(
        node: Arc<MeshNode>,
        credentials: OrgCredentials,
    ) -> Result<Self, OrgSdkError> {
        // Node metadata, not an authority decision — but the facade's contract
        // is that org credentials bind to a durable entity, and a generated
        // fallback identity is not one.
        if !node.has_configured_identity() {
            return Err(OrgCredentialError::PersistentIdentityRequired.into());
        }
        let authority = node
            .node_authority()
            .ok_or(OrgCredentialError::NodeAuthorityRequired)?;

        let authority_org = authority.owner_org();
        if authority_org != credentials.acting_org() {
            return Err(OrgCredentialError::NodeAuthorityOrgMismatch {
                authority_org,
                membership_org: credentials.acting_org(),
            }
            .into());
        }
        if credentials.member() != node.entity_id() {
            return Err(OrgCredentialError::MemberBindingMismatch {
                expected: node.entity_id().clone(),
                credential: credentials.member().clone(),
            }
            .into());
        }

        let acting_org = credentials.acting_org();
        let (membership, dispatcher, grants, secrets) = credentials.into_parts();

        // Pair each secret with its grant. Construction proved every secret
        // matches exactly one held grant, so this loses nothing; a grant with no
        // secret (INVOKE-only, or DISCOVER whose key was not supplied) simply
        // installs no audience.
        let mut pairs = Vec::with_capacity(secrets.len());
        for secret in secrets {
            let Some(grant) = grants.iter().find(|g| secret.matches_grant(g)) else {
                // Unreachable: `OrgCredentials::new` rejects an unmatched secret.
                continue;
            };
            pairs.push((grant.clone(), secret));
        }

        // The lease registry lives on the NODE, so every wrapper over this node
        // shares one refcount per grant id.
        let grant_ids = node
            .acquire_consumer_audience_leases(pairs)
            .map_err(|(id, source)| OrgCredentialError::AudienceInstallRefused {
                grant_id: hex32(&id),
                source,
            })?;

        // SENSING ACQUISITION, once per bind. Mapped, never propagated: a
        // refused mint is an inert binding, not a failed bind, and the error
        // type of this function is unchanged. Recorded once, here - never per
        // call - because the mint does not happen again on this client.
        #[cfg(feature = "cortex")]
        let _sensing = match OrgSensingFamily::mint(&node) {
            Ok(family) => OrgSensingBinding::Active(OrgSensingAcquisition::new(family)),
            Err(refusal) => {
                // `eprintln!` because this crate takes no logging dependency
                // (the `compute` verbs do the same for their one operator
                // warning). Once per BIND, never per call: the mint does not
                // happen again on this client, so there is nothing to rate
                // limit. It names the typed refusal so an operator can tell
                // "no authority" from "family space exhausted".
                eprintln!(
                    "WARN: org sensing family unavailable ({refusal:?}); \
                     this binding plans unsensed"
                );
                OrgSensingBinding::Inert
            }
        };

        Ok(OrgClient {
            caller: node.entity_keypair_arc(),
            membership,
            dispatcher,
            grants,
            acting_org,
            skew_secs: authority.config.verification_skew_secs,
            _lease: Arc::new(AudienceLeaseGuard::new(node.clone(), grant_ids)),
            #[cfg(feature = "cortex")]
            _sensing,
            node,
        })
    }
}

impl Mesh {
    /// Bind an organization credential set to this mesh (OSDK §1).
    ///
    /// Thin delegation to [`OrgClient::bind_node`], which documents the
    /// complete relation this refuses on. One pipeline, two doors.
    pub fn org(&self, credentials: OrgCredentials) -> Result<OrgClient, OrgSdkError> {
        OrgClient::bind_node(self.node().clone(), credentials)
    }
}
