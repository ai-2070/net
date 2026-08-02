//! S4A of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — exact
//! attachments and local gateway authority.
//!
//! Two distinctions carry this slice:
//!
//! 1. **`attachment` is not `scope`.** A grant scoped at `vehicle` and
//!    presented for `camera-domain` admits the camera domain only.
//!    Using the scope as the peer's location would place that one peer
//!    everywhere beneath `vehicle`, and a gateway would then mistake a
//!    boundary crossing for an internal transition.
//! 2. **A peer context is not gateway authority.** Forwarding rights
//!    belong to the relaying node, proven by a credential naming that
//!    node as subject — never inherited from whichever peer happens to
//!    be talking to it.

#![cfg(feature = "net")]

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    admission::unix_now_secs, auth::compile_gateway_context, build_gateway_context_set,
    ForwardDenial, SubnetAuthError, SubnetAuthorityConfig, SubnetBoundarySet, SubnetCredentialSet,
    SubnetFloorRegistry, SubnetGrant, SubnetRights, TopologySubnetId, VerifiedGatewayContext,
    VerifiedGatewayContextSet, VerifiedSubnetContext, MAX_GATEWAY_CONTEXTS_PER_AUTHORITY,
};

const DAY: u64 = 24 * 60 * 60;

fn kp(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}

fn now() -> u64 {
    unix_now_secs()
}

fn config(root: &EntityKeypair) -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

fn grant(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            root,
            root.entity_id().clone(),
            TopologySubnetId::new(scope),
            0,
            subject.entity_id().clone(),
            rights,
            1,
            now() - 60,
            DAY,
        )
        .expect("issue grant"),
    )
}

/// A compiled peer context, built directly so these tests exercise the
/// decision rule rather than re-running the S3 admission handshake
/// (which `subnet_session_auth.rs` already pins).
fn peer_ctx(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    attachment: &[u8],
    scope: &[u8],
    rights: SubnetRights,
) -> VerifiedSubnetContext {
    VerifiedSubnetContext {
        authority: root.entity_id().clone(),
        attachment: TopologySubnetId::new(attachment),
        scope: TopologySubnetId::new(scope),
        topology_epoch: 0,
        subject: subject.entity_id().clone(),
        subject_node: subject.entity_id().node_id(),
        session_id: 1,
        rights,
        generation: 1,
        subnet_auth_epoch: 0,
        expires_at: now() + DAY,
        credential_set_hash: [0; 32],
    }
}

fn gateway_entry(
    root: &EntityKeypair,
    local: &EntityKeypair,
    scope: &[u8],
    rights: SubnetRights,
) -> VerifiedGatewayContext {
    compile_gateway_context(
        &grant(root, local, scope, rights),
        local.entity_id(),
        TopologySubnetId::new(scope),
        &config(root),
        0,
        &SubnetFloorRegistry::new(),
        now(),
        60,
    )
    .expect("compile gateway entry")
}

fn gateway_set(
    root: &EntityKeypair,
    entries: Vec<VerifiedGatewayContext>,
) -> VerifiedGatewayContextSet {
    build_gateway_context_set(root.entity_id(), entries).expect("build set")
}

/// Declared boundary inventory — a separate surface from the
/// credentials that satisfy it.
fn boundaries(root: &EntityKeypair, scopes: &[&[u8]]) -> SubnetBoundarySet {
    SubnetBoundarySet::new(
        root.entity_id().clone(),
        0,
        scopes.iter().map(|s| TopologySubnetId::new(s)),
    )
}

// ---------------------------------------------------------------------------
// attachment vs scope
// ---------------------------------------------------------------------------

/// A parent-scoped credential attached at a child records the child as
/// `attachment` and the parent as `scope`. Conflating them is the bug
/// this field split exists to prevent.
#[test]
fn attachment_is_the_exact_admitted_point_not_the_grant_scope() {
    let root = kp(1);
    let camera = kp(2);
    let ctx = peer_ctx(&root, &camera, &[3, 7, 2], &[3], SubnetRights::ATTACH);
    assert_eq!(ctx.attachment, TopologySubnetId::new(&[3, 7, 2]));
    assert_eq!(ctx.scope, TopologySubnetId::new(&[3]));
    assert_ne!(
        ctx.attachment, ctx.scope,
        "a parent-scoped grant attached at a child must not report the parent as its location",
    );
}

