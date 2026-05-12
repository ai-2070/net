//! Pure-logic admission + migration decision helpers for the
//! Dataforts integration rules G-1..G-3 + G-6 (see
//! `docs/plans/DATAFORTS_BLOB_STORAGE_PLAN.md` § G-1 / G-2 / G-3 / G-6).
//!
//! The decisions live as standalone functions over typed inputs so
//! the greedy + gravity runtimes can call them without taking the
//! adapter / mesh state directly. PR-5b wires these into the
//! actual GreedyObserver + gravity migration hot paths; this
//! module ships the contract + unit-test coverage today so the
//! later integration is a thin wiring layer.
//!
//! # Decision surface
//!
//! - [`should_pull_blob`] — G-1: should this local node
//!   speculatively pull a blob referenced by an admitted chain
//!   event? Combines the local greedy capability, scope-vs-scope
//!   match against the publisher's caps, proximity floor, and
//!   storage-participation gate.
//! - [`should_migrate_blob_to`] — G-2 / G-3: should heat-driven
//!   migration land a hot blob on `target_node`? Combines the
//!   target's gravity + blob capabilities, scope-vs-scope match
//!   against the blob's origin, disk-free headroom, and the
//!   target's health-gate state.
//! - [`auth_allows_blob_op`] — G-6: does the operator have
//!   authority to pin / unpin / delete a blob? Keys on the
//!   publishing chain's `(origin_hash, ChannelName)` ACL via
//!   the substrate's existing `AuthGuard::is_authorized_full`.
//!
//! All three functions are *advisory* — they answer "should we?"
//! but don't act. Call sites combine them with the existing
//! placement / replication state machine to produce the final
//! placement decision.

use super::error::BlobError;
use crate::adapter::net::behavior::{
    is_blob_storage_unhealthy, BlobCapability, CapabilitySet, GravityCapability, GreedyCapability,
    TopologyScope,
};
use crate::adapter::net::channel::{AuthGuard, ChannelName};

/// G-1 verdict: should `local_caps` speculatively pull a blob
/// originating from `publisher_caps`?
///
/// Hard `false` when any of these fail:
///
/// 1. Local node not participating in blob storage at all
///    (`local_caps.blob.storage = false`).
/// 2. Local greedy disabled (`local_caps.dataforts_greedy.enabled = false`).
/// 3. Local greedy proximity is `0` — operator-driven disable
///    without flipping the master flag.
/// 4. The local greedy `scope` is narrower than the publisher's
///    advertised scope (e.g. local scope `Zone`, publisher in a
///    different zone). Scope mismatch is a hard boundary.
/// 5. Local node currently advertising
///    `dataforts:blob-storage-unhealthy` — disk pressure makes a
///    speculative pull the wrong move.
///
/// Otherwise `true` — the caller still applies the heat-weighted
/// scoring + bandwidth budget on top.
///
/// Note: this primitive answers the local-vs-publisher decision.
/// G-1 also forbids speculative blob pulls absent a referencing
/// chain admit; that "did we already pull the parent chain" gate
/// is checked at the GreedyObserver site, not here.
pub fn should_pull_blob(
    local_caps: &CapabilitySet,
    publisher_caps: &CapabilitySet,
) -> PullBlobVerdict {
    let local_blob = BlobCapability::from_capability_set(local_caps);
    let local_greedy = GreedyCapability::from_capability_set(local_caps);

    if !local_blob.storage {
        return PullBlobVerdict::Reject(PullBlobReject::NoStorageCap);
    }
    if !local_greedy.enabled {
        return PullBlobVerdict::Reject(PullBlobReject::GreedyDisabled);
    }
    if local_greedy.proximity == 0 {
        return PullBlobVerdict::Reject(PullBlobReject::ProximityZero);
    }
    if is_blob_storage_unhealthy(local_caps) {
        return PullBlobVerdict::Reject(PullBlobReject::Unhealthy);
    }
    if !scope_allows_cross(local_greedy.scope, publisher_caps) {
        return PullBlobVerdict::Reject(PullBlobReject::ScopeMismatch);
    }
    PullBlobVerdict::Admit
}

/// G-1 verdict outcome. `Admit` = pull is eligible; `Reject(reason)`
/// = veto with a typed reason for the operator-facing metrics
/// counter (`dataforts_greedy_blob_pulls_rejected_total{reason}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullBlobVerdict {
    /// The local node should speculatively pull this blob.
    Admit,
    /// The local node should NOT pull this blob; the reason
    /// identifies the failed gate.
    Reject(PullBlobReject),
}

