//! S2 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — subtree
//! revocation floors.
//!
//! Floors are root-signed, scoped to an authority-qualified subtree,
//! and monotonic per `(scope, topology_epoch)` by revision. A child
//! floor invalidates child-scoped credentials without touching the
//! structurally dominant parent-scoped grant; a parent floor covers
//! the subtree. Accepting a state-changing floor advances the
//! per-authority auth epoch that compiled contexts compare against.

#![cfg(feature = "net")]

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    auth::verify_credential_set, SubnetAuthError, SubnetAuthorityConfig, SubnetCredentialSet,
    SubnetFloorRegistry, SubnetGrant, SubnetRef, SubnetRevocationFloor, SubnetRights,
    TopologySubnetId,
};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn kp(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}

fn config(root: &EntityKeypair) -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

fn floor(
    root: &EntityKeypair,
    levels: &[u8],
    minimum_generation: u32,
    revision: u64,
) -> SubnetRevocationFloor {
    SubnetRevocationFloor::try_issue(
        root,
        SubnetRef {
            authority: root.entity_id().clone(),
            path: TopologySubnetId::new(levels),
        },
        0,
        minimum_generation,
        revision,
        NOW,
    )
    .expect("issue floor")
}

fn grant(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    levels: &[u8],
    generation: u32,
) -> SubnetCredentialSet {
    SubnetCredentialSet::Direct(
        SubnetGrant::try_issue(
            root,
            root.entity_id().clone(),
            TopologySubnetId::new(levels),
            0,
            subject.entity_id().clone(),
            SubnetRights::ATTACH,
            generation,
            NOW - 60,
            DAY,
        )
        .expect("issue grant"),
    )
}

#[test]
fn floors_apply_monotonically_and_survive_replay_reorder() {
    let root = kp(1);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();
    let scope = TopologySubnetId::new(&[3, 7]);

    // First floor applies and bumps the epoch.
    assert!(reg
        .apply(&floor(&root, &[3, 7], 5, 1), &cfg)
        .expect("apply"));
    assert_eq!(reg.max_floor(root.entity_id(), 0, scope), 5);
    assert_eq!(reg.auth_epoch(root.entity_id()), 1);

    // Exact replay is a no-op and does not advance the epoch.
    assert!(!reg
        .apply(&floor(&root, &[3, 7], 5, 1), &cfg)
        .expect("apply"));
    assert_eq!(reg.auth_epoch(root.entity_id()), 1);

    // A newer revision raising the floor applies.
    assert!(reg
        .apply(&floor(&root, &[3, 7], 9, 3), &cfg)
        .expect("apply"));
    assert_eq!(reg.max_floor(root.entity_id(), 0, scope), 9);
    assert_eq!(reg.auth_epoch(root.entity_id()), 2);

    // A delayed OLDER revision (reorder) can never roll back.
    assert!(!reg
        .apply(&floor(&root, &[3, 7], 7, 2), &cfg)
        .expect("apply"));
    assert_eq!(reg.max_floor(root.entity_id(), 0, scope), 9);
    assert_eq!(reg.auth_epoch(root.entity_id()), 2);
}

