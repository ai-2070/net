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
    MAX_TRANSITION_LOOKUPS,
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
    assert_eq!(set.entries().len(), 1, "same scope collapses to one entry");
    assert!(set.entries()[0].rights.contains(SubnetRights::ROUTE));
    assert!(set.entries()[0].rights.contains(SubnetRights::EXPORT));
    assert_eq!(
        set.entries()[0].expires_at,
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
    assert!(both.entries()[0].rights.contains(SubnetRights::EXPORT));

    // EXPORT credential revoked/expired: recompile from what remains.
    let after = gateway_set(
        &root,
        vec![gateway_entry(&root, &local, &[3], SubnetRights::ROUTE)],
    );
    assert!(after.entries()[0].rights.contains(SubnetRights::ROUTE));
    assert!(
        !after.entries()[0].rights.contains(SubnetRights::EXPORT),
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

// ---------------------------------------------------------------------------
// off-path scope index (D6)
// ---------------------------------------------------------------------------

/// Build a gateway entry directly, bypassing credential compilation.
///
/// `compile_gateway_context` requires every scope to contain the local
/// attachment, so credentials alone can only ever produce one ancestor
/// chain — at most five entries. These index tests need a set far
/// wider than that to show that width does not cost anything, which is
/// exactly the case a credential path cannot construct.
fn raw_entry(
    root: &EntityKeypair,
    local: &EntityKeypair,
    scope: TopologySubnetId,
    rights: SubnetRights,
) -> VerifiedGatewayContext {
    VerifiedGatewayContext {
        authority: root.entity_id().clone(),
        attachment: scope,
        scope,
        topology_epoch: 0,
        subject: local.entity_id().clone(),
        rights,
        generation: 1,
        subnet_auth_epoch: 0,
        expires_at: now() + DAY,
        credential_set_hash: [0; 32],
    }
}

/// The reason the index exists: the *shape* of a transition must not
/// depend on how much an operator provisioned.
///
/// The same decision is evaluated against a two-entry gateway and a
/// gateway holding the maximum, with the boundary inventory likewise
/// padded. If evaluation still walked either collection, the wide case
/// would issue more lookups. It must not issue even one more.
///
/// This pins lookup *calls*, not CPU cost. Each call is a binary
/// search, so wider inventories still cost more comparisons inside a
/// call — the claim being defended is that no linear credential or
/// boundary scan survives on the packet path, not that forwarding is
/// literally inventory-independent.
#[test]
fn lookup_count_does_not_grow_with_what_is_held() {
    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    let x = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 7, 2], &[3], SubnetRights::ATTACH);

    // Narrow: exactly what this transition needs.
    let narrow_entries = vec![
        raw_entry(
            &root,
            &local,
            TopologySubnetId::new(&[3]),
            SubnetRights::ROUTE,
        ),
        raw_entry(
            &root,
            &local,
            TopologySubnetId::new(&[3, 7]),
            SubnetRights::ATTACH,
        ),
    ];
    // Wide: the same two, buried among the maximum number of
    // irrelevant scopes.
    let mut wide_entries = narrow_entries.clone();
    let mut filler = 0u8;
    while wide_entries.len() < MAX_GATEWAY_CONTEXTS_PER_AUTHORITY {
        filler += 1;
        let scope = TopologySubnetId::new(&[200, filler]);
        wide_entries.push(raw_entry(&root, &local, scope, SubnetRights::ROUTE));
    }
    assert_eq!(wide_entries.len(), MAX_GATEWAY_CONTEXTS_PER_AUTHORITY);

    let narrow = gateway_set(&root, narrow_entries);
    let wide = gateway_set(&root, wide_entries);

    // Same asymmetry on the boundary side: one declared boundary
    // versus many, none of which lie between these two endpoints.
    let few = boundaries(&root, &[&[9, 9]]);
    let many_scopes: Vec<[u8; 2]> = (1..=64u8).map(|i| [201, i]).collect();
    let many = SubnetBoundarySet::new(
        root.entity_id().clone(),
        0,
        many_scopes.iter().map(|s| TopologySubnetId::new(s)),
    );

    let baseline = narrow.authorize_transition_counted(&x, &y, &few, 0, 0, now());
    assert_eq!(baseline.verdict, Ok(()), "the narrow gateway authorizes");

    for (label, set, bset) in [
        ("wide gateway", &wide, &few),
        ("wide boundaries", &narrow, &many),
        ("both wide", &wide, &many),
    ] {
        let decision = set.authorize_transition_counted(&x, &y, bset, 0, 0, now());
        assert_eq!(
            decision.verdict, baseline.verdict,
            "{label}: the verdict must not change",
        );
        assert_eq!(
            decision.lookup_calls, baseline.lookup_calls,
            "{label}: probes must not grow with what is held",
        );
    }
}

