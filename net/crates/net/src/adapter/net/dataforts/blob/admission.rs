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

/// G-7 verdict: should the local node accept an inbound
/// `OverflowPush { hash, size }` from `sender_caps`?
///
/// Receive-side mirror of [`should_migrate_blob_to`] for the
/// active-overflow track ([`DATAFORTS_BLOB_OVERFLOW_PLAN.md`]).
/// Migration is *pull* (the local node decides to take an
/// advertised hot blob); overflow is *push* (a remote node
/// decides to shed a cold blob and the local node decides
/// whether to accept). The two functions are intentionally
/// close — every reject reason maps to a Prometheus counter
/// label so operators can dashboard both sides.
///
/// Hard `false` when any of:
///
/// 1. Local not blob-storage-participating.
/// 2. Local hasn't opted into overflow
///    (`cap.blob.overflow_enabled = false`).
/// 3. Sender hasn't opted into overflow — defends against
///    single-sided pushes where a non-overflow peer tries to
///    dump bytes onto an overflow-enabled node.
/// 4. Local advertising `dataforts:blob-storage-unhealthy`.
///    Refusing inbound while unhealthy prevents the failure
///    cascade where two near-full nodes push at each other.
/// 5. Sender's gravity scope outside the local gravity scope.
/// 6. Local `disk_free_gb` insufficient for the blob's
///    `size_bytes` (rounded up — same rule as
///    [`should_migrate_blob_to`]).
///
/// Returns [`OverflowVerdict::Admit`] when every gate passes.
///
/// **Sender-side opt-in.** Gate (3) reads `sender_caps`'s
/// `cap.blob.overflow_enabled` rather than treating the
/// presence of an `OverflowPush` as implicit opt-in. The
/// capability tag is the authoritative signal — a sender that
/// flips its own boolean off should not be able to push
/// (capability-index staleness is acceptable here: the next
/// re-broadcast catches up, and a one-tick window of
/// stale-rejected pushes is preferable to accepting bytes
/// from a peer that no longer claims to participate).
///
/// [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`]: ../../../../../docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md
pub fn should_accept_overflow_from(
    local_caps: &CapabilitySet,
    sender_caps: &CapabilitySet,
    blob_size_bytes: u64,
) -> OverflowVerdict {
    let local_blob = BlobCapability::from_capability_set(local_caps);
    let sender_blob = BlobCapability::from_capability_set(sender_caps);
    let local_gravity = GravityCapability::from_capability_set(local_caps);

    if !local_blob.storage {
        return OverflowVerdict::Reject(OverflowReject::NoStorageCap);
    }
    if !local_blob.overflow_enabled {
        return OverflowVerdict::Reject(OverflowReject::NotParticipating);
    }
    if !sender_blob.overflow_enabled {
        return OverflowVerdict::Reject(OverflowReject::SenderNotOverflowing);
    }
    if is_blob_storage_unhealthy(local_caps) {
        return OverflowVerdict::Reject(OverflowReject::Unhealthy);
    }
    if !scope_allows_cross(local_gravity.scope, sender_caps) {
        return OverflowVerdict::Reject(OverflowReject::ScopeMismatch);
    }
    let required_gb = blob_size_bytes.div_ceil(1 << 30);
    if local_blob.disk_free_gb < required_gb {
        return OverflowVerdict::Reject(OverflowReject::InsufficientDisk);
    }
    OverflowVerdict::Admit
}

/// G-7 verdict outcome. `Admit` = open the chunk channel
/// against the local Redex with replication armed; the wire
/// runtime pulls the bytes from any holder. `Reject(reason)`
/// = surface the typed reason in the `OverflowPushAck` reply
/// so the sender can route to a different target on the next
/// tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowVerdict {
    /// Local node accepts the inbound push.
    Admit,
    /// Local node should NOT accept; reason identifies the
    /// failed gate.
    Reject(OverflowReject),
}

