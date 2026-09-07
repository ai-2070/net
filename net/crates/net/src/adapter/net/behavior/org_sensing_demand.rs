//! Retained organization exact-provider sensing demand, owned by a clone-shared
//! family (OLB / design D4–D5).
//!
//! # What this owns
//!
//! One `OrgSensingFamily` is the demand ownership root for one binding. It
//! holds, per capability, the set of exact-provider sensing leases this node
//! retains on that capability's AUTHORIZED providers, and it is the thing whose
//! last owner releasing them retires that demand.
//!
//! ```text
//! OrgSensingFamily (Clone = one Arc bump, NO Drop)
//!   └─ Arc<OrgSensingFamilyInner>            ── Drop HERE: last owner retires
//!        ├─ RoutingFamily                     (the node-minted family identity)
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
//! # Authority
//!
//! Nothing about the demand is caller-supplied except which capability to sense
//! and how big the population may be:
//!
//! * the AUDIENCE is derived from this node's own installed organization
//!   authority (`MeshNode::capture_sensing_authority_snapshot` →
//!   `canonical_org_sensing_commitment`). There is no audience argument, so a
//!   caller cannot fabricate one, and there is no legacy fallback: with no live
//!   authority the retention refuses outright;
//! * the POPULATION is derived from verified owner-private discovery
//!   (`MeshNode::org_sensing_authorized_population`);
//! * every emitted registration is authored by the existing organization lease
//!   leg, which performs its own fresh capture and its own publication fence.
//!
//! Replacement, revocation and poison are fenced by that existing machinery:
//! the retention's snapshot stamp is recorded, and a reconciliation whose stamp
//! is no longer current re-derives rather than reusing it.
//!
//! # Lock discipline
//!
//! `demand_mu` serializes the map and NOTHING else. Under it: integer, pointer
//! and map work only — no destructor, no lease-apply acquisition, no
//! certificate verification, no emission, no `.await`. Every site therefore
//! EXTRACTS superseded/removed `Arc`s into a local declared BEFORE the guard,
//! releases the guard, and only then closes them. A lease release takes
//! `sensing_lease_apply_mu` and can emit, and `parking_lot::Mutex` is not
//! reentrant, so closing a demand inside the guarded section is exactly the
//! hazard this shape removes.
//!
//! # What stays dark
//!
//! Provider-free `OrgCapabilityRegistration` is not lit here: retention is
//! exact-provider only (`ProviderSelector::Node(provider)`). This module is not
//! wired into `OrgClient::call` and adds no public consumer API; it is the
//! internal demand/refresh substrate a later slice binds to.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use super::org_grant::CapabilityAuthorityId;
use super::org_routing_registry::RoutingFamily;
use super::sensing;
use crate::adapter::net::mesh::{MeshNode, MAX_ORG_SENSING_POPULATION};

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
    ticket: sensing::SensingLeaseTicket,
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

    /// Whether the authority view this demand was derived against is STILL the
    /// live one. A replacement, revocation, rotation or poison makes it false,
    /// and a reconciliation then re-derives instead of reusing it.
    pub fn authority_is_current(&self) -> bool {
        self.node
            .sensing_authority_stamp_is_current(&self.authority_epoch)
    }

    /// RETIRE every retained provider: disarm its refresh, then release its
    /// ticket. The lease key's LAST holder release is what emits `Deregister`.
    ///
    /// Runs with `demand_mu` RELEASED — releasing takes
    /// `sensing_lease_apply_mu` and may emit, neither of which may happen under
    /// the family lock.
    ///
    /// A refused release (a surviving-holder re-authoring that current
    /// authority will not admit) is counted and logged rather than retried
    /// here: the transactional refusal means nothing moved, and the remaining
    /// holder's own release still deregisters. Retrying under a teardown would
    /// be an unbounded wait on authority this node may never regain.
    fn close(&self) {
        for retained in &self.retained {
            self.node.disarm_sensing_refresh(&retained.key);
            if let Err(refused) = self
                .node
                .try_release_sensing_interest_lease(retained.ticket)
            {
                tracing::warn!(
                    provider = format!("{:#x}", retained.provider),
                    reason = %refused.reason,
                    "org sensing demand: retirement release refused; the lease keeps its \
                     pre-release state and its remaining holders still own it"
                );
                continue;
            }
            self.node.org_sensing_demand_counters().note_released();
        }
    }
}

