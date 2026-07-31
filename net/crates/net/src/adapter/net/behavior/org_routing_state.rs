//! OLB-2B.3b: the clone-family routing state.
//!
//! One `OrgRoutingState` per clone family. It owns three things and nothing
//! else: a lock-free capability index, the `CapabilityRouteHandle`s that index
//! points at, and the refusal bookkeeping that decides whether a miss may try
//! to acquire demand again.
//!
//! ```text
//! OrgRoutingState
//!   index    ArcSwap<CapabilityIndex>      immutable, bounded; the READ path
//!   mutate   parking_lot::Mutex<Refusal>   miss / insert / drop ONLY
//! ```
//!
//! Design §8 sketches `mutate` as `Mutex<()>`. It guards the refusal
//! bookkeeping here instead of nothing, because that state is read and written
//! on exactly the miss path the lock already delimits — a separate `Mutex<()>`
//! beside separately-synchronized refusal state would be two things to keep in
//! step where one will do, and would let a refusal be recorded against a
//! capacity generation sampled outside the section that acted on it. What §8
//! actually pins is preserved exactly: the READ path takes no lock, and the
//! mutation path takes exactly one.
//!
//! **The read path takes no lock.** `index.load()` is one atomic; the handle it
//! yields is an `Arc` clone. `mutate` is never touched by a hit. It is not a
//! `DashMap` on purpose (design §8): internally-sharded locking would leave the
//! zero-lock contract unproven while appearing to satisfy it, because the
//! sharded lock is usually uncontended and a witness would pass by luck.
//!
//! ## What this slice does NOT do
//!
//! A `CapabilityRouteHandle` here owns a demand set and nothing more. There is
//! no `OrgRouteSet`, no route-set cell, no scoped pool, no candidate, no
//! provider list, no selection, and no projection — those are 2B.3c and 2B.3d,
//! and this module deliberately cannot express them. `route_handle` returns
//! ownership of retained demand, never a route.
//!
//! ## The demand set (design §4)
//!
//! ```text
//! Owner
//! + each Grant audience THIS FAMILY ACTUALLY LEASED FOR DISCOVER
//! ```
//!
//! Both qualifiers are load-bearing and each has its own witness. A DISCOVER
//! grant whose audience secret was never installed yields no consumer audience,
//! so demanding it would retain a slot that can never be anything but
//! `Unserved` — permanently consuming node and family budget for a contributor
//! that can never contribute (W-N1). And an INVOKE-only grant is not a
//! discovery authority at all; it stays in the family's credential set for the
//! projection to match providers against, but it is not a source demand (W-N2).
//!
//! ```text
//! DISCOVER ≠ INVOKE ≠ SENSE
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;

use super::org::OrgId;
use super::org_grant::{CapabilityAuthorityId, OrgCapabilityGrant};
use super::org_grant_registry::ConsumerGrantSnapshot;
use super::org_routing_registry::{
    DemandHandle, DemandRefused, PrivateAudienceScope, RoutingFamily, SlotKey,
    MAX_HANDLES_PER_FAMILY,
};
use super::org_scoped_ingest::CapabilityAudienceScope;

/// Max capability entries ONE clone family may retain (design §4).
///
/// STRUCTURAL rather than separately enforced: every entry retains at least the
/// Owner demand, and the family's demand budget is
/// [`MAX_HANDLES_PER_FAMILY`], so the index can never hold more entries than
/// that however cheap the entries are. Stating it as a `<=` bound derived from
/// the demand bound — rather than as a second independent counter — is what
/// keeps the two from disagreeing.
///
/// It is a CEILING that is normally not reached. A family with two leased
/// DISCOVER grants per capability spends three demands per entry and warms 21
/// capabilities, not 64.
#[allow(dead_code)] // documents the structural ceiling; nothing enforces it twice.
pub(crate) const MAX_CAPABILITY_ENTRIES_PER_FAMILY: usize = MAX_HANDLES_PER_FAMILY;