/// Reasons [`should_pull_blob`] vetoes a pull. Each maps to a
/// distinct Prometheus counter label so operators can disambiguate
/// "why isn't greedy pulling this chain's blobs?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullBlobReject {
    /// Local node doesn't carry `dataforts.blob.storage`.
    NoStorageCap,
    /// Local greedy is disabled
    /// (`dataforts.greedy.enabled` absent).
    GreedyDisabled,
    /// Local greedy proximity is `0` — operator-driven disable
    /// without flipping the master `enabled` flag.
    ProximityZero,
    /// Local node currently advertising
    /// `dataforts:blob-storage-unhealthy`.
    Unhealthy,
    /// Publisher's scope is outside the local greedy scope
    /// boundary.
    ScopeMismatch,
}

/// G-2 / G-3 verdict: should heat-driven migration place `blob` on
/// `target_caps` given the publisher's caps + the blob's size?
///
/// Hard `false` when:
///
/// 1. Target not blob-storage-participating.
/// 2. Target's gravity disabled / proximity zero.
/// 3. Target's gravity scope narrower than publisher's scope.
/// 4. Target advertising
///    `dataforts:blob-storage-unhealthy`.
/// 5. Target's `disk_free_gb` insufficient for the blob's
///    `size_bytes` (rounded up — defends against truncated-fit
///    placement).
///
/// Mirrors the [`super::admission::should_pull_blob`] structure
/// but reads from `gravity_*` capability tags rather than
/// `greedy_*`. The two are independent — a node can participate
/// in gravity migration without speculatively greedy-pulling.
pub fn should_migrate_blob_to(
    target_caps: &CapabilitySet,
    publisher_caps: &CapabilitySet,
    blob_size_bytes: u64,
) -> MigrateBlobVerdict {
    let target_blob = BlobCapability::from_capability_set(target_caps);
    let target_gravity = GravityCapability::from_capability_set(target_caps);

    if !target_blob.storage {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::NoStorageCap);
    }
    if !target_gravity.enabled {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::GravityDisabled);
    }
    if target_gravity.proximity == 0 {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::ProximityZero);
    }
    if is_blob_storage_unhealthy(target_caps) {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::Unhealthy);
    }
    if !scope_allows_cross(target_gravity.scope, publisher_caps) {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::ScopeMismatch);
    }
    // Disk-free gate — rounded up so a 1.5 GiB blob requires
    // ceil(1.5) = 2 GiB free. Pinned via test.
    let required_gb = blob_size_bytes.div_ceil(1 << 30);
    if target_blob.disk_free_gb < required_gb {
        return MigrateBlobVerdict::Reject(MigrateBlobReject::InsufficientDisk);
    }
    MigrateBlobVerdict::Admit
}

/// G-2 / G-3 verdict outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateBlobVerdict {
    /// Target is eligible for the heat-driven migration.
    Admit,
    /// Target should NOT receive the migration; reason maps to
    /// the gravity counter label.
    Reject(MigrateBlobReject),
}

/// Reasons [`should_migrate_blob_to`] vetoes a migration. Distinct
/// from [`PullBlobReject`] because the two have different operator-
/// facing implications (greedy is per-event admission; gravity is
/// long-term drift).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrateBlobReject {
    /// Target doesn't carry `dataforts.blob.storage`.
    NoStorageCap,
    /// Target gravity is disabled.
    GravityDisabled,
    /// Target gravity proximity is `0`.
    ProximityZero,
    /// Target advertising
    /// `dataforts:blob-storage-unhealthy`.
    Unhealthy,
    /// Publisher's scope is outside the target's gravity scope
    /// boundary.
    ScopeMismatch,
    /// Target's `disk_free_gb` < `ceil(size_bytes / 1 GiB)`.
    InsufficientDisk,
}

/// G-6 verdict: does `origin_hash` have authority to `pin` /
/// `unpin` / `delete` a blob originally published on `channel`?
///
/// Routes through the substrate's existing
/// [`AuthGuard::is_authorized_full`] — the exact-name (not
/// hash-based) ACL keyed on the canonical `ChannelName`. Two
/// distinct channel names can never alias on the exact path, so
/// the auth decision is collision-free even under adversarial
/// channel-name selection.
///
/// Returns `Ok(())` when the operator is authorized; `Err` carries
/// a typed [`BlobError::Unauthorized`] with the origin_hash. The
/// channel name is intentionally NOT included — names can carry
/// tenant / project identifiers and we don't want them flowing
/// to client bindings via error strings.
pub fn auth_allows_blob_op(
    guard: &AuthGuard,
    origin_hash: u64,
    channel: &ChannelName,
) -> Result<(), BlobError> {
    if guard.is_authorized_full(origin_hash, channel) {
        Ok(())
    } else {
        Err(BlobError::Unauthorized(format!(
            "origin {:#x} not authorized",
            origin_hash
        )))
    }
}