/// The forwarding decision reads `attachment`. If it read `scope`, a
/// camera peer holding a vehicle-scoped grant would appear to sit at
/// the vehicle root and an export boundary between them would vanish.
#[test]
fn forwarding_uses_attachment_not_scope() {
    let root = kp(1);
    let bset = boundaries(&root, &[&[3, 7, 1]]);
    let local = kp(9);
    let camera = kp(2);
    let outside = kp(3);

    // Local gateway: ROUTE over the whole vehicle, EXPORT only at
    // world-model.
    let set = gateway_set(
        &root,
        vec![
            gateway_entry(
                &root,
                &local,
                &[3],
                SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            ),
            gateway_entry(
                &root,
                &local,
                &[3, 7, 1],
                SubnetRights::ATTACH.union(SubnetRights::EXPORT),
            ),
        ],
    );

    // Both peers hold VEHICLE-scoped grants, but are attached at
    // different points: one inside world-model, one outside it.
    let inside = peer_ctx(&root, &camera, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let out = peer_ctx(&root, &outside, &[3, 8], &[3], SubnetRights::ATTACH);

    // Attachments differ across the world-model boundary, so this is a
    // crossing and the EXPORT entry governs it. Had the rule used
    // `scope`, both peers would look like `[3]` and this would be a
    // plain internal ROUTE.
    set.authorize_transition(&inside, &out, &bset, 0, 0, now())
        .expect("EXPORT(world-model) authorizes the crossing");

    // Remove EXPORT from the boundary entry: the same transition now
    // fails, proving the boundary was actually evaluated.
    let no_export = gateway_set(
        &root,
        vec![
            gateway_entry(
                &root,
                &local,
                &[3],
                SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            ),
            gateway_entry(&root, &local, &[3, 7, 1], SubnetRights::ATTACH),
        ],
    );
    assert_eq!(
        no_export
            .authorize_transition(&inside, &out, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ExportMissing,
    );
}

// ---------------------------------------------------------------------------
// Local gateway authority
// ---------------------------------------------------------------------------

/// The local credential's subject must be this process. A credential
/// issued to another node never becomes local forwarding authority.
#[test]
fn gateway_credential_subject_must_be_the_local_entity() {
    let root = kp(1);
    let local = kp(9);
    let someone_else = kp(4);
    let err = compile_gateway_context(
        &grant(&root, &someone_else, &[3], SubnetRights::ROUTE),
        local.entity_id(),
        TopologySubnetId::new(&[3]),
        &config(&root),
        0,
        &SubnetFloorRegistry::new(),
        now(),
        60,
    )
    .unwrap_err();
    assert_eq!(err, SubnetAuthError::WrongSubject);
}

/// The local attachment must lie inside the credential's scope.
#[test]
fn local_attachment_must_be_inside_the_credential_scope() {
    let root = kp(1);
    let local = kp(9);
    let err = compile_gateway_context(
        &grant(&root, &local, &[3, 7], SubnetRights::ROUTE),
        local.entity_id(),
        TopologySubnetId::new(&[4]),
        &config(&root),
        0,
        &SubnetFloorRegistry::new(),
        now(),
        60,
    )
    .unwrap_err();
    assert_eq!(err, SubnetAuthError::ScopeNotAncestor);
}

/// Entries dedupe by scope with a rights union, and the merged entry
/// takes the tightest expiry so it cannot outlive the shorter
/// credential behind it.
#[test]
fn entries_dedupe_by_scope_and_take_the_tightest_expiry() {
    let root = kp(1);
    let local = kp(9);
    let mut route = gateway_entry(&root, &local, &[3], SubnetRights::ROUTE);
    let mut export = gateway_entry(&root, &local, &[3], SubnetRights::EXPORT);
    route.expires_at = now() + DAY;
    export.expires_at = now() + 3600;

    let set = gateway_set(&root, vec![route, export]);
    assert_eq!(set.entries.len(), 1, "same scope collapses to one entry");
    assert!(set.entries[0].rights.contains(SubnetRights::ROUTE));
    assert!(set.entries[0].rights.contains(SubnetRights::EXPORT));
    assert_eq!(
        set.entries[0].expires_at,
        now() + 3600,
        "merged entry expires with the shortest-lived credential",
    );
}

/// Recompiling drops rights that are no longer backed by a current
/// credential — the set is replaced wholesale, never merged into.
#[test]
fn stale_rights_do_not_accumulate_across_recompile() {
    let root = kp(1);
    let local = kp(9);
    let both = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &local, &[3], SubnetRights::ROUTE),
            gateway_entry(&root, &local, &[3], SubnetRights::EXPORT),
        ],
    );
    assert!(both.entries[0].rights.contains(SubnetRights::EXPORT));

    // EXPORT credential revoked/expired: recompile from what remains.
    let after = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    assert!(after.entries[0].rights.contains(SubnetRights::ROUTE));
    assert!(
        !after.entries[0].rights.contains(SubnetRights::EXPORT),
        "a rights bit must not survive the credential that granted it",
    );
}