/// The family's discovery credentials, as the demand derivation sees them.
///
/// Deliberately plain data. The derivation is a pure function of
/// (credentials, capability, installed consumer audiences), which is what lets
/// a witness drive it without a node and what keeps this slice from acquiring
/// a dependency on the SDK's credential type.
#[derive(Clone, Debug)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) struct FamilyDiscoveryCredentials {
    /// The acting org — the family's own organization.
    pub acting_org: OrgId,
    /// The org's owner audience routing handle. With `acting_org` this is the
    /// family's Owner discovery scope, which is a demand for EVERY capability
    /// the family warms.
    pub owner_audience_handle: [u8; 32],
    /// The family's COMPLETE grant set, in exact credential order.
    ///
    /// Complete on purpose: INVOKE-only grants belong here even though they are
    /// never source demands, because the projection matches INVOKE against this
    /// set (§4). A derivation that could only see DISCOVER grants would make
    /// W-N2 unwitnessable — there would be nothing left to wrongly demand.
    pub grants: Vec<Arc<OrgCapabilityGrant>>,
}

impl FamilyDiscoveryCredentials {
    /// The family's Owner discovery scope.
    fn owner_scope(&self) -> Option<PrivateAudienceScope> {
        PrivateAudienceScope::new(CapabilityAudienceScope::Owner {
            org_id: self.acting_org,
            audience_handle: self.owner_audience_handle,
        })
    }
}

/// Why one grant is or is not a source demand for a capability.
///
/// A named classification rather than a filter chain, because "which of the
/// family's credentials are source demands" is the single decision W-N1 and
/// W-N2 pin, and a decision that lives in a `.filter(..).filter(..)` cannot be
/// mutated one reason at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantDemand {
    /// A DISCOVER grant whose exact audience the family has leased.
    Leased([u8; 32]),
    /// Right for another verb. INVOKE-only grants stay available to the
    /// projection and consume no source budget.
    NotDiscovery,
    /// DISCOVER, but no installed audience secret — so no consumer audience
    /// exists and the source can never serve the scope.
    NotLeased,
}

/// Classify one grant of the family's credential set for `capability`.
///
/// `None` when the grant is for a different capability or a different grantee;
/// otherwise the exact reason it is or is not a demand.
fn classify(
    grant: &OrgCapabilityGrant,
    capability: &CapabilityAuthorityId,
    acting_org: &OrgId,
    leased: &ConsumerGrantSnapshot,
) -> Option<GrantDemand> {
    if &grant.capability != capability || &grant.grantee_org != acting_org {
        return None;
    }
    // W-N2. DISCOVER ≠ INVOKE: only a DISCOVER grant carries the discovery
    // binding that names an audience, and only an audience can be a source
    // scope. Demanding an INVOKE-only grant would key a slot on an audience
    // that does not exist, which the source can only ever answer `Unserved`.
    if !grant.permits_discover() {
        return Some(GrantDemand::NotDiscovery);
    }
    // Both halves of the structural rule, checked rather than assumed. `verify`
    // enforces `rights ⊇ DISCOVER ⇔ binding present`, so for a verified grant
    // these two cannot disagree — which is exactly why neither is load-bearing
    // on its own and why §17.6a records that this gate is not independently
    // observable further downstream.
    let Some(binding) = grant.discovery.as_ref() else {
        return Some(GrantDemand::NotDiscovery);
    };
    // W-N1. The right to discover is not the audience. Without the out-of-band
    // secret installed there is no consumer audience, the live query admits no
    // row, and the slot is a permanently-`Unserved` required contributor that
    // consumes budget forever (§4).
    //
    // Compared on the WHOLE scope — `(grant_id, audience_handle)` — never on
    // `grant_id` alone. A rotated audience leaves the id installed under a
    // DIFFERENT handle, and an id-keyed check would call the rotated-away scope
    // leased (the aliasing the source seam already closed at `cbbd448b3`).
    let installed = leased
        .get(&grant.grant_id)
        .is_some_and(|record| record.audience_handle() == &binding.audience_handle);
    if installed {
        Some(GrantDemand::Leased(binding.audience_handle))
    } else {
        Some(GrantDemand::NotLeased)
    }
}