/// The clone-family body. `Drop` lives here, and only here.
struct OrgSensingFamilyInner {
    /// The node-minted family identity. Held for the lifetime of the demand so
    /// the routing registry's family bound accounts for this binding.
    _family: RoutingFamily,
    /// THE serializing lock for this family's demand map.
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
    /// Mint a family for `node`. Fallible because the routing registry's family
    /// identity space is bounded and terminal.
    pub fn mint(node: &Arc<MeshNode>) -> Result<Self, OrgSensingDemandRefused> {
        let family = node
            .org_routing_family()
            .map_err(|_| OrgSensingDemandRefused::FamilyUnavailable)?;
        Ok(Self {
            inner: Arc::new(OrgSensingFamilyInner {
                _family: family,
                demand_mu: parking_lot::Mutex::new(BTreeMap::new()),
            }),
        })
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
    /// currently authorized population.
    ///
    /// Derives the audience from this node's own live authority and the
    /// population from verified owner-private discovery, then converges:
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
        node: &Arc<MeshNode>,
        tag: &str,
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        let population =
            node.org_sensing_authorized_population(&CapabilityAuthorityId::for_tag(tag));
        self.reconcile(node, tag, &population)
    }

    /// [`Self::retain`] against an EXPLICIT population.
    ///
    /// The population still has to be an authorized one — this is the seam the
    /// churn ordering is driven through, not a way to widen authority: the
    /// audience is still derived from live authority, and every registration is
    /// still authored and fenced by the organization lease leg.
    pub fn reconcile(
        &self,
        node: &Arc<MeshNode>,
        tag: &str,
        population: &[u64],
    ) -> Result<Arc<OrgSensingCapabilityDemand>, OrgSensingDemandRefused> {
        let capability = CapabilityAuthorityId::for_tag(tag);
        // PHASE 0 — off the family lock: derive the audience from THIS node's
        // own installed authority. No argument, no fallback.
        let snapshot = node
            .capture_sensing_authority_snapshot()
            .map_err(|_| OrgSensingDemandRefused::NoAuthority)?;
        let audience =
            sensing::canonical_org_sensing_commitment(&snapshot.authority_view().owner_org);
        let authority_epoch = *snapshot.stamp();
        // Bound the sensed subset. Truncation is the caller's own population
        // cap, already counted by the derivation helper; an explicit population
        // is clamped here so both entry points obey one bound.
        let mut wanted: Vec<u64> = population.to_vec();
        wanted.sort_unstable();
        wanted.dedup();
        wanted.truncate(MAX_ORG_SENSING_POPULATION);

        // Bound check and previous-state read, under the family lock. Map work
        // only.
        let previous = {
            let map = self.inner.demand_mu.lock();
            match map.get(&capability) {
                Some(existing) => Some(Arc::clone(existing)),
                None if map.len() >= MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY => {
                    node.org_sensing_demand_counters().note_at_capacity();
                    return Err(OrgSensingDemandRefused::FamilyAtCapacity);
                }
                None => None,
            }
        };

        // PHASE 1 — off the family lock. Carry FORWARD every still-authorized
        // holder, then acquire the additions.
        let mut carried: Vec<RetainedProvider> = Vec::new();
        let mut departed: Vec<RetainedProvider> = Vec::new();
        if let Some(previous) = &previous {
            for retained in &previous.retained {
                if wanted.binary_search(&retained.provider).is_ok() {
                    carried.push(RetainedProvider {
                        provider: retained.provider,
                        key: retained.key,
                        ticket: retained.ticket,
                    });
                } else {
                    departed.push(RetainedProvider {
                        provider: retained.provider,
                        key: retained.key,
                        ticket: retained.ticket,
                    });
                }
            }
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

        // PHASE 2 — publish the new demand under the family lock, EXTRACTING
        // the superseded entry. The superseded `Arc` must not be dropped here:
        // its own `close` would take the lease-apply guard under this one.
        let superseded = {
            let mut map = self.inner.demand_mu.lock();
            map.insert(capability, Arc::clone(&demand))
        };
        // PHASE 3 — guard released. The superseded demand's carried-forward
        // tickets were MOVED into the new demand, so only the departed ones are
        // released; the superseded container itself just drops.
        drop(superseded);
        for gone in &departed {
            node.disarm_sensing_refresh(&gone.key);
            if node
                .try_release_sensing_interest_lease(gone.ticket)
                .is_err()
            {
                tracing::warn!(
                    provider = format!("{:#x}", gone.provider),
                    "org sensing demand: reconciliation release refused; the lease keeps \
                     its pre-release state"
                );
                continue;
            }
            node.org_sensing_demand_counters().note_released();
        }
        Ok(demand)
    }

    /// RETIRE the demand for capability `tag` entirely.
    pub fn retire(&self, tag: &str) {
        let capability = CapabilityAuthorityId::for_tag(tag);
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
        let ticket = match node.acquire_sensing_interest_lease(&spec, provider, interval) {
            Ok(ticket) => ticket,
            Err(error) => {
                node.org_sensing_demand_counters().note_no_authority();
                tracing::debug!(
                    provider = format!("{:#x}", provider),
                    %error,
                    "org sensing demand: provider not retained"
                );
                return None;
            }
        };
        let key = sensing::SensingLeaseKey::ExactProvider {
            audience,
            interest_digest: spec.interest_digest(),
            provider,
        };
        node.org_sensing_demand_counters().note_retained();
        // ARM the refresh for THIS installation. `ttl/2` on the node's own
        // soft-state horizon, as an absolute deadline on the node's single
        // worker — no timer per lease and no whole-second rounding.
        if let Some(installation) = node.sensing_refresh_installation(&key) {
            MeshNode::arm_sensing_refresh(node, key, installation, node.sensing_refresh_period());
        }
        Some(RetainedProvider {
            provider,
            key,
            ticket,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use crate::adapter::net::behavior::org_authority::NodeAuthority;
    use crate::adapter::net::mesh::SensingRefreshOutcome;
    use crate::adapter::net::{EntityKeypair, MeshNodeConfig};
    use crate::adapter::Adapter;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

    const TAG: &str = "nrpc:gpu.infer";

    fn org() -> OrgKeypair {
        OrgKeypair::from_bytes([0x42u8; 32])
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
            .reconcile(&node, TAG, &providers)
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
            "and every refresh must be disarmed: {state:?}"
        );
        assert!(
            node.sensing_interest_leases_for_test().is_empty(),
            "no lease may survive its family"
        );
    }

    /// Reconciliation over a changed population keeps the SURVIVING holder's
    /// installation identity, adds the newcomer, and releases only the
    /// departed one.
    ///
    /// Identity preservation is the load-bearing half: re-acquiring a survivor
    /// would give it a fresh installation id, which disarms its refresh and
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

        let first = family.reconcile(&node, TAG, &[a, b]).expect("retain a,b");
        assert_eq!(first.retained_providers(), vec![a, b]);
        let b_installation = node
            .sensing_refresh_installation(&lease_key_for(&node, b))
            .expect("b is installed");

        let second = family.reconcile(&node, TAG, &[b, c]).expect("retain b,c");
        assert_eq!(second.retained_providers(), vec![b, c]);
        assert_eq!(
            second.population().as_ref(),
            &[b, c],
            "the population is the reconciliation's own immutable input"
        );
        assert_eq!(
            node.sensing_refresh_installation(&lease_key_for(&node, b)),
            Some(b_installation),
            "the surviving holder was re-acquired instead of carried forward — its \
             installation identity moved, so its refresh was silently disarmed"
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
            .reconcile(&node, TAG, &providers)
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
            state.refused_no_authority, 2,
            "both refusals must be counted: {state:?}"
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
                .reconcile(&node, TAG, &[node.node_id().wrapping_add(1)])
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
                .reconcile(&node, &format!("nrpc:cap-{index}"), &[provider])
                .expect("within the bound");
        }
        assert_eq!(
            family.capabilities().len(),
            MAX_ORG_SENSING_CAPABILITIES_PER_FAMILY
        );

        assert_eq!(
            family
                .reconcile(&node, "nrpc:one-too-many", &[provider])
                .err(),
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

    // ---- REFRESH ---------------------------------------------------------

    /// A refresh RENEWS the installation and acquires no holder.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refresh_renews_without_acquiring_a_holder() {
        let node = demand_node("refresh-renew", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let demand = family.reconcile(&node, TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        let holders_before = node
            .sensing_interest_leases_for_test()
            .entry_for_test(&key)
            .expect("entry");

        for _ in 0..8 {
            assert_eq!(
                node.refresh_sensing_interest_lease(&key, installation),
                SensingRefreshOutcome::Renewed
            );
        }
        assert_eq!(
            node.sensing_interest_leases_for_test().entry_for_test(&key),
            Some(holders_before),
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

    /// A refresh armed for a RETIRED installation renews nothing, and a refresh
    /// armed for a SUPERSEDED one cannot touch its successor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refresh_never_resurrects_retired_or_superseded_demand() {
        let node = demand_node("refresh-retired", Duration::from_secs(30)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        let key = lease_key_for(&node, provider);

        let first = family.reconcile(&node, TAG, &[provider]).expect("retain");
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
        let second = family
            .reconcile(&node, TAG, &[provider])
            .expect("re-retain");
        let successor = node.sensing_refresh_installation(&key).expect("installed");
        assert_ne!(
            successor, retired,
            "a re-established installation must not reuse the retired identity"
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

    /// Authority replacement fences the refresh: it refuses on its own plane
    /// and never downgrades to a legacy frame.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authority_replacement_refuses_a_refresh_without_downgrading() {
        // A short horizon so the upstream registration damper's minimum gap
        // (`min(const, ttl/2)`) is short enough to step past deliberately: a
        // refresh INSIDE that gap is correctly suppressed, so an emission
        // control has to sit outside it or it proves nothing.
        let node = demand_node("refresh-authority", Duration::from_millis(200)).await;
        let family = OrgSensingFamily::mint(&node).expect("mint");
        let provider = node.node_id().wrapping_add(1);
        // Count every organization authoring/emission phase, from before the
        // retention, so the observer is proven live by real traffic.
        let emissions = Arc::new(std::sync::atomic::AtomicU64::new(0));
        {
            let seen = emissions.clone();
            node.set_sensing_phase_two_seam_for_test(Arc::new(move || {
                seen.fetch_add(1, AtomicOrdering::SeqCst);
            }));
        }
        let demand = family.reconcile(&node, TAG, &[provider]).expect("retain");
        let key = lease_key_for(&node, provider);
        let installation = node.sensing_refresh_installation(&key).expect("installed");
        assert!(demand.authority_is_current(), "precondition");
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            1,
            "the retention itself must author exactly one organization frame"
        );

        // Step past the damper's minimum gap so a LIVE refresh demonstrably
        // authors — the control that makes the absence below meaningful.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::Renewed
        );
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            2,
            "a live refresh past the damper gap must author exactly one more frame"
        );

        node.clear_node_authority_for_test();
        assert!(
            !demand.authority_is_current(),
            "the retained epoch must no longer be current"
        );
        assert_eq!(
            node.refresh_sensing_interest_lease(&key, installation),
            SensingRefreshOutcome::AuthorityUnavailable
        );
        assert_eq!(
            emissions.load(AtomicOrdering::SeqCst),
            2,
            "a refresh with no live authority emitted a frame — an organization \
             installation must never be renewed under a legacy downgrade"
        );
        assert_eq!(
            node.org_sensing_demand_state_for_test()
                .refresh_authority_refused,
            1
        );
        node.clear_sensing_phase_two_seam_for_test();
        drop(demand);
        drop(family);
    }

    /// END TO END at the internal boundary: the node's ONE refresh worker fires
    /// on its own schedule and renews the retained installation, with no timer
    /// per lease and no holder growth. Then shutdown closes the schedule.
    ///
    /// This is a fixture proof of the demand/refresh substrate. It is NOT a
    /// production sensed `org.call` proof — nothing here is wired into call
    /// planning.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_single_worker_refreshes_retained_demand_and_shutdown_closes_it() {
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
        let demand = family.reconcile(&node, TAG, &providers).expect("retain");
        let keys: Vec<_> = providers
            .iter()
            .map(|provider| lease_key_for(&node, *provider))
            .collect();
        let installations: Vec<_> = keys
            .iter()
            .map(|key| node.sensing_refresh_installation(key).expect("installed"))
            .collect();
        let holders: Vec<_> = keys
            .iter()
            .map(|key| node.sensing_interest_leases_for_test().entry_for_test(key))
            .collect();

        let armed = node.org_sensing_demand_state_for_test();
        assert_eq!(armed.armed, 2, "both installations armed: {armed:?}");
        assert!(armed.worker_started, "on ONE worker: {armed:?}");

        // The worker fires on its own.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = node.org_sensing_demand_state_for_test();
            if state.refresh_renewed >= 4 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the node's refresh worker never renewed the retained demand: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(
                node.sensing_refresh_installation(key),
                Some(installations[index]),
                "a worker refresh moved an installation identity"
            );
            assert_eq!(
                node.sensing_interest_leases_for_test().entry_for_test(key),
                holders[index],
                "a worker refresh acquired a holder"
            );
        }
        let state = node.org_sensing_demand_state_for_test();
        assert_eq!(state.armed, 2, "still exactly two armed: {state:?}");
        assert_eq!(state.refresh_absent, 0, "{state:?}");
        assert_eq!(state.refresh_superseded, 0, "{state:?}");

        node.shutdown().await.expect("shutdown");
        let closed = node.org_sensing_demand_state_for_test();
        assert!(closed.terminal, "the schedule must be terminal: {closed:?}");
        assert_eq!(closed.armed, 0, "and armed nothing: {closed:?}");
        assert!(
            !MeshNode::arm_sensing_refresh(
                &node,
                keys[0],
                installations[0],
                Duration::from_millis(20)
            ),
            "nothing may be armed after the schedule is terminal"
        );
        drop(demand);
        drop(family);
    }
}