#[test]
fn gateway_set_is_capped_and_authority_homogeneous() {
    let root = kp(1);
    let local = kp(9);
    // One distinct scope per level-2 value, past the cap.
    let mut entries = Vec::new();
    for i in 1..=(MAX_GATEWAY_CONTEXTS_PER_AUTHORITY + 1) {
        entries.push(gateway_entry(
            &root,
            &local,
            &[3, i as u8],
            SubnetRights::ROUTE,
        ));
    }
    assert_eq!(
        build_gateway_context_set(root.entity_id(), entries).unwrap_err(),
        SubnetAuthError::TooManyGatewayContexts,
    );

    // An entry from another authority is refused outright.
    let other = kp(5);
    let foreign = gateway_entry(&other, &local, &[3], SubnetRights::ROUTE);
    assert_eq!(
        build_gateway_context_set(root.entity_id(), vec![foreign]).unwrap_err(),
        SubnetAuthError::WrongAuthority,
    );
}

// ---------------------------------------------------------------------------
// The D6 decision rule
// ---------------------------------------------------------------------------

/// A broad `ROUTE(vehicle)` cannot carry traffic out through a
/// narrower configured `EXPORT(world-model)` boundary. The crossed
/// boundary is evaluated first precisely so a wider grant cannot
/// swallow a narrower one.
#[test]
fn broad_route_cannot_bypass_a_narrower_export_boundary() {
    let root = kp(1);
    let bset = boundaries(&root, &[&[3, 7, 1]]);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    let set = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &local, &[3], SubnetRights::ROUTE),
            // world-model boundary WITHOUT export
            gateway_entry(&root, &local, &[3, 7, 1], SubnetRights::ATTACH),
        ],
    );
    let inside = peer_ctx(&root, &a, &[3, 7, 1], &[3, 7, 1], SubnetRights::ATTACH);
    let outside = peer_ctx(&root, &b, &[3, 8], &[3, 8], SubnetRights::ATTACH);

    assert_eq!(
        set.authorize_transition(&inside, &outside, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ExportMissing,
        "ROUTE(vehicle) must not authorize a crossing of the world-model boundary",
    );
}

/// Crossing two configured sibling boundaries requires EXPORT on both.
#[test]
fn crossing_two_sibling_boundaries_requires_both_exports() {
    let root = kp(1);
    let bset = boundaries(&root, &[&[3, 7], &[3, 8]]);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    let in_left = peer_ctx(&root, &a, &[3, 7, 1], &[3, 7], SubnetRights::ATTACH);
    let in_right = peer_ctx(&root, &b, &[3, 8, 1], &[3, 8], SubnetRights::ATTACH);

    // Only the left boundary exports.
    let one = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &local, &[3, 7], SubnetRights::EXPORT),
            gateway_entry(&root, &local, &[3, 8], SubnetRights::ATTACH),
        ],
    );
    assert_eq!(
        one.authorize_transition(&in_left, &in_right, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ExportMissing,
    );

    // Both export.
    let both = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &local, &[3, 7], SubnetRights::EXPORT),
            gateway_entry(&root, &local, &[3, 8], SubnetRights::EXPORT),
        ],
    );
    both.authorize_transition(&in_left, &in_right, &bset, 0, 0, now())
        .expect("both boundaries export");
}

#[test]
fn route_covers_internal_transitions_and_attach_alone_forwards_nothing() {
    let root = kp(1);
    let bset = boundaries(&root, &[]);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);
    let x = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 7, 2], &[3], SubnetRights::ATTACH);

    // Parent ROUTE covers descendant-to-descendant.
    gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    )
    .authorize_transition(&x, &y, &bset, 0, 0, now())
    .expect("parent ROUTE covers descendants");

    // ATTACH alone forwards nothing.
    assert_eq!(
        gateway_set(
            &root,
            vec![gateway_entry(&root, &local, &[3], SubnetRights::ATTACH)],
        )
        .authorize_transition(&x, &y, &bset, 0, 0, now())
        .unwrap_err(),
        ForwardDenial::RouteMissing,
        "ATTACH is admission, never forwarding",
    );

    // A child-scoped ROUTE cannot route a sibling transition: it
    // contains one endpoint but not the other, so no entry covers the
    // pair.
    assert_eq!(
        gateway_set(
            &root,
            vec![gateway_entry(
                &root,
                &local,
                &[3, 7, 1],
                SubnetRights::ROUTE
            )],
        )
        .authorize_transition(&x, &y, &bset, 0, 0, now())
        .unwrap_err(),
        ForwardDenial::RouteMissing,
    );
}

