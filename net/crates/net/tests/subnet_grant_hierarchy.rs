//! S2 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — hierarchy
//! semantics of authority-qualified scopes.
//!
//! `SubnetRef::contains` composes authority equality with the
//! canonical fixed-width path operation whose truth table (including
//! the path-`0` rows) is pinned in `subnet/id.rs`; this file pins the
//! composition and the issuer→leaf containment behavior, including
//! the "equal compact paths under two authorities are unrelated"
//! witness.

#![cfg(feature = "net")]

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::{
    auth::verify_credential_set, SubnetAuthError, SubnetAuthorityConfig, SubnetCredentialSet,
    SubnetFloorRegistry, SubnetGrant, SubnetIssuerGrant, SubnetRef, SubnetRights, TopologySubnetId,
};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn kp(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}

fn subnet_ref(kp: &EntityKeypair, levels: &[u8]) -> SubnetRef {
    SubnetRef {
        authority: kp.entity_id().clone(),
        path: TopologySubnetId::new(levels),
    }
}

/// The D1 truth table lifted onto authority-qualified refs, plus the
/// cross-authority row.
#[test]
fn subnet_ref_contains_matrix() {
    let x = kp(1);
    let y = kp(2);

    let a = subnet_ref(&x, &[3]);
    let a_b = subnet_ref(&x, &[3, 7]);
    let a_c = subnet_ref(&x, &[3, 8]);
    let a_b_c = subnet_ref(&x, &[3, 7, 2]);
    let zero = subnet_ref(&x, &[]);

    assert!(a.contains(&a));
    assert!(a.contains(&a_b));
    assert!(!a_b.contains(&a));
    assert!(!a_b.contains(&a_c));
    assert!(a_b.contains(&a_b_c));
    assert!(zero.contains(&zero));
    assert!(zero.contains(&a));
    assert!(zero.contains(&a_b_c));
    assert!(!a.contains(&zero));
    assert!(!a_b_c.contains(&zero));

    // Equal path bits under different authorities are unrelated —
    // in both directions, at every depth including the root.
    let y_a = subnet_ref(&y, &[3]);
    let y_zero = subnet_ref(&y, &[]);
    assert!(!a.contains(&y_a));
    assert!(!y_a.contains(&a));
    assert!(!zero.contains(&y_a));
    assert!(!zero.contains(&y_zero));
    assert!(!y_zero.contains(&zero));
}

/// A scope-0 issuer grant is an authority-root envelope: it may
/// provision leaves anywhere under that authority. A nonzero issuer
/// scope may not provision the authority root.
#[test]
fn issuer_scope_zero_covers_all_and_nonzero_cannot_reach_root() {
    let root = kp(1);
    let issuer = kp(5);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let cfg = SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };

    // Root-scope issuer grant (path 0).
    let root_scope_ig = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::GLOBAL,
        0,
        issuer.entity_id().clone(),
        SubnetRights::ALL,
        1,
        NOW - 120,
        2 * DAY,
    )
    .expect("issue");
    // Leaf deep in the hierarchy verifies under it.
    let deep_leaf = SubnetGrant::try_issue(
        &issuer,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3, 7, 2, 1]),
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    verify_credential_set(
        &SubnetCredentialSet::OneHop {
            issuer_grant: root_scope_ig.clone(),
            leaf: deep_leaf,
        },
        subject.entity_id(),
        &cfg,
        0,
        &floors,
        NOW,
        60,
    )
    .expect("scope-0 issuer grant covers every path under its authority");
    // And a scope-0 leaf (whole-installation grant) verifies too.
    let root_leaf = SubnetGrant::try_issue(
        &issuer,
        root.entity_id().clone(),
        TopologySubnetId::GLOBAL,
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    verify_credential_set(
        &SubnetCredentialSet::OneHop {
            issuer_grant: root_scope_ig,
            leaf: root_leaf.clone(),
        },
        subject.entity_id(),
        &cfg,
        0,
        &floors,
        NOW,
        60,
    )
    .expect("scope-0 leaf under scope-0 issuer grant");

    // A [3]-scoped issuer grant cannot provision the authority root.
    let narrow_ig = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ALL,
        1,
        NOW - 120,
        2 * DAY,
    )
    .expect("issue");
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: narrow_ig,
                leaf: root_leaf,
            },
            subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60,
        )
        .unwrap_err(),
        SubnetAuthError::ScopeNotAncestor
    );
}

/// Equal compact paths under Vehicle A's and Vehicle B's authorities
/// stay unrelated end to end: a grant minted under B's authority
/// fails against A's config even though the path bits match, and a
/// config for B with A's root in its root set still cannot verify a
/// grant claiming A's authority.
#[test]
fn equal_paths_under_different_authorities_are_unrelated() {
    let vehicle_a = kp(0xA0);
    let vehicle_b = kp(0xB0);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let path = TopologySubnetId::new(&[1, 2]);

    let cfg_a = SubnetAuthorityConfig {
        authority: vehicle_a.entity_id().clone(),
        roots: vec![vehicle_a.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };

    let b_grant = SubnetGrant::try_issue(
        &vehicle_b,
        vehicle_b.entity_id().clone(),
        path,
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(b_grant),
            subject.entity_id(),
            &cfg_a,
            0,
            &floors,
            NOW,
            60,
        )
        .unwrap_err(),
        SubnetAuthError::WrongAuthority
    );

    // A root configured for authority A cannot verify a grant naming
    // authority B — a config's roots mint only their own authority.
    let cfg_b_with_a_root = SubnetAuthorityConfig {
        authority: vehicle_b.entity_id().clone(),
        roots: vec![vehicle_a.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };
    let a_claimed = SubnetGrant::try_issue(
        &vehicle_a,
        vehicle_a.entity_id().clone(),
        path,
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(a_claimed),
            subject.entity_id(),
            &cfg_b_with_a_root,
            0,
            &floors,
            NOW,
            60,
        )
        .unwrap_err(),
        SubnetAuthError::WrongAuthority
    );
}

/// Parent-scoped grants dominate their subtree: a single grant at
/// `[3]` carries rights whose scope contains every descendant, and a
/// child-scoped grant contains neither its parent nor a sibling —
/// the asymmetry S3's session admission consumes.
#[test]
fn verified_scope_containment_is_asymmetric() {
    let root = kp(1);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let cfg = SubnetAuthorityConfig {
        authority: root.entity_id().clone(),
        roots: vec![root.entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };
    let parent = SubnetGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    let verified = verify_credential_set(
        &SubnetCredentialSet::Direct(parent),
        subject.entity_id(),
        &cfg,
        0,
        &floors,
        NOW,
        60,
    )
    .expect("verify");

    let scope = verified.scope;
    assert!(scope.is_ancestor_or_self_of(TopologySubnetId::new(&[3])));
    assert!(scope.is_ancestor_or_self_of(TopologySubnetId::new(&[3, 7])));
    assert!(scope.is_ancestor_or_self_of(TopologySubnetId::new(&[3, 7, 2, 1])));
    assert!(!scope.is_ancestor_or_self_of(TopologySubnetId::new(&[4])));
    assert!(!scope.is_ancestor_or_self_of(TopologySubnetId::GLOBAL));
}