/// Reasons [`should_accept_overflow_from`] rejects an inbound
/// push. Each maps to a distinct Prometheus counter label:
/// `dataforts_blob_overflow_pushes_rejected_total{reason}`.
/// Serializable so a receiver can carry the typed reason back
/// to the sender in [`super::overflow::OverflowPushAck`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OverflowReject {
    /// Local node doesn't carry `dataforts.blob.storage`.
    NoStorageCap,
    /// Local node's `cap.blob.overflow_enabled` is `false` —
    /// not opted into the overflow protocol.
    NotParticipating,
    /// Sender's `cap.blob.overflow_enabled` is `false`.
    /// Single-sided pushes are rejected: the sender must also
    /// be overflow-enabled for the symmetry to hold.
    SenderNotOverflowing,
    /// Local node currently advertising
    /// `dataforts:blob-storage-unhealthy`.
    Unhealthy,
    /// Sender's scope is outside the local gravity scope
    /// boundary (overflow reuses the gravity-scope axis; see
    /// "Should overflow have a separate scope axis from
    /// migration?" in `DATAFORTS_BLOB_OVERFLOW_PLAN.md`'s open
    /// design questions).
    ScopeMismatch,
    /// Local `disk_free_gb` < `ceil(size_bytes / 1 GiB)`.
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

    // --- should_accept_overflow_from (G-7) ---

    /// Fixture: an overflow-enabled receiver, gravity-participating
    /// at `scope`, with `disk_free_gb` headroom. Mirrors
    /// `participating_gravity_node` (the migration-target shape)
    /// because overflow reuses the gravity scope axis, and adds
    /// the `dataforts.blob.overflow` presence tag for opt-in.
    fn overflow_enabled_node(scope: TopologyScope, disk_free_gb: u64) -> CapabilitySet {
        participating_gravity_node(scope, 128, disk_free_gb)
            .add_tag("dataforts.blob.overflow")
    }

    /// Fixture: a sender that's also overflow-enabled and
    /// advertising a matching scope tag. The sender's
    /// `disk_*_gb` are irrelevant on the receive side — only
    /// `overflow_enabled` + scope tags are read.
    fn overflow_enabled_sender(scope: TopologyScope) -> CapabilitySet {
        publisher_with_scope(scope).add_tag("dataforts.blob.overflow")
    }

    #[test]
    fn overflow_admits_when_both_sides_opted_in() {
        let local = overflow_enabled_node(TopologyScope::Mesh, 100);
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Admit
        );
    }

    #[test]
    fn overflow_rejects_when_local_has_no_storage_cap() {
        // A compute-only node never accepts pushes regardless
        // of the overflow tag — the storage gate runs first.
        let local = CapabilitySet::new().add_tag("dataforts.blob.overflow");
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::NoStorageCap)
        );
    }

    #[test]
    fn overflow_rejects_when_local_not_participating() {
        // Local node carries blob.storage but hasn't flipped
        // the overflow boolean. The receiver-side master switch
        // gate fires before the sender-side opt-in check.
        let local = participating_gravity_node(TopologyScope::Mesh, 128, 100);
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        assert!(!BlobCapability::from_capability_set(&local).overflow_enabled);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::NotParticipating)
        );
    }

    #[test]
    fn overflow_rejects_when_sender_not_overflowing() {
        // Local opted in; sender did NOT. Defends against
        // single-sided pushes from non-participating peers.
        let local = overflow_enabled_node(TopologyScope::Mesh, 100);
        let sender = publisher_with_mesh_scope();
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::SenderNotOverflowing)
        );
    }

    #[test]
    fn overflow_rejects_when_local_unhealthy() {
        // Local is overflow-enabled with disk headroom but
        // currently advertising `dataforts:blob-storage-unhealthy`.
        // Refusing inbound while unhealthy prevents the
        // failure cascade (two near-full nodes pushing at each
        // other).
        let mut local = overflow_enabled_node(TopologyScope::Mesh, 100);
        local
            .tags
            .insert(crate::adapter::net::behavior::Tag::Reserved {
                prefix: "dataforts:".to_owned(),
                body: "blob-storage-unhealthy".to_owned(),
            });
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::Unhealthy)
        );
    }

    #[test]
    fn overflow_rejects_when_sender_scope_outside_local() {
        // Local gravity scope=Zone, sender advertises only a
        // Node-scope tag → narrower-than-local. `scope_allows_cross`
        // returns false; G-7 surfaces ScopeMismatch.
        let local = overflow_enabled_node(TopologyScope::Zone, 100);
        let sender = CapabilitySet::new()
            .add_tag("dataforts.gravity.scope=node")
            .add_tag("dataforts.greedy.scope=node")
            .add_tag("dataforts.blob.overflow");
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::ScopeMismatch)
        );
    }

    #[test]
    fn overflow_rejects_when_insufficient_disk() {
        // 2 GiB free, 10 GiB push → veto. Same disk-gate rule
        // as `should_migrate_blob_to`.
        let local = overflow_enabled_node(TopologyScope::Mesh, 2);
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        let ten_gib: u64 = 10 * (1 << 30);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, ten_gib),
            OverflowVerdict::Reject(OverflowReject::InsufficientDisk)
        );
    }

    #[test]
    fn overflow_disk_gate_rounds_up() {
        // 1 GiB free, 1.5 GiB push → ceil(1.5 / 1) = 2; 1 < 2
        // → veto. Then 2 GiB free → admit. Pin the rounding
        // direction so a 1.5-GiB push isn't accepted onto a
        // 1-GiB-free node by truncating-instead-of-rounding.
        let one_and_a_half_gib: u64 = (1 << 30) + (1 << 29);
        let sender = overflow_enabled_sender(TopologyScope::Mesh);

        let tight = overflow_enabled_node(TopologyScope::Mesh, 1);
        assert_eq!(
            should_accept_overflow_from(&tight, &sender, one_and_a_half_gib),
            OverflowVerdict::Reject(OverflowReject::InsufficientDisk)
        );

        let loose = overflow_enabled_node(TopologyScope::Mesh, 2);
        assert_eq!(
            should_accept_overflow_from(&loose, &sender, one_and_a_half_gib),
            OverflowVerdict::Admit
        );
    }

    #[test]
    fn overflow_reject_ordering_storage_before_overflow_opt_in() {
        // The gate order matters operationally: a compute-only
        // node should surface `NoStorageCap`, not
        // `NotParticipating`, even if both gates would reject.
        // The operator-actionable signal is "this node never
        // does blob storage at all," not "the overflow flag is
        // off." Pin the order against accidental reshuffling.
        let local = CapabilitySet::new(); // no storage, no overflow tag
        let sender = overflow_enabled_sender(TopologyScope::Mesh);
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::NoStorageCap)
        );
    }

    #[test]
    fn overflow_reject_ordering_local_overflow_before_sender_overflow() {
        // Similarly: a receiver that didn't opt in should
        // surface `NotParticipating`, not `SenderNotOverflowing`,
        // even when the sender ALSO didn't opt in. The
        // local-side flag is the operator's master switch on
        // this node; that's the more actionable signal.
        let local = participating_gravity_node(TopologyScope::Mesh, 128, 100);
        let sender = publisher_with_mesh_scope();
        assert_eq!(
            should_accept_overflow_from(&local, &sender, 1024),
            OverflowVerdict::Reject(OverflowReject::NotParticipating)
        );
    }
}