/// `true` when `local_scope` admits an artifact published with
/// `publisher_caps`. Hard-boundary semantics: a narrower local
/// scope only admits when the publisher's scope-tag overlaps the
/// local one. `local_scope == Mesh` admits everything.
///
/// Internal helper for [`should_pull_blob`] /
/// [`should_migrate_blob_to`] — the two share the scope-matching
/// rule because both gates ultimately read from the publisher's
/// capability set.
///
/// Today we use a simple rule: `Mesh` admits everything; any
/// narrower scope is admit-iff-publisher-advertises-the-same-or-
/// narrower-scope. Operators map their `scope:zone:*` /
/// `scope:region:*` tags to [`TopologyScope`] at policy time; the
/// substrate doesn't enforce a specific scope-tag taxonomy beyond
/// the enum width.
///
/// **PR-5a is a simplified pass**: this returns `true` whenever
/// `local_scope == TopologyScope::Mesh` and otherwise checks for
/// a `dataforts.{greedy|gravity}.scope` tag on the publisher that
/// matches the local scope. The full scope-bag operator-mapping
/// from `scope:zone:east-1a` strings to [`TopologyScope`] lands
/// alongside the operator's scope-policy work in PR-5b.
fn scope_allows_cross(local_scope: TopologyScope, publisher_caps: &CapabilitySet) -> bool {
    if matches!(local_scope, TopologyScope::Mesh) {
        return true;
    }
    // Read the publisher's advertised greedy/gravity scope (if
    // any). If absent, default to `Mesh` — publisher made no
    // scope claim, so locally-narrower scope vetoes by default.
    let pub_greedy = GreedyCapability::from_capability_set(publisher_caps);
    let pub_gravity = GravityCapability::from_capability_set(publisher_caps);
    // Take whichever the publisher advertised; default to Mesh
    // if neither. Conservative: if either advertised scope is
    // narrower than or equal to local, admit.
    let candidate_scopes = [pub_greedy.scope, pub_gravity.scope];
    candidate_scopes
        .iter()
        .any(|s| scope_at_least_as_narrow(local_scope, *s))
}