/// The advertised ceiling holds across every shape of transition, not
/// just the convenient ones.
///
/// Deepest attachments, disjoint branches, identical endpoints, a
/// boundary at every level of both chains — whichever branch the
/// decision takes, it stays under [`MAX_TRANSITION_LOOKUPS`].
#[test]
fn lookup_count_never_exceeds_the_advertised_bound() {
    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    // A boundary at every level of both chains, so the crossing branch
    // probes as much as it possibly can.
    let bset = boundaries(
        &root,
        &[
            &[1],
            &[1, 2],
            &[1, 2, 3],
            &[1, 2, 3, 4],
            &[9],
            &[9, 8],
            &[9, 8, 7],
            &[9, 8, 7, 6],
        ],
    );
    // Hold EXPORT and ROUTE at every one of those scopes plus the
    // root, so no branch short-circuits early on a missing right.
    let mut entries = vec![raw_entry(
        &root,
        &local,
        TopologySubnetId::GLOBAL,
        SubnetRights::ROUTE,
    )];
    for scope in [
        &[1u8][..],
        &[1, 2][..],
        &[1, 2, 3][..],
        &[1, 2, 3, 4][..],
        &[9][..],
        &[9, 8][..],
        &[9, 8, 7][..],
        &[9, 8, 7, 6][..],
    ] {
        entries.push(raw_entry(
            &root,
            &local,
            TopologySubnetId::new(scope),
            SubnetRights::EXPORT.union(SubnetRights::ROUTE),
        ));
    }
    let set = gateway_set(&root, entries);

    let paths: [&[u8]; 7] = [
        &[],
        &[1],
        &[1, 2],
        &[1, 2, 3],
        &[1, 2, 3, 4],
        &[9, 8, 7, 6],
        &[5, 5, 5, 5],
    ];
    let mut saw_crossing = false;
    let mut saw_internal = false;
    for source in paths {
        for target in paths {
            let x = peer_ctx(&root, &a, source, &[], SubnetRights::ATTACH);
            let y = peer_ctx(&root, &b, target, &[], SubnetRights::ATTACH);
            let decision = set.authorize_transition_counted(&x, &y, &bset, 0, 0, now());
            assert!(
                decision.lookup_calls <= MAX_TRANSITION_LOOKUPS,
                "{:?} -> {:?} cost {} lookups, over the bound of {MAX_TRANSITION_LOOKUPS}",
                source,
                target,
                decision.lookup_calls,
            );
            // Both branches must actually be exercised, or the bound
            // is only proven for whichever one happened to run.
            if source == target {
                saw_internal = true;
            } else if TopologySubnetId::new(source).common_ancestor(TopologySubnetId::new(target))
                != TopologySubnetId::new(source)
            {
                saw_crossing = true;
            }
        }
    }
    assert!(saw_internal && saw_crossing, "both branches were exercised");
}

