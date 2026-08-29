//! OLB-2B.3d-pre — the captured inputs of the coherent current-authority cold
//! plan.
//!
//! The cold plan is the ONE canonical destination for every call the warmed
//! path cannot serve (`docs/internal/plans/OLB_2B3B_WARMED_CALL_BOUNDARY_DESIGN.md`
//! §10). Its first step is the one this module owns: capture the node's
//! private-discovery authority and the rows that authority makes visible as ONE
//! immutable observation, so every later step — credential windows, grant
//! matching, selection, the proof intent — is a pure function of a single view
//! of the world rather than a sequence of independent re-samples.
//!
//! ```text
//! capture   one clock, one revocation view, one consumer-grant view,
//!           one scoped-store critical section  ->  OrgColdDiscovery
//! derive    credential windows, grant matching, order, selection   (pure)
//! compare   the captured authority identity is STILL installed
//! mint      exactly one OrgProofIntent
//! ```
//!
//! What the capture proves, exactly:
//!
//! - **one instant.** [`OrgColdDiscovery::now_secs`] is the only clock the plan
//!   sees. Before this, a plan sampled the wall clock once per credential and
//!   once per discovery plane, so it could be assembled from credentials that
//!   were never simultaneously valid.
//! - **one revocation view.** Every plane is filtered against the SAME floor
//!   snapshot, and the floor generation that qualifies it is captured from that
//!   same snapshot. Before this, the owner plane and each grant plane loaded
//!   their own snapshot, so a floor raise landing between two of them produced a
//!   plan mixing pre-raise and post-raise authority.
//! - **one consumer-grant view.** Every grant scope is pinned from ONE
//!   `ConsumerGrantSnapshot` load.
//! - **one scoped-store critical section.** The owner scope and every grant
//!   scope are queried under a single lock acquisition, so a store mutation
//!   cannot land between two planes of one plan.
//! - **one authority identity.** [`OrgColdAuthorityStamp`] names the exact
//!   authority the capture was taken under, and the plan compares it again
//!   before the proof exists.
//!
//! What it deliberately does NOT claim:
//!
//! - **it is not a route set.** No candidate, no order, no selection, no
//!   sensing, no scoped pool. Those are 2B.3d and later.
//! - **it does not freeze the world.** Discovery rows are copied out and are
//!   filtered per row at query time (expiry, floors, exact grant authority), so
//!   a later announcement simply is not in this plan — exactly as before.
//! - **it says nothing about direct reachability.** Session state is not
//!   authority; the plan annotates it after authorization, as it always has.

use std::sync::Arc;

use crate::adapter::net::behavior::org::OrgId;
use crate::adapter::net::behavior::org_grant_registry::GrantAudienceRecord;
use crate::adapter::net::behavior::org_scoped_ingest::{
    CapabilityAudienceScope, VerifiedScopedCapability,
};
use crate::adapter::net::behavior::org_scoped_store::PrivateCapabilityProvider;

/// Why a coherent capture could not be taken.
///
/// Both arms are LOCAL refusals: nothing was sent, no proof was minted, and no
/// provider was contacted. Neither is a statement about any provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgColdRefusal {
    /// No node authority is installed, so this node holds no private-discovery
    /// authority to plan under. Fail-closed: the facade refuses to plan against
    /// state that cannot exist rather than reporting an empty provider set.
    NoNodeAuthority,
    /// The authority view could not be observed under one unchanging identity
    /// within the bounded attempts, or the epoch space is spent.
    ///
    /// Bounded rather than a retry loop, for the same reason
    /// `MeshNode::sample_routing_authority` is: authority movement is
    /// node-mediated and rare, so exhausting the attempts means something is
    /// genuinely churning and the honest answer is a cold plan.
    IncoherentAuthority,
}

/// The exact installed authority of ONE consumer grant, as captured.
///
/// All three components, not just the id: `grant_id` alone passes a
/// remove-then-reinstall and a different signed grant reusing the id. This
/// mirrors `MeshNode::scope_authority_is_current` — the cached routing plane's
/// equivalent comparison — so the cold plan is never weaker about grant
/// authority than the warmed plane it is the fallback for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgColdGrantAuthority {
    /// The grant this scope was authorized by.
    pub grant_id: [u8; 32],
    /// The node-local installation identity (non-aliasing and monotone), which
    /// distinguishes two installations of the same signed grant.
    pub install_seq: u64,
    /// The signature over the whole canonical grant.
    pub grant_signature: [u8; 64],
    /// The installed audience handle, mirroring the live query's own
    /// defense-in-depth check.
    pub audience_handle: [u8; 32],
}

impl OrgColdGrantAuthority {
    /// Pin the installed record's exact authority.
    pub(crate) fn of(record: &GrantAudienceRecord) -> Self {
        Self {
            grant_id: record.grant().grant_id,
            install_seq: record.install_seq(),
            grant_signature: record.grant().signature,
            audience_handle: *record.audience_handle(),
        }
    }

    /// The row predicate the granted plane filters with — the SAME predicate the
    /// uncached `MeshNode::granted_capability_providers` seam applies.
    ///
    /// Shared rather than restated: two copies of a currentness predicate is how
    /// a cached path silently becomes weaker than the live one it mirrors.
    pub(crate) fn admits(&self, candidate: &VerifiedScopedCapability) -> bool {
        candidate.grant_signature() == Some(&self.grant_signature)
            && matches!(
                candidate.scope(),
                CapabilityAudienceScope::Grant { audience_handle, .. }
                    if audience_handle == &self.audience_handle
            )
    }
}