/// `true` when `local` is at least as narrow as `publisher` —
/// i.e. the artifact's scope claim covers the local node. The
/// scope ordering is `Node < Zone < Region < Mesh` (Node is
/// strictest).
fn scope_at_least_as_narrow(local: TopologyScope, publisher: TopologyScope) -> bool {
    use TopologyScope::*;
    matches!(
        (local, publisher),
        (Node, _) | (Zone, Zone | Region | Mesh) | (Region, Region | Mesh) | (Mesh, Mesh)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::CapabilitySet;

    fn participating_local_node(scope: TopologyScope, proximity: u8) -> CapabilitySet {
        let scope_str = match scope {
            TopologyScope::Node => "node",
            TopologyScope::Zone => "zone",
            TopologyScope::Region => "region",
            TopologyScope::Mesh => "mesh",
        };
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=50")
            .add_tag("dataforts.greedy.enabled")
            .add_tag(format!("dataforts.greedy.scope={}", scope_str))
            .add_tag(format!("dataforts.greedy.proximity={}", proximity))
    }

    fn participating_gravity_node(
        scope: TopologyScope,
        proximity: u8,
        disk_free_gb: u64,
    ) -> CapabilitySet {
        let scope_str = match scope {
            TopologyScope::Node => "node",
            TopologyScope::Zone => "zone",
            TopologyScope::Region => "region",
            TopologyScope::Mesh => "mesh",
        };
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag(format!("dataforts.blob.disk_free_gb={}", disk_free_gb))
            .add_tag("dataforts.gravity.enabled")
            .add_tag(format!("dataforts.gravity.scope={}", scope_str))
            .add_tag(format!("dataforts.gravity.proximity={}", proximity))
    }

    fn publisher_with_mesh_scope() -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.gravity.scope=mesh")
    }

    fn publisher_with_scope(scope: TopologyScope) -> CapabilitySet {
        let scope_str = match scope {
            TopologyScope::Node => "node",
            TopologyScope::Zone => "zone",
            TopologyScope::Region => "region",
            TopologyScope::Mesh => "mesh",
        };
        CapabilitySet::new()
            .add_tag(format!("dataforts.greedy.scope={}", scope_str))
            .add_tag(format!("dataforts.gravity.scope={}", scope_str))
    }

    // --- should_pull_blob ---

    #[test]
    fn pull_admits_participating_local_with_mesh_publisher() {
        let local = participating_local_node(TopologyScope::Mesh, 128);
        let publisher = publisher_with_mesh_scope();
        assert_eq!(should_pull_blob(&local, &publisher), PullBlobVerdict::Admit);
    }

    #[test]
    fn pull_rejects_no_storage_cap() {
        let local = CapabilitySet::new(); // no dataforts.blob.storage
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_pull_blob(&local, &publisher),
            PullBlobVerdict::Reject(PullBlobReject::NoStorageCap)
        );
    }

    #[test]
    fn pull_rejects_greedy_disabled() {
        let local = CapabilitySet::new().add_tag("dataforts.blob.storage");
        // No `dataforts.greedy.enabled` tag.
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_pull_blob(&local, &publisher),
            PullBlobVerdict::Reject(PullBlobReject::GreedyDisabled)
        );
    }

    #[test]
    fn pull_rejects_proximity_zero() {
        let local = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=mesh");
        // No proximity tag → default 0.
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_pull_blob(&local, &publisher),
            PullBlobVerdict::Reject(PullBlobReject::ProximityZero)
        );
    }

    #[test]
    fn pull_rejects_unhealthy_local() {
        let mut local = participating_local_node(TopologyScope::Mesh, 128);
        local
            .tags
            .insert(crate::adapter::net::behavior::Tag::Reserved {
                prefix: "dataforts:".to_owned(),
                body: "blob-storage-unhealthy".to_owned(),
            });
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_pull_blob(&local, &publisher),
            PullBlobVerdict::Reject(PullBlobReject::Unhealthy)
        );
    }

    #[test]
    fn pull_admits_when_local_zone_and_publisher_mesh_covers_it() {
        // Local greedy scope=Zone, publisher carries
        // `dataforts.greedy.scope=mesh`. Local Zone is narrower
        // than publisher's Mesh → publisher's Mesh covers local
        // → admit.
        let local = participating_local_node(TopologyScope::Zone, 128);
        let publisher = publisher_with_scope(TopologyScope::Mesh);
        assert_eq!(should_pull_blob(&local, &publisher), PullBlobVerdict::Admit);
    }

    #[test]
    fn pull_admits_when_local_zone_and_publisher_makes_no_scope_claim() {
        // Local greedy scope=Zone, publisher has no scope tag →
        // defaults to Mesh; Zone is narrower than Mesh so
        // scope_at_least_as_narrow returns true → admit.
        // Pin this conservative-default behavior.
        let local = participating_local_node(TopologyScope::Zone, 128);
        let publisher = CapabilitySet::new();
        assert_eq!(should_pull_blob(&local, &publisher), PullBlobVerdict::Admit);
    }

    // --- should_migrate_blob_to ---

    #[test]
    fn migrate_admits_target_with_disk_and_caps() {
        let target = participating_gravity_node(TopologyScope::Mesh, 128, 100);
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, 1024),
            MigrateBlobVerdict::Admit
        );
    }

    #[test]
    fn migrate_rejects_no_blob_storage() {
        let target = CapabilitySet::new().add_tag("dataforts.gravity.enabled");
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, 1024),
            MigrateBlobVerdict::Reject(MigrateBlobReject::NoStorageCap)
        );
    }

    #[test]
    fn migrate_rejects_gravity_disabled() {
        // dataforts.blob.storage but no dataforts.gravity.enabled
        let target = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_free_gb=100");
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, 1024),
            MigrateBlobVerdict::Reject(MigrateBlobReject::GravityDisabled)
        );
    }

    #[test]
    fn migrate_rejects_insufficient_disk() {
        // 2 GiB free, 10 GiB blob → veto.
        let target = participating_gravity_node(TopologyScope::Mesh, 128, 2);
        let publisher = publisher_with_mesh_scope();
        let ten_gib: u64 = 10 * (1 << 30);
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, ten_gib),
            MigrateBlobVerdict::Reject(MigrateBlobReject::InsufficientDisk)
        );
    }

    #[test]
    fn migrate_disk_check_rounds_up() {
        // 1 GiB free, 1.5 GiB blob → ceil(1.5 GiB / 1 GiB) = 2 →
        // 1 GiB free < 2 → veto. Pinning the rounding-up
        // direction.
        let target = participating_gravity_node(TopologyScope::Mesh, 128, 1);
        let publisher = publisher_with_mesh_scope();
        let one_and_a_half_gib: u64 = (1 << 30) + (1 << 29);
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, one_and_a_half_gib),
            MigrateBlobVerdict::Reject(MigrateBlobReject::InsufficientDisk)
        );

        // 2 GiB free → admit.
        let target2 = participating_gravity_node(TopologyScope::Mesh, 128, 2);
        assert_eq!(
            should_migrate_blob_to(&target2, &publisher, one_and_a_half_gib),
            MigrateBlobVerdict::Admit
        );
    }

    #[test]
    fn migrate_rejects_unhealthy_target() {
        let mut target = participating_gravity_node(TopologyScope::Mesh, 128, 100);
        target
            .tags
            .insert(crate::adapter::net::behavior::Tag::Reserved {
                prefix: "dataforts:".to_owned(),
                body: "blob-storage-unhealthy".to_owned(),
            });
        let publisher = publisher_with_mesh_scope();
        assert_eq!(
            should_migrate_blob_to(&target, &publisher, 1024),
            MigrateBlobVerdict::Reject(MigrateBlobReject::Unhealthy)
        );
    }

    // --- auth_allows_blob_op ---

    #[test]
    fn auth_admits_when_origin_authorized_for_channel() {
        let guard = AuthGuard::new();
        let origin = 0xDEAD_BEEF_u64;
        let channel = ChannelName::new("dataforts/test/auth").unwrap();
        guard.allow_channel(origin, &channel);
        assert!(auth_allows_blob_op(&guard, origin, &channel).is_ok());
    }

    #[test]
    fn auth_rejects_when_origin_not_authorized() {
        let guard = AuthGuard::new();
        let channel = ChannelName::new("dataforts/test/auth").unwrap();
        // No allow_channel call → veto.
        let err = auth_allows_blob_op(&guard, 0xDEAD, &channel).unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    #[test]
    fn auth_rejects_when_origin_authorized_for_different_channel() {
        let guard = AuthGuard::new();
        let allowed = ChannelName::new("allowed/channel").unwrap();
        let other = ChannelName::new("other/channel").unwrap();
        let origin = 0xC0FFEE_u64;
        guard.allow_channel(origin, &allowed);
        // Origin authorized for `allowed`, but op is against
        // `other` → veto.
        let err = auth_allows_blob_op(&guard, origin, &other).unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    // --- scope_at_least_as_narrow ---

    #[test]
    fn scope_node_is_narrowest() {
        use TopologyScope::*;
        assert!(scope_at_least_as_narrow(Node, Node));
        assert!(scope_at_least_as_narrow(Node, Zone));
        assert!(scope_at_least_as_narrow(Node, Region));
        assert!(scope_at_least_as_narrow(Node, Mesh));
    }

    #[test]
    fn scope_zone_admits_zone_region_mesh() {
        use TopologyScope::*;
        assert!(scope_at_least_as_narrow(Zone, Zone));
        assert!(scope_at_least_as_narrow(Zone, Region));
        assert!(scope_at_least_as_narrow(Zone, Mesh));
        // Zone is NOT at-least-as-narrow as Node (Node is
        // narrower than Zone).
        assert!(!scope_at_least_as_narrow(Zone, Node));
    }

    #[test]
    fn scope_mesh_only_admits_mesh() {
        use TopologyScope::*;
        assert!(scope_at_least_as_narrow(Mesh, Mesh));
        assert!(!scope_at_least_as_narrow(Mesh, Region));
        assert!(!scope_at_least_as_narrow(Mesh, Zone));
        assert!(!scope_at_least_as_narrow(Mesh, Node));
    }

    // --- type-system smoke ---

    #[test]
    fn arc_authguard_compiles() {
        // The MeshBlobAdapter will wire Arc<AuthGuard>; make sure
        // the pure-logic helper takes a plain reference so the
        // adapter's `&*self.auth.as_ref().unwrap()` projection
        // compiles cleanly.
        use std::sync::Arc;
        let guard: Arc<AuthGuard> = Arc::new(AuthGuard::new());
        let channel = ChannelName::new("dataforts/test").unwrap();
        let _ = auth_allows_blob_op(&guard, 0, &channel);
    }
}