/// The exact demand set for `capability`: Owner plus every leased DISCOVER
/// audience, deduplicated and in deterministic key order.
///
/// `None` when the family has no representable Owner scope, which is the one
/// input that makes the whole set meaningless rather than merely smaller.
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) fn demand_set_for(
    credentials: &FamilyDiscoveryCredentials,
    capability: &CapabilityAuthorityId,
    leased: &ConsumerGrantSnapshot,
) -> Option<Vec<SlotKey>> {
    let mut keys = vec![SlotKey {
        scope: credentials.owner_scope()?,
        capability: *capability,
    }];
    for grant in &credentials.grants {
        let Some(GrantDemand::Leased(audience_handle)) =
            classify(grant, capability, &credentials.acting_org, leased)
        else {
            continue;
        };
        let scope = CapabilityAudienceScope::Grant {
            grant_id: grant.grant_id,
            audience_handle,
        };
        // `Public` is the only rejection, and a Grant scope is never public.
        if let Some(scope) = PrivateAudienceScope::new(scope) {
            keys.push(SlotKey {
                scope,
                capability: *capability,
            });
        }
    }
    // The registry deduplicates too; doing it here as well keeps the count this
    // function reports equal to the budget the acquisition actually spends.
    keys.sort();
    keys.dedup();
    Some(keys)
}

/// One warmed capability entry: the family's COMPLETE retained demand for it.
///
/// Ownership is the whole content of this type in 2B.3b. Dropping it releases
/// every handle it holds, and the last handle for a slot retires that slot —
/// so an entry is exactly as alive as the demand behind it, with no separate
/// lifecycle to keep in step.
///
/// The demand set is never partial: it is acquired all-or-none (§4.1), so a
/// handle either names every contributor its capability needs or does not
/// exist.
#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
pub(crate) struct CapabilityRouteHandle {
    capability: CapabilityAuthorityId,
    /// The complete demand set, in deterministic key order. Owner first —
    /// `SlotKey` sorts by scope, and `CapabilityAudienceScope::Owner` precedes
    /// `Grant` — which is the order §3.1 projects in.
    demands: Vec<DemandHandle>,
}

#[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
impl CapabilityRouteHandle {
    pub(crate) fn capability(&self) -> &CapabilityAuthorityId {
        &self.capability
    }

    /// The retained contributors, in projection order.
    pub(crate) fn demands(&self) -> &[DemandHandle] {
        &self.demands
    }
}

/// The family's immutable capability index.
///
/// Rebuilt by copy-on-write under `mutate` and published with one `ArcSwap`
/// store. Readers never see a partially-built index and never take a lock.
#[derive(Default)]
pub(crate) struct CapabilityIndex {
    entries: BTreeMap<CapabilityAuthorityId, Arc<CapabilityRouteHandle>>,
}

impl CapabilityIndex {
    fn get(&self, capability: &CapabilityAuthorityId) -> Option<&Arc<CapabilityRouteHandle>> {
        self.entries.get(capability)
    }

    #[allow(dead_code)] // consumer: the family projection (OLB-2B.3d).
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    fn with(&self, handle: Arc<CapabilityRouteHandle>) -> Self {
        let mut entries = self.entries.clone();
        entries.insert(handle.capability, handle);
        Self { entries }
    }
}

/// What a lookup found, and — when it found nothing — whether acquiring demand
/// is still worth attempting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) enum RouteLookup {
    /// A warmed entry. The caller holds retained demand for the capability.
    Warm,
    /// No entry, and none was attempted or acquired. The call takes the
    /// current-authority cold path; this is bounded degradation, never failure.
    Cold(ColdReason),
}

/// Why a lookup is cold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // consumer: `MeshNode::call` (OLB-2B.3d).
pub(crate) enum ColdReason {
    /// The family has no representable Owner discovery scope.
    NoOwnerScope,
    /// Demand was refused, with the class deciding whether a later miss retries
    /// (§9).
    Refused(DemandRefused),
}