// ---------------------------------------------------------------------------
// Boundaries are declared, not inferred from credentials
// ---------------------------------------------------------------------------

/// The S4A review's blocker 4. Boundaries used to be discovered from
/// the gateway's own credential set, which inverted revocation:
/// dropping `EXPORT(world-model)` deleted the world-model entry, the
/// crossing stopped being a crossing, and a broader `ROUTE(vehicle)`
/// silently carried the same traffic. Removing a credential widened
/// authority.
///
/// With the boundary declared separately, the same removal leaves the
/// boundary standing and unsatisfied — which denies.
#[test]
fn revoking_an_export_credential_denies_rather_than_widening() {
    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);
    // world-model is a declared boundary regardless of credentials.
    let bset = boundaries(&root, &[&[3, 7, 1]]);
    let inside = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let outside = peer_ctx(&root, &b, &[3, 8], &[3], SubnetRights::ATTACH);

    // With EXPORT at the boundary the crossing is authorized.
    let with_export = gateway_set(
        &root,
        vec![
            gateway_entry(&root, &local, &[3], SubnetRights::ROUTE),
            gateway_entry(&root, &local, &[3, 7, 1], SubnetRights::EXPORT),
        ],
    );
    with_export
        .authorize_transition(&inside, &outside, &bset, 0, 0, now())
        .expect("EXPORT at the declared boundary authorizes the crossing");

    // Revoke ONLY the EXPORT credential, leaving the broad ROUTE. The
    // boundary is still declared, so the crossing still requires an
    // EXPORT nobody holds.
    let revoked = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    assert_eq!(
        revoked
            .authorize_transition(&inside, &outside, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ExportMissing,
        "revoking EXPORT must deny, never fall back to the broader ROUTE",
    );
}

/// A boundary set for the wrong authority or a superseded topology
/// epoch is not usable: boundaries mean nothing once paths are
/// reinterpreted, and one authority's boundaries do not govern
/// another's.
#[test]
fn boundary_sets_are_authority_and_epoch_bound() {
    let root = kp(1);
    let other = kp(5);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);
    let set = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    let x = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 7, 2], &[3], SubnetRights::ATTACH);

    let foreign = boundaries(&other, &[]);
    assert_eq!(
        set.authorize_transition(&x, &y, &foreign, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );

    let stale_epoch = SubnetBoundarySet::new(root.entity_id().clone(), 7, []);
    assert_eq!(
        set.authorize_transition(&x, &y, &stale_epoch, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );
}

/// A peer that has not proved ATTACH is not a forwardable endpoint,
/// and neither is one whose context is stale.
#[test]
fn peers_must_be_attached_and_current() {
    let root = kp(1);
    let bset = boundaries(&root, &[]);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);
    let set = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    let ok = peer_ctx(&root, &a, &[3, 7], &[3], SubnetRights::ATTACH);

    // Peer holding ROUTE but not ATTACH is not an endpoint.
    let unattached = peer_ctx(&root, &b, &[3, 8], &[3], SubnetRights::ROUTE);
    assert_eq!(
        set.authorize_transition(&ok, &unattached, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::AttachMissing,
    );

    // Wrong topology epoch.
    assert_eq!(
        set.authorize_transition(&ok, &ok, &bset, 1, 0, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );
    // Stale auth epoch.
    assert_eq!(
        set.authorize_transition(&ok, &ok, &bset, 0, 1, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );
    // Expired peer context.
    let mut expired = ok.clone();
    expired.expires_at = now() - 1;
    assert_eq!(
        set.authorize_transition(&expired, &ok, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );
}

/// Equal path bits under a different authority are a different place.
#[test]
fn cross_authority_peers_are_not_forwardable() {
    let root = kp(1);
    let bset = boundaries(&root, &[]);
    let other = kp(5);
    let local = kp(9);
    let a = kp(2);
    let set = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    let ours = peer_ctx(&root, &a, &[3, 7], &[3], SubnetRights::ATTACH);
    let theirs = peer_ctx(&other, &a, &[3, 7], &[3], SubnetRights::ATTACH);
    assert_eq!(
        set.authorize_transition(&ours, &theirs, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
    );
}