/// The authority identity a cold plan was derived under (OLB-2B.3d-pre).
///
/// Compared as a WHOLE by the plan's final coherent comparison. Every component
/// moves only through a node-mediated transition (authority installation, store
/// installation, a floor raise, a poison transition, a consumer-grant
/// install/remove/replacement), which is why comparing it cannot make an
/// ordinary call fail: announcement traffic — the frequent movement — is not in
/// here, because the rows it changes are captured values that were already
/// filtered per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgColdAuthorityStamp {
    authority_org: OrgId,
    epoch: u64,
    poisoned: bool,
    floor_generation: u64,
    grants: Vec<([u8; 32], Option<OrgColdGrantAuthority>)>,
}

impl OrgColdAuthorityStamp {
    /// Assemble a stamp. Crate-internal: the only honest producer is the node
    /// seam that took the observation it describes.
    pub(crate) fn new(
        authority_org: OrgId,
        epoch: u64,
        poisoned: bool,
        floor_generation: u64,
        grants: Vec<([u8; 32], Option<OrgColdGrantAuthority>)>,
    ) -> Self {
        Self {
            authority_org,
            epoch,
            poisoned,
            floor_generation,
            grants,
        }
    }

    /// The organization that owns this node, as observed.
    pub fn authority_org(&self) -> OrgId {
        self.authority_org
    }

    /// The routing authority epoch the observation was taken under.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The revocation view's poison bit, sampled coherently with the generation
    /// that qualifies it.
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// The revocation floor generation every plane was filtered against.
    pub fn floor_generation(&self) -> u64 {
        self.floor_generation
    }

    /// The pinned installation of each requested grant scope, in the order the
    /// caller asked for them. `None` means the grant was not installed, which is
    /// itself part of the identity: an install landing afterwards moves the
    /// stamp.
    pub fn grants(&self) -> &[([u8; 32], Option<OrgColdGrantAuthority>)] {
        &self.grants
    }
}

/// The authority half of a capture: the ONE instant a plan is derived at, and
/// the exact authority identity it was derived under.
///
/// Its own value because the exported (public-plane) call path needs exactly
/// this and no private rows — one clock, one authority identity, one final
/// comparison — and querying the private planes to obtain a clock would be work
/// that path has no use for.
#[derive(Debug, Clone)]
pub struct OrgColdAuthority {
    now_secs: u64,
    stamp: OrgColdAuthorityStamp,
}

impl OrgColdAuthority {
    /// Assemble an authority capture. Crate-internal: the only honest producer
    /// is the node seam that took the observation it describes.
    pub(crate) fn new(now_secs: u64, stamp: OrgColdAuthorityStamp) -> Self {
        Self { now_secs, stamp }
    }

    /// The ONE instant this plan is derived at — every credential window, grant
    /// window and expiry filter in the plan uses exactly this value.
    pub fn now_secs(&self) -> u64 {
        self.now_secs
    }

    /// The authority identity the observation was taken under.
    pub fn stamp(&self) -> &OrgColdAuthorityStamp {
        &self.stamp
    }

    /// The organization that owns this node, as observed at capture time.
    pub fn authority_org(&self) -> OrgId {
        self.stamp.authority_org()
    }
}

/// One coherent observation of this node's private-discovery authority and the
/// rows it makes visible (OLB-2B.3d-pre).
///
/// Immutable by construction — there is no mutating accessor and no interior
/// mutability — so a plan derived from it cannot observe two states of the
/// world. Produced only by `MeshNode::org_cold_discovery`.
#[derive(Debug, Clone)]
pub struct OrgColdDiscovery {
    authority: OrgColdAuthority,
    owner: Vec<PrivateCapabilityProvider>,
    granted: Vec<([u8; 32], Arc<[PrivateCapabilityProvider]>)>,
}

impl OrgColdDiscovery {
    /// Assemble a capture. Crate-internal for the same reason the stamp's
    /// constructor is.
    pub(crate) fn new(
        authority: OrgColdAuthority,
        owner: Vec<PrivateCapabilityProvider>,
        granted: Vec<([u8; 32], Arc<[PrivateCapabilityProvider]>)>,
    ) -> Self {
        Self {
            authority,
            owner,
            granted,
        }
    }

    /// The authority half — the captured instant and identity.
    pub fn authority(&self) -> &OrgColdAuthority {
        &self.authority
    }

    /// The ONE instant this plan is derived at — every credential window, grant
    /// window and expiry filter in the plan uses exactly this value.
    pub fn now_secs(&self) -> u64 {
        self.authority.now_secs()
    }

    /// The authority identity the observation was taken under.
    pub fn stamp(&self) -> &OrgColdAuthorityStamp {
        self.authority.stamp()
    }

    /// The organization that owns this node, as observed at capture time.
    pub fn authority_org(&self) -> OrgId {
        self.authority.authority_org()
    }

    /// The owner-plane (same-org) providers of the requested capability.
    pub fn owner_providers(&self) -> &[PrivateCapabilityProvider] {
        &self.owner
    }

    /// The providers discoverable under `grant_id`, or an empty slice for a
    /// grant that was not requested or not installed.
    ///
    /// Linear in the number of requested grants, which is bounded by the held
    /// DISCOVER grants for one capability — a map would cost more than it saves
    /// at that size, and preserving the caller's order is what lets the plan
    /// walk its own grants in its own order.
    pub fn granted_providers(&self, grant_id: &[u8; 32]) -> &[PrivateCapabilityProvider] {
        self.granted
            .iter()
            .find(|(id, _)| id == grant_id)
            .map(|(_, rows)| rows.as_ref())
            .unwrap_or(&[])
    }
}