/// Whether a refusal class may ever be attempted again, and on what.
///
/// Three classes, three policies, and they are genuinely different — collapsing
/// them would break in a different direction each time (§9):
///
/// ```text
/// FamilyAtCapacity   sticky for the family's LIFETIME
/// NodeAtCapacity     retryable, gated on the node capacity generation
/// IdSpaceExhausted   terminal — never retry
/// ```
#[derive(Debug, Default)]
pub(crate) struct RefusalState {
    /// Set once the family's demand budget is spent.
    ///
    /// Sticky is EXACT here, not merely conservative: this state never evicts a
    /// capability entry, so a family's handle count only ever grows. Once the
    /// budget is spent it stays spent for the family's lifetime, and re-deriving
    /// a demand set to be refused again on every later call would be pure waste.
    family_at_capacity: bool,
    /// Set once the identity space is exhausted.
    ///
    /// TERMINAL, and terminal outranks the retryable class. Exhaustion is
    /// irreversible by construction — the allocator is checked and never wraps —
    /// so a node capacity generation that moves afterwards says nothing about
    /// it. A family that retried on that signal would attempt acquisition
    /// forever on a node that can never satisfy it.
    id_space_exhausted: bool,
    /// The node capacity generation observed at the last `NodeAtCapacity`
    /// refusal, or `None` if there has not been one.
    ///
    /// A later miss retries only if the generation has MOVED. Equality means no
    /// slot has retired since the refusal, so the node is provably still full
    /// and the attempt would take the registry lock only to be refused again.
    node_at_capacity_at: Option<u64>,
}

impl RefusalState {
    /// Whether acquisition may be attempted, given the CURRENT node capacity
    /// generation.
    fn may_attempt(&self, node_capacity_generation: u64) -> bool {
        if self.family_at_capacity || self.id_space_exhausted {
            return false;
        }
        self.node_at_capacity_at
            .is_none_or(|at| at != node_capacity_generation)
    }

    fn record(&mut self, refusal: DemandRefused, node_capacity_generation: u64) {
        match refusal {
            DemandRefused::FamilyAtCapacity => self.family_at_capacity = true,
            DemandRefused::IdSpaceExhausted => self.id_space_exhausted = true,
            DemandRefused::NodeAtCapacity => {
                self.node_at_capacity_at = Some(node_capacity_generation);
            }
        }
    }
}

/// The clone-family routing state (design §8).
#[allow(dead_code)] // consumer: `OrgClient` (OLB-2B.3d).
pub(crate) struct OrgRoutingState {
    family: RoutingFamily,
    credentials: FamilyDiscoveryCredentials,
    /// The READ path. One atomic load, no lock, ever.
    index: ArcSwap<CapabilityIndex>,
    /// Miss / insert / drop ONLY. Never taken by a hit.
    mutate: parking_lot::Mutex<RefusalState>,
    /// Every acquisition of `mutate`, so a witness can assert that a warmed
    /// lookup takes it ZERO times rather than inferring it from timing.
    ///
    /// An end-to-end counter and a real contention witness prove different
    /// things and the design asks for both: the counter cannot distinguish "no
    /// lock" from "an uncontended lock", and the contention witness cannot see
    /// a lock taken on a path it did not exercise.
    mutate_acquisitions: AtomicU64,
}

#[allow(dead_code)] // consumer: `OrgClient` (OLB-2B.3d).
impl OrgRoutingState {
    pub(crate) fn new(family: RoutingFamily, credentials: FamilyDiscoveryCredentials) -> Self {
        Self {
            family,
            credentials,
            index: ArcSwap::from_pointee(CapabilityIndex::default()),
            mutate: parking_lot::Mutex::new(RefusalState::default()),
            mutate_acquisitions: AtomicU64::new(0),
        }
    }