#[test]
fn child_floor_revokes_child_scope_but_not_parent_grant() {
    let root = kp(1);
    let subject = kp(2);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();

    // Floor at vehicle/perception ([3, 7]) requiring generation >= 5.
    reg.apply(&floor(&root, &[3, 7], 5, 1), &cfg)
        .expect("apply");

    // A perception-scoped grant at generation 1 is revoked…
    assert_eq!(
        verify_credential_set(
            &grant(&root, &subject, &[3, 7], 1),
            subject.entity_id(),
            &cfg,
            0,
            &reg,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::Revoked
    );
    // …and so is a deeper camera-domain grant ([3, 7, 2]).
    assert_eq!(
        verify_credential_set(
            &grant(&root, &subject, &[3, 7, 2], 1),
            subject.entity_id(),
            &cfg,
            0,
            &reg,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::Revoked
    );
    // A refreshed perception grant at generation 5 verifies.
    verify_credential_set(
        &grant(&root, &subject, &[3, 7], 5),
        subject.entity_id(),
        &cfg,
        0,
        &reg,
        NOW,
        60,
    )
    .expect("generation at the floor verifies");
    // The structurally dominant vehicle-root grant ([3]) at
    // generation 1 is untouched by the child floor.
    verify_credential_set(
        &grant(&root, &subject, &[3], 1),
        subject.entity_id(),
        &cfg,
        0,
        &reg,
        NOW,
        60,
    )
    .expect("parent-scoped grant survives a child floor");
    // And an unrelated chassis grant ([3, 8]) is untouched.
    verify_credential_set(
        &grant(&root, &subject, &[3, 8], 1),
        subject.entity_id(),
        &cfg,
        0,
        &reg,
        NOW,
        60,
    )
    .expect("sibling-scoped grant survives");
}

#[test]
fn parent_floor_covers_the_subtree() {
    let root = kp(1);
    let subject = kp(2);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();

    // Floor at the vehicle root ([3]).
    reg.apply(&floor(&root, &[3], 4, 1), &cfg).expect("apply");

    for levels in [&[3u8][..], &[3, 7], &[3, 7, 2], &[3, 8]] {
        assert_eq!(
            verify_credential_set(
                &grant(&root, &subject, levels, 3),
                subject.entity_id(),
                &cfg,
                0,
                &reg,
                NOW,
                60
            )
            .unwrap_err(),
            SubnetAuthError::Revoked,
            "scope {levels:?} must be covered by the [3] floor",
        );
    }
    // An authority-root-scoped grant (path 0) structurally dominates
    // the [3] floor and survives.
    verify_credential_set(
        &grant(&root, &subject, &[], 3),
        subject.entity_id(),
        &cfg,
        0,
        &reg,
        NOW,
        60,
    )
    .expect("authority-root grant dominates a [3] floor");
    // An authority-root floor covers even that.
    reg.apply(&floor(&root, &[], 4, 1), &cfg).expect("apply");
    assert_eq!(
        verify_credential_set(
            &grant(&root, &subject, &[], 3),
            subject.entity_id(),
            &cfg,
            0,
            &reg,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::Revoked
    );
}

#[test]
fn floor_epochs_are_independent() {
    let root = kp(1);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();

    // A floor minted for topology epoch 0 does not apply to epoch 1
    // lookups: reparenting reinterprets paths, so floors do not
    // carry across.
    reg.apply(&floor(&root, &[3], 5, 1), &cfg).expect("apply");
    assert_eq!(
        reg.max_floor(root.entity_id(), 0, TopologySubnetId::new(&[3, 7])),
        5
    );
    assert_eq!(
        reg.max_floor(root.entity_id(), 1, TopologySubnetId::new(&[3, 7])),
        0
    );
}

#[test]
fn unauthorized_or_forged_floors_change_no_state() {
    let root = kp(1);
    let rogue = kp(3);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();

    // Signed by a non-root.
    let rogue_floor = SubnetRevocationFloor::try_issue(
        &rogue,
        SubnetRef {
            authority: root.entity_id().clone(),
            path: TopologySubnetId::new(&[3]),
        },
        0,
        99,
        1,
        NOW,
    )
    .expect("issue");
    assert_eq!(
        reg.apply(&rogue_floor, &cfg).unwrap_err(),
        SubnetAuthError::IssuerNotAuthorized
    );

    // Wrong authority.
    let other = kp(4);
    let wrong_authority = SubnetRevocationFloor::try_issue(
        &root,
        SubnetRef {
            authority: other.entity_id().clone(),
            path: TopologySubnetId::new(&[3]),
        },
        0,
        99,
        1,
        NOW,
    )
    .expect("issue");
    assert_eq!(
        reg.apply(&wrong_authority, &cfg).unwrap_err(),
        SubnetAuthError::WrongAuthority
    );

    // Tampered payload.
    let mut tampered = floor(&root, &[3], 5, 1);
    tampered.minimum_generation = 99;
    assert_eq!(
        reg.apply(&tampered, &cfg).unwrap_err(),
        SubnetAuthError::InvalidSignature
    );

    // Empty roots fail closed.
    let mut no_roots = config(&root);
    no_roots.roots.clear();
    assert_eq!(
        reg.apply(&floor(&root, &[3], 5, 1), &no_roots).unwrap_err(),
        SubnetAuthError::UnknownAuthority
    );

    // Nothing above changed floor state or the epoch.
    assert_eq!(
        reg.max_floor(root.entity_id(), 0, TopologySubnetId::new(&[3])),
        0
    );
    assert_eq!(reg.auth_epoch(root.entity_id()), 0);
}

/// The auth epoch is per authority while floors are keyed per
/// `(authority, topology_epoch, path)`, so every epoch advance
/// invalidates every compiled context under the authority. A floor
/// with `minimum_generation` 0 revokes nothing by definition, so
/// accepting one — including the first for a never-seen key — must
/// not charge that authority-wide cost. It is still stored: its
/// revision anchors the stream's monotonicity.
#[test]
fn a_floor_that_revokes_nothing_is_stored_without_costing_the_epoch() {
    let root = kp(1);
    let cfg = config(&root);
    let reg = SubnetFloorRegistry::new();
    let scope = TopologySubnetId::new(&[3, 7]);

    // A placeholder floor on a fresh key: stored, epoch untouched.
    assert!(!reg
        .apply(&floor(&root, &[3, 7], 0, 1), &cfg)
        .expect("a zero floor is accepted"));
    assert_eq!(reg.auth_epoch(root.entity_id()), 0);
    assert_eq!(reg.max_floor(root.entity_id(), 0, scope), 0);

    // Replay stays inert, and so does a provisioning run laying one
    // placeholder per scope — fresh keys, nothing enforceable.
    assert!(!reg
        .apply(&floor(&root, &[3, 7], 0, 1), &cfg)
        .expect("replay"));
    for levels in [&[3u8][..], &[3, 8], &[3, 7, 2]] {
        assert!(!reg.apply(&floor(&root, levels, 0, 1), &cfg).expect("apply"));
    }
    assert_eq!(reg.auth_epoch(root.entity_id()), 0);

    // The stored revision still anchors monotonicity: a materially
    // restrictive floor must arrive with a newer revision, and only
    // that one advances the epoch.
    assert!(!reg
        .apply(&floor(&root, &[3, 7], 5, 1), &cfg)
        .expect("stale revision refused"));
    assert_eq!(reg.auth_epoch(root.entity_id()), 0);
    assert!(reg
        .apply(&floor(&root, &[3, 7], 5, 2), &cfg)
        .expect("apply"));
    assert_eq!(reg.max_floor(root.entity_id(), 0, scope), 5);
    assert_eq!(reg.auth_epoch(root.entity_id()), 1);
}

#[test]
fn floor_wire_round_trips() {
    let root = kp(1);
    let f = floor(&root, &[3, 7], 5, 42);
    let decoded = SubnetRevocationFloor::from_bytes(&f.to_bytes()).expect("round trip");
    assert_eq!(decoded, f);
    decoded.verify().expect("signature verifies");
}