/// The index must decide exactly what the collection walk it replaced
/// decided.
///
/// The reference below is the previous implementation verbatim: test
/// every declared boundary for a crossing, then scan every entry for
/// one containing both endpoints. Over an exhaustive matrix of
/// endpoints, boundary inventories, and held rights, the two must
/// agree on every verdict — a faster decision that is a different
/// decision is not an optimization.
///
/// The universe deliberately includes interior-zero paths (`3.0.7`,
/// `3.0.9`). The first version of this oracle drew only from tidy
/// `SubnetId::new` paths, and agreed with the reference everywhere
/// *because both were being asked about inputs that never triggered the
/// broken meet*. A differential test is only as good as its domain, and
/// the domain here has to be what the wire decoders accept.
#[test]
fn the_index_decides_exactly_what_the_scan_decided() {
    fn reference(
        set: &VerifiedGatewayContextSet,
        source: TopologySubnetId,
        target: TopologySubnetId,
        declared: &[TopologySubnetId],
    ) -> Result<(), ForwardDenial> {
        let mut crossed_any = false;
        for boundary in declared {
            if boundary.is_ancestor_or_self_of(source) == boundary.is_ancestor_or_self_of(target) {
                continue;
            }
            crossed_any = true;
            let satisfied = set
                .entries()
                .iter()
                .any(|e| &e.scope == boundary && e.rights.contains(SubnetRights::EXPORT));
            if !satisfied {
                return Err(ForwardDenial::ExportMissing);
            }
        }
        if crossed_any {
            return Ok(());
        }
        let routed = set.entries().iter().any(|e| {
            e.rights.contains(SubnetRights::ROUTE)
                && e.scope.is_ancestor_or_self_of(source)
                && e.scope.is_ancestor_or_self_of(target)
        });
        if routed {
            Ok(())
        } else {
            Err(ForwardDenial::RouteMissing)
        }
    }

    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    let universe: Vec<&[u8]> = vec![
        &[],
        &[3],
        &[3, 7],
        &[3, 7, 1],
        &[3, 7, 2],
        &[3, 8],
        &[4],
        &[4, 1],
        // Interior zeros: the shape the first version of this oracle
        // could not see, and the one that widened authority.
        &[3, 0, 7],
        &[3, 0, 9],
    ];
    // Every scope that can appear as a boundary or as a held right.
    let scopes: Vec<TopologySubnetId> = universe
        .iter()
        .map(|p| TopologySubnetId::new(p))
        .collect::<Vec<_>>();
    assert!(
        scopes.contains(&TopologySubnetId::from_raw(0x03_00_07_00)),
        "the differential domain must contain interior-zero paths",
    );

    // A spread of boundary inventories, including none and all.
    let inventories: Vec<Vec<TopologySubnetId>> = vec![
        vec![],
        vec![TopologySubnetId::new(&[3, 7])],
        vec![TopologySubnetId::new(&[3, 7, 1])],
        vec![
            TopologySubnetId::new(&[3, 7]),
            TopologySubnetId::new(&[3, 8]),
        ],
        vec![
            TopologySubnetId::new(&[3]),
            TopologySubnetId::new(&[3, 7, 1]),
        ],
        scopes.clone(),
    ];

    // A spread of held rights, including gaps that force denials.
    let rights_sets: Vec<SubnetRights> = vec![
        SubnetRights::ATTACH,
        SubnetRights::ROUTE,
        SubnetRights::EXPORT,
        SubnetRights::ROUTE.union(SubnetRights::EXPORT),
    ];

    let mut compared = 0usize;
    let mut agreed_ok = 0usize;
    for (holding, rights) in rights_sets.iter().enumerate() {
        // Hold `rights` at half the scopes and ATTACH at the rest, so
        // the two implementations must agree about partial coverage
        // rather than about a uniformly-permissive gateway.
        let entries: Vec<VerifiedGatewayContext> = scopes
            .iter()
            .enumerate()
            .map(|(i, &scope)| {
                let held = if (i + holding) % 2 == 0 {
                    *rights
                } else {
                    SubnetRights::ATTACH
                };
                raw_entry(&root, &local, scope, held)
            })
            .collect();
        let set = gateway_set(&root, entries);

        for declared in &inventories {
            let bset =
                SubnetBoundarySet::new(root.entity_id().clone(), 0, declared.iter().copied());
            for source in &universe {
                for target in &universe {
                    let x = peer_ctx(&root, &a, source, &[], SubnetRights::ATTACH);
                    let y = peer_ctx(&root, &b, target, &[], SubnetRights::ATTACH);
                    let actual = set.authorize_transition(&x, &y, &bset, 0, 0, now());
                    let expected = reference(
                        &set,
                        TopologySubnetId::new(source),
                        TopologySubnetId::new(target),
                        declared,
                    );
                    assert_eq!(
                        actual, expected,
                        "disagreement: {source:?} -> {target:?}, boundaries {declared:?}, \
                         rights {rights:?} (holding {holding})",
                    );
                    compared += 1;
                    if actual.is_ok() {
                        agreed_ok += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 1000, "matrix must be broad, compared {compared}");
    // Guard against a matrix that only ever denies: agreement on
    // "everything is refused" would prove nothing about the index.
    assert!(
        agreed_ok > 0 && agreed_ok < compared,
        "matrix must contain both authorizations and denials, got {agreed_ok}/{compared}",
    );
}

/// Kyra's S4A-index RED: an interior zero in a path must not
/// manufacture a boundary crossing, letting `EXPORT` stand in for
/// `ROUTE`.
///
/// `3.0.7` is an ordinary constructible path — `SubnetId::new(&[3, 0,
/// 7])`, and every wire decoder reaches the same value through
/// `from_raw` with no canonical rejection. When `common_ancestor`
/// stopped at the first zero it answered `3` for `3.0.7 ∧ 3.0.7`, so a
/// transition between two *identical* attachments looked like it
/// crossed a boundary declared at `3.0.7`. A gateway holding only
/// `EXPORT(3.0.7)` was then authorized for a transition that requires
/// `ROUTE` — authority widening produced by a path shape, with no
/// credential involved and nothing revoked.
///
/// The previous meet witness built its universe from paths without
/// interior zeros, which is exactly why it passed.
#[test]
fn interior_zero_paths_do_not_manufacture_a_crossing() {
    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);

    let interior_zero = TopologySubnetId::new(&[3, 0, 7]);
    assert_eq!(
        interior_zero.raw(),
        0x03_00_07_00,
        "the path under test really does carry an interior zero",
    );

    let bset = SubnetBoundarySet::new(root.entity_id().clone(), 0, [interior_zero]);
    // EXPORT at the boundary, and no ROUTE anywhere.
    let set = gateway_set(
        &root,
        vec![raw_entry(
            &root,
            &local,
            interior_zero,
            SubnetRights::EXPORT,
        )],
    );

    // Both peers attached at the SAME point. There is no such thing as
    // crossing a boundary to reach yourself.
    let x = peer_ctx(&root, &a, &[3, 0, 7], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 0, 7], &[3], SubnetRights::ATTACH);

    assert_eq!(
        set.authorize_transition(&x, &y, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::RouteMissing,
        "an internal transition requires ROUTE; EXPORT must never substitute for it",
    );

    // Granting ROUTE at that scope is what actually authorizes it.
    let routed = gateway_set(
        &root,
        vec![raw_entry(
            &root,
            &local,
            interior_zero,
            SubnetRights::EXPORT.union(SubnetRights::ROUTE),
        )],
    );
    routed
        .authorize_transition(&x, &y, &bset, 0, 0, now())
        .expect("ROUTE at the attachment authorizes the internal transition");

    // And a real crossing of that same boundary still needs EXPORT, so
    // the repair did not simply stop detecting the boundary.
    let outside = peer_ctx(&root, &b, &[3, 0, 9], &[3], SubnetRights::ATTACH);
    set.authorize_transition(&x, &outside, &bset, 0, 0, now())
        .expect("EXPORT authorizes a genuine crossing out of 3.0.7");
    assert_eq!(
        gateway_set(
            &root,
            vec![raw_entry(&root, &local, interior_zero, SubnetRights::ROUTE)],
        )
        .authorize_transition(&x, &outside, &bset, 0, 0, now())
        .unwrap_err(),
        ForwardDenial::ExportMissing,
        "a genuine crossing still demands EXPORT",
    );
}

/// Kyra's second S4A-index RED, structurally: removing authority must
/// never leave authority active.
///
/// The forwarding decision consults a private compiled index; the
/// entries, epochs and expiry it is derived from used to be public
/// fields. Assigning an empty `entries` left the index still granting
/// `ROUTE`, while the emptiness shortcut skipped the currency check —
/// so clearing a gateway's authority *kept* it authorized.
///
/// The repair is that no field is independently assignable, which makes
/// the original reproduction not compile. What remains testable is the
/// property that motivated it: the only way to change a published set
/// is to publish another one, and doing so with nothing in it
/// authorizes nothing.
#[test]
fn removing_authority_never_leaves_authority_active() {
    let root = kp(1);
    let local = kp(9);
    let a = kp(2);
    let b = kp(3);
    let bset = boundaries(&root, &[]);

    let x = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 7, 2], &[3], SubnetRights::ATTACH);

    let granted = gateway_set(
        &root,
        vec![raw_entry(
            &root,
            &local,
            TopologySubnetId::new(&[3]),
            SubnetRights::ROUTE,
        )],
    );
    granted
        .authorize_transition(&x, &y, &bset, 0, 0, now())
        .expect("ROUTE(3) authorizes the internal transition");
    assert_eq!(granted.entries().len(), 1);

    // Republishing with nothing revokes completely: the derived index
    // is rebuilt from the same input, so there is no stale half left
    // behind to keep answering.
    let cleared = gateway_set(&root, vec![]);
    assert!(cleared.entries().is_empty());
    assert_eq!(
        cleared
            .authorize_transition(&x, &y, &bset, 0, 0, now())
            .unwrap_err(),
        ForwardDenial::RouteMissing,
        "a set with no entries must grant nothing",
    );

    // `entries()` hands out a shared slice, never a mutable handle, so
    // a diagnostic reader cannot become a writer.
    let observed: &[VerifiedGatewayContext] = granted.entries();
    assert_eq!(observed.len(), 1);
    assert_eq!(granted.topology_epoch(), 0);
    assert_eq!(granted.subnet_auth_epoch(), 0);
    assert!(granted.earliest_expiry() > now());
}

/// The set folds epochs across its entries so the packet path can
/// check currency without walking them. That fold is only sound if the
/// entries agree, so a mixed set is refused at publication rather than
/// silently resolved.
#[test]
fn entries_from_different_epochs_cannot_be_published_as_one_set() {
    let root = kp(1);
    let local = kp(9);

    let mut later_topology = raw_entry(
        &root,
        &local,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ROUTE,
    );
    later_topology.topology_epoch = 1;
    assert_eq!(
        build_gateway_context_set(
            root.entity_id(),
            vec![
                raw_entry(
                    &root,
                    &local,
                    TopologySubnetId::new(&[3]),
                    SubnetRights::ROUTE
                ),
                later_topology,
            ],
        )
        .unwrap_err(),
        SubnetAuthError::MixedGatewayEpochs,
    );

    let mut later_auth = raw_entry(
        &root,
        &local,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ROUTE,
    );
    later_auth.subnet_auth_epoch = 1;
    assert_eq!(
        build_gateway_context_set(
            root.entity_id(),
            vec![
                raw_entry(
                    &root,
                    &local,
                    TopologySubnetId::new(&[3]),
                    SubnetRights::ROUTE
                ),
                later_auth,
            ],
        )
        .unwrap_err(),
        SubnetAuthError::MixedGatewayEpochs,
    );

    // The published fold reports the tightest expiry, which is what
    // the constant-time currency check compares against.
    let mut short = raw_entry(
        &root,
        &local,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ROUTE,
    );
    short.expires_at = now() + 60;
    let set = gateway_set(
        &root,
        vec![
            raw_entry(
                &root,
                &local,
                TopologySubnetId::new(&[3]),
                SubnetRights::ROUTE,
            ),
            short,
        ],
    );
    assert_eq!(set.earliest_expiry(), now() + 60);
    assert_eq!(set.topology_epoch(), 0);
    assert_eq!(set.subnet_auth_epoch(), 0);

    // And that fold denies once the shortest-lived credential lapses,
    // even though the other entry is still valid.
    let a = kp(2);
    let b = kp(3);
    let x = peer_ctx(&root, &a, &[3, 7, 1], &[3], SubnetRights::ATTACH);
    let y = peer_ctx(&root, &b, &[3, 7, 2], &[3], SubnetRights::ATTACH);
    let bset = boundaries(&root, &[]);
    set.authorize_transition(&x, &y, &bset, 0, 0, now())
        .expect("current while every entry is");
    assert_eq!(
        set.authorize_transition(&x, &y, &bset, 0, 0, now() + 61)
            .unwrap_err(),
        ForwardDenial::ContextNotCurrent,
        "the set stops authorizing when its shortest-lived entry does",
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