    /// The WARMED read: one atomic load, one map lookup, one `Arc` clone.
    ///
    /// Takes no lock — not this state's, not the registry's, not the SDK's.
    /// This is the path a warmed call runs, and it is the reason the index is
    /// an immutable snapshot behind an `ArcSwap` rather than a map with a lock
    /// in front of it.
    pub(crate) fn warm(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<Arc<CapabilityRouteHandle>> {
        self.index.load().get(capability).cloned()
    }

    /// Look the capability up and, on a miss, acquire its COMPLETE demand set.
    ///
    /// The hit is checked twice by design. Once lock-free, which is the whole
    /// hot path; then again under `mutate`, because two threads can miss
    /// concurrently and only one of them may spend the family's budget — the
    /// loser must adopt the winner's entry rather than acquire a second,
    /// duplicate demand set for the same capability.
    pub(crate) fn route_handle(
        &self,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> RouteLookup {
        if self.warm(capability).is_some() {
            return RouteLookup::Warm;
        }
        self.acquire(capability, leased)
    }

    /// The MISS path. Never reached by a hit.
    fn acquire(
        &self,
        capability: &CapabilityAuthorityId,
        leased: &ConsumerGrantSnapshot,
    ) -> RouteLookup {
        let mut refusals = self.mutate.lock();
        self.mutate_acquisitions.fetch_add(1, Ordering::AcqRel);

        // Re-check under the mutation lock. A concurrent miss may have inserted
        // the entry between the lock-free load and here.
        if self.index.load().get(capability).is_some() {
            return RouteLookup::Warm;
        }

        // The node capacity generation is read ONCE, before the attempt, and
        // the same value is what a refusal records. Reading it after a refusal
        // would record a generation that may already have moved because of the
        // very retirement that would have made the attempt succeed, and the
        // family would then never retry.
        let generation = self.family.node_capacity_generation();
        if !refusals.may_attempt(generation) {
            return RouteLookup::Cold(self.settled_reason(&refusals));
        }

        let Some(keys) = demand_set_for(&self.credentials, capability, leased) else {
            return RouteLookup::Cold(ColdReason::NoOwnerScope);
        };
        match self.family.demand_set(keys) {
            Ok(demands) => {
                let handle = Arc::new(CapabilityRouteHandle {
                    capability: *capability,
                    demands,
                });
                // Copy-on-write publication: readers hold the previous snapshot
                // until this store, and the new one the moment after it. There
                // is no window in which the index is half-built, because the
                // index a reader can reach was never mutated in place.
                self.index.store(Arc::new(self.index.load().with(handle)));
                RouteLookup::Warm
            }
            Err(refusal) => {
                refusals.record(refusal, generation);
                RouteLookup::Cold(ColdReason::Refused(refusal))
            }
        }
    }

    /// The refusal a settled family reports without attempting acquisition.
    ///
    /// Terminal before sticky before retryable, so the reported reason is the
    /// one that actually stops the attempt.
    fn settled_reason(&self, refusals: &RefusalState) -> ColdReason {
        if refusals.id_space_exhausted {
            ColdReason::Refused(DemandRefused::IdSpaceExhausted)
        } else if refusals.family_at_capacity {
            ColdReason::Refused(DemandRefused::FamilyAtCapacity)
        } else {
            ColdReason::Refused(DemandRefused::NodeAtCapacity)
        }
    }

    /// Capability entries currently retained.
    pub(crate) fn entries(&self) -> usize {
        self.index.load().len()
    }

    /// Demand handles this family currently holds, across all entries.
    pub(crate) fn handles(&self) -> usize {
        self.family.handles()
    }

    /// How many times `mutate` has been acquired.
    pub(crate) fn mutate_acquisitions(&self) -> u64 {
        self.mutate_acquisitions.load(Ordering::Acquire)
    }

    /// Test-only: the mutation lock itself, so a contention witness can hold
    /// the REAL lock rather than a stand-in and prove a contender's `try_lock`
    /// fails while a warmed lookup completes.
    #[cfg(test)]
    pub(crate) fn mutate_lock_for_test(&self) -> &parking_lot::Mutex<RefusalState> {
        &self.mutate
    }
}

#[cfg(test)]
#[path = "org_routing_state_tests.rs"]
mod tests;
