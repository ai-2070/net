//! Fixtures-only test bridge for the organization exact-provider sensing
//! composition witness (`tests/sensing_org_exact_seam.rs`).
//!
//! Everything the composition needs is already public EXCEPT two things that
//! are `pub(crate)` for good reasons and must stay that way:
//!
//! * the authorized POPULATION derivation, which reads verified owner-private
//!   discovery and the TOFU pin map. Production has exactly one caller — the
//!   retention transaction — and exposing it would invite a caller that senses
//!   a population discovery never authorized;
//! * the composition of a projection with the candidate ORDER rule. The rule
//!   itself is public plain data; what is bridged here is the two-step
//!   application, so a witness cannot drift into a parallel test-only
//!   algorithm.
//!
//! # Why a separate file
//!
//! `mesh.rs` is declared `mod mesh;` — private — so a `pub` item inside it is
//! reachable from another crate only through an explicit re-export. A bridge
//! module one level up, where the module path is already public, is nameable as
//! `net::adapter::net::org_exact_sensing_bridge` without widening anything in
//! `mesh.rs`: the guard in `tests/sensing_org_exact_guards.rs` asserts that no
//! bridge identifier appears there at all. This mirrors the one existing
//! core-crate precedent, `subnet::alloc_probe`.
//!
//! # Darkness
//!
//! The whole module is gated on `test`/`fixtures`, carries no production
//! caller, and creates no production call edge. A fixtures-off build has
//! neither the module nor its symbols; `guards/fixtures_off_probe/` compiles
//! against exactly these names and MUST fail without the feature.

use std::sync::Arc;
use std::time::Instant;

use super::behavior::org_grant::CapabilityAuthorityId;
use super::behavior::org_sensing_demand::{
    org_sensed_bucket_permutation, OrgSensingCapabilityDemand,
};
use super::behavior::sensing;
use super::MeshNode;

/// Unstable fixtures-only test bridge; not supported core API.
///
/// The authorized exact-provider population for `tag`, as the retention
/// transaction derives it: verified owner-private discovery intersected with
/// this node's pinned peer entities, plus this node itself when it serves the
/// capability, bounded by the sensing population cap.
///
/// A witness needs this to prove the composition's INPUT is authorization, not
/// a test-supplied list — and to prove sensing adds none: whatever the
/// projection later reports about, it is a subset of exactly this.
#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub fn authorized_population(node: &Arc<MeshNode>, tag: &str) -> Vec<u64> {
    node.org_sensing_authorized_population(&CapabilityAuthorityId::for_tag(tag))
}

/// Unstable fixtures-only test bridge; not supported core API.
///
/// The deterministic candidate order one request implies: capture the
/// projection at `now` under `budget`, then apply the accepted bucket
/// permutation to the caller's COMPLETE candidate list, returning provider ids
/// in emission order.
///
/// `providers` and `same_org` are parallel views of that complete list, in the
/// caller's own deterministic order; the list is NOT bounded by the sensing
/// population cap. The result is a permutation of `providers`: no candidate is
/// invented, dropped or duplicated, sensed-viable ones lead in sensed rank
/// order, and a provider sensed not-ready is ordered last rather than removed.
///
/// This is the composition, not a second implementation: the classification
/// comes from `project_sensed_order` and the ordering from
/// `org_sensed_bucket_permutation`, both unchanged.
#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub fn sensed_provider_order(
    demand: &OrgSensingCapabilityDemand,
    now: Instant,
    budget: &sensing::ConsumerLatencyBudget,
    providers: &[u64],
    same_org: &[bool],
) -> Vec<u64> {
    let projection = demand.project_sensed_order(now, budget);
    org_sensed_bucket_permutation(
        same_org,
        providers,
        projection.viable(),
        projection.non_viable(),
    )
    .into_iter()
    .map(|index| providers[index])
    .collect()
}
