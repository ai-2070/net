//! S2 of `docs/internal/plans/SUBNET_AUTH_PLAN.md` — wire format and
//! verification witnesses for the subnet credential family.
//!
//! Pins: exact wire sizes and field offsets; strict decode (unknown
//! version/rights, bit 3 = the retired `DELEGATE`, trailing bytes,
//! inverted windows); root anchoring with fail-closed empty roots;
//! the one-hop issuer envelope (scope containment, rights
//! attenuation, window nesting, no second hop, no leaf-as-issuer);
//! and cross-domain unusability against `PermissionToken` and the
//! org credential family.

#![cfg(feature = "net")]

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::identity::{EntityKeypair, PermissionToken, TokenScope};
use net::adapter::net::subnet::{
    auth::verify_credential_set, SubnetAuthError, SubnetAuthorityConfig, SubnetCredentialSet,
    SubnetFloorRegistry, SubnetGrant, SubnetIssuerGrant, SubnetRights, TopologySubnetId,
};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn kp(seed: u8) -> EntityKeypair {
    EntityKeypair::from_bytes([seed; 32])
}

fn config(authority: &EntityKeypair, roots: &[&EntityKeypair]) -> SubnetAuthorityConfig {
    SubnetAuthorityConfig {
        authority: authority.entity_id().clone(),
        roots: roots.iter().map(|r| r.entity_id().clone()).collect(),
        maximum_grant_lifetime_secs: 7 * DAY,
    }
}

/// Direct root-signed leaf: root keypair doubles as the authority
/// identity (the common single-root installation).
fn direct_leaf(
    root: &EntityKeypair,
    subject: &EntityKeypair,
    scope: TopologySubnetId,
    rights: SubnetRights,
) -> SubnetGrant {
    SubnetGrant::try_issue(
        root,
        root.entity_id().clone(),
        scope,
        0,
        subject.entity_id().clone(),
        rights,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue direct leaf")
}

// ---------------------------------------------------------------------------
// Wire layout
// ---------------------------------------------------------------------------

/// The fixed sizes are load-bearing (strict length checks + framing);
/// a change here is a wire break and must be deliberate.
#[test]
fn wire_sizes_are_pinned() {
    assert_eq!(SubnetGrant::SIGNED_PAYLOAD_SIZE, 134);
    assert_eq!(SubnetGrant::WIRE_SIZE, 198);
    assert_eq!(SubnetIssuerGrant::SIGNED_PAYLOAD_SIZE, 94);
    assert_eq!(SubnetIssuerGrant::WIRE_SIZE, 158);
    assert_eq!(
        net::adapter::net::subnet::SubnetRevocationFloor::SIGNED_PAYLOAD_SIZE,
        93
    );
    assert_eq!(
        net::adapter::net::subnet::SubnetRevocationFloor::WIRE_SIZE,
        157
    );
}

/// Field offsets pinned by hand-assembling a wire image and decoding
/// it: version 0, authority 1..33, scope 33..37 LE, epoch 37..41 LE,
/// issuer 41..73, subject 73..105, rights 105, generation 106..110,
/// not_before 110..118, not_after 118..126, nonce 126..134,
/// signature 134..198.
#[test]
fn grant_field_offsets_are_pinned() {
    let mut bytes = vec![0u8; SubnetGrant::WIRE_SIZE];
    bytes[0] = 1; // version
    bytes[1..33].fill(0xAA); // authority
    bytes[33..37].copy_from_slice(&0x0307_0000u32.to_le_bytes()); // scope 3.7
    bytes[37..41].copy_from_slice(&9u32.to_le_bytes()); // epoch
    bytes[41..73].fill(0xBB); // issuer
    bytes[73..105].fill(0xCC); // subject
    bytes[105] = SubnetRights::ATTACH.union(SubnetRights::ROUTE).bits();
    bytes[106..110].copy_from_slice(&7u32.to_le_bytes()); // generation
    bytes[110..118].copy_from_slice(&NOW.to_le_bytes()); // not_before
    bytes[118..126].copy_from_slice(&(NOW + DAY).to_le_bytes()); // not_after
    bytes[126..134].copy_from_slice(&0xDEAD_BEEFu64.to_le_bytes()); // nonce
    bytes[134..198].fill(0xEE); // signature

    let g = SubnetGrant::from_bytes(&bytes).expect("decode hand-assembled grant");
    assert_eq!(g.version, 1);
    assert_eq!(g.authority.as_bytes(), &[0xAA; 32]);
    assert_eq!(g.scope, TopologySubnetId::new(&[3, 7]));
    assert_eq!(g.topology_epoch, 9);
    assert_eq!(g.issuer.as_bytes(), &[0xBB; 32]);
    assert_eq!(g.subject.as_bytes(), &[0xCC; 32]);
    assert_eq!(g.rights, SubnetRights::ATTACH.union(SubnetRights::ROUTE));
    assert_eq!(g.generation, 7);
    assert_eq!(g.not_before, NOW);
    assert_eq!(g.not_after, NOW + DAY);
    assert_eq!(g.nonce, 0xDEAD_BEEF);
    assert_eq!(g.signature, [0xEE; 64]);
    // Round-trip is byte-identical.
    assert_eq!(g.to_bytes(), bytes);
}

#[test]
fn grant_round_trips() {
    let root = kp(1);
    let subject = kp(2);
    let g = direct_leaf(
        &root,
        &subject,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ATTACH,
    );
    let decoded = SubnetGrant::from_bytes(&g.to_bytes()).expect("round trip");
    assert_eq!(decoded, g);
    decoded
        .verify()
        .expect("signature verifies after round trip");
}

// ---------------------------------------------------------------------------
// Strict decode
// ---------------------------------------------------------------------------

#[test]
fn decode_rejects_wrong_length_version_rights_and_window() {
    let root = kp(1);
    let subject = kp(2);
    let g = direct_leaf(
        &root,
        &subject,
        TopologySubnetId::new(&[3]),
        SubnetRights::ATTACH,
    );
    let good = g.to_bytes();

    // Trailing byte.
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(
        SubnetGrant::from_bytes(&trailing).unwrap_err(),
        SubnetAuthError::InvalidFormat
    );
    // Truncated.
    assert_eq!(
        SubnetGrant::from_bytes(&good[..good.len() - 1]).unwrap_err(),
        SubnetAuthError::InvalidFormat
    );
    // Unknown version.
    let mut v2 = good.clone();
    v2[0] = 2;
    assert_eq!(
        SubnetGrant::from_bytes(&v2).unwrap_err(),
        SubnetAuthError::InvalidFormat
    );
    // Bit 3 — the retired DELEGATE — is an unknown-rights decode
    // error on a leaf grant.
    let mut delegate = good.clone();
    delegate[105] = 0b1000;
    assert_eq!(
        SubnetGrant::from_bytes(&delegate).unwrap_err(),
        SubnetAuthError::InvalidRights
    );
    // A high unknown bit likewise; an empty mask likewise.
    let mut high = good.clone();
    high[105] = 0b1000_0001;
    assert_eq!(
        SubnetGrant::from_bytes(&high).unwrap_err(),
        SubnetAuthError::InvalidRights
    );
    let mut empty = good.clone();
    empty[105] = 0;
    assert_eq!(
        SubnetGrant::from_bytes(&empty).unwrap_err(),
        SubnetAuthError::InvalidRights
    );
    // Inverted window.
    let mut inverted = good.clone();
    inverted[110..118].copy_from_slice(&(NOW + DAY).to_le_bytes());
    inverted[118..126].copy_from_slice(&NOW.to_le_bytes());
    assert_eq!(
        SubnetGrant::from_bytes(&inverted).unwrap_err(),
        SubnetAuthError::InvalidValidityWindow
    );
}

#[test]
fn rights_try_from_bits_is_strict() {
    assert!(SubnetRights::try_from_bits(0).is_err());
    assert!(
        SubnetRights::try_from_bits(0b1000).is_err(),
        "bit 3 (DELEGATE) rejected"
    );
    assert!(SubnetRights::try_from_bits(0b1_0000).is_err());
    for bits in [0b001u8, 0b010, 0b100, 0b011, 0b111] {
        assert_eq!(SubnetRights::try_from_bits(bits).unwrap().bits(), bits);
    }
}

#[test]
fn issue_rejects_zero_and_oversized_lifetime() {
    let root = kp(1);
    let subject = kp(2);
    let err = SubnetGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::GLOBAL,
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW,
        0,
    )
    .unwrap_err();
    assert_eq!(err, SubnetAuthError::InvalidValidityWindow);

    let err = SubnetGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::GLOBAL,
        0,
        subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW,
        31 * DAY,
    )
    .unwrap_err();
    assert_eq!(err, SubnetAuthError::LifetimeTooWide);
}

// ---------------------------------------------------------------------------
// Direct verification
// ---------------------------------------------------------------------------

#[test]
fn direct_root_grant_verifies() {
    let root = kp(1);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&root, &[&root]);
    let scope = TopologySubnetId::new(&[3, 7]);
    let set =
        SubnetCredentialSet::Direct(direct_leaf(&root, &subject, scope, SubnetRights::ATTACH));

    let verified = verify_credential_set(&set, subject.entity_id(), &cfg, 0, &floors, NOW, 60)
        .expect("verify");
    assert_eq!(verified.scope, scope);
    assert_eq!(verified.rights, SubnetRights::ATTACH);
    assert_eq!(&verified.subject, subject.entity_id());
    assert_eq!(verified.subnet_auth_epoch, 0);
    assert_eq!(verified.credential_set_hash, set.credential_set_hash());
}

#[test]
fn empty_roots_fail_closed() {
    let root = kp(1);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let mut cfg = config(&root, &[&root]);
    cfg.roots.clear();
    let set = SubnetCredentialSet::Direct(direct_leaf(
        &root,
        &subject,
        TopologySubnetId::GLOBAL,
        SubnetRights::ATTACH,
    ));
    assert_eq!(
        verify_credential_set(&set, subject.entity_id(), &cfg, 0, &floors, NOW, 60).unwrap_err(),
        SubnetAuthError::UnknownAuthority
    );
}

#[test]
fn non_root_signer_is_rejected() {
    let root = kp(1);
    let rogue = kp(3);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&root, &[&root]);
    // Signed by a key that is not a configured root, claiming the
    // right authority.
    let leaf = SubnetGrant::try_issue(
        &rogue,
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
    let set = SubnetCredentialSet::Direct(leaf);
    assert_eq!(
        verify_credential_set(&set, subject.entity_id(), &cfg, 0, &floors, NOW, 60).unwrap_err(),
        SubnetAuthError::IssuerNotAuthorized
    );
}

#[test]
fn wrong_subject_epoch_authority_and_tampered_signature_fail() {
    let root = kp(1);
    let subject = kp(2);
    let other = kp(4);
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&root, &[&root]);
    let leaf = direct_leaf(
        &root,
        &subject,
        TopologySubnetId::new(&[3]),
        SubnetRights::ATTACH,
    );

    // Wrong subject (full EntityId inequality).
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(leaf.clone()),
            other.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::WrongSubject
    );
    // Wrong topology epoch.
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(leaf.clone()),
            subject.entity_id(),
            &cfg,
            1,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::WrongTopologyEpoch
    );
    // Wrong authority: config anchored elsewhere.
    let cfg_other = config(&other, &[&root]);
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(leaf.clone()),
            subject.entity_id(),
            &cfg_other,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::WrongAuthority
    );
    // Tampered payload → signature failure.
    let mut tampered = leaf;
    tampered.generation += 1;
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::Direct(tampered),
            subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::InvalidSignature
    );
}

#[test]
fn expiry_and_authority_lifetime_ceiling_are_enforced() {
    let root = kp(1);
    let subject = kp(2);
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&root, &[&root]);
    let leaf = direct_leaf(
        &root,
        &subject,
        TopologySubnetId::GLOBAL,
        SubnetRights::ATTACH,
    );
    let set = SubnetCredentialSet::Direct(leaf);

    // Expired (past not_after + skew).
    assert_eq!(
        verify_credential_set(
            &set,
            subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW + 2 * DAY,
            60
        )
        .unwrap_err(),
        SubnetAuthError::Expired
    );
    // Not yet valid.
    assert_eq!(
        verify_credential_set(&set, subject.entity_id(), &cfg, 0, &floors, NOW - DAY, 60)
            .unwrap_err(),
        SubnetAuthError::NotYetValid
    );
    // Authority ceiling tighter than the wire ceiling: a 1-day grant
    // against a 1-hour authority maximum is lifetime_too_wide.
    let mut tight = config(&root, &[&root]);
    tight.maximum_grant_lifetime_secs = 3600;
    assert_eq!(
        verify_credential_set(&set, subject.entity_id(), &tight, 0, &floors, NOW, 60).unwrap_err(),
        SubnetAuthError::LifetimeTooWide
    );
}

// ---------------------------------------------------------------------------
// One-hop issuance
// ---------------------------------------------------------------------------

struct OneHopFixture {
    root: EntityKeypair,
    issuer: EntityKeypair,
    subject: EntityKeypair,
    issuer_grant: SubnetIssuerGrant,
}

fn one_hop_fixture() -> OneHopFixture {
    let root = kp(1);
    let issuer = kp(5);
    let subject = kp(2);
    let issuer_grant = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
        1,
        NOW - 120,
        2 * DAY,
    )
    .expect("issue issuer grant");
    OneHopFixture {
        root,
        issuer,
        subject,
        issuer_grant,
    }
}

fn one_hop_leaf(
    f: &OneHopFixture,
    scope: TopologySubnetId,
    rights: SubnetRights,
    not_before: u64,
    duration: u64,
) -> SubnetGrant {
    SubnetGrant::try_issue(
        &f.issuer,
        f.root.entity_id().clone(),
        scope,
        0,
        f.subject.entity_id().clone(),
        rights,
        1,
        not_before,
        duration,
    )
    .expect("issue one-hop leaf")
}

#[test]
fn one_hop_within_envelope_verifies() {
    let f = one_hop_fixture();
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&f.root, &[&f.root]);
    let leaf = one_hop_leaf(
        &f,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ATTACH,
        NOW - 60,
        DAY,
    );
    let set = SubnetCredentialSet::OneHop {
        issuer_grant: f.issuer_grant.clone(),
        leaf,
    };
    let verified = verify_credential_set(&set, f.subject.entity_id(), &cfg, 0, &floors, NOW, 60)
        .expect("verify");
    assert_eq!(verified.scope, TopologySubnetId::new(&[3, 7]));
    assert_eq!(verified.rights, SubnetRights::ATTACH);
}

#[test]
fn one_hop_envelope_violations_fail() {
    let f = one_hop_fixture();
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&f.root, &[&f.root]);

    // Upward scope: leaf at the authority root while the issuer is
    // scoped to [3].
    let upward = one_hop_leaf(
        &f,
        TopologySubnetId::GLOBAL,
        SubnetRights::ATTACH,
        NOW - 60,
        DAY,
    );
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: f.issuer_grant.clone(),
                leaf: upward
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::ScopeNotAncestor
    );
    // Sibling scope: [4] is outside [3].
    let sibling = one_hop_leaf(
        &f,
        TopologySubnetId::new(&[4]),
        SubnetRights::ATTACH,
        NOW - 60,
        DAY,
    );
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: f.issuer_grant.clone(),
                leaf: sibling
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::ScopeNotAncestor
    );
    // Widened rights: EXPORT is not in the issuer's maximum.
    let widened = one_hop_leaf(
        &f,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::EXPORT,
        NOW - 60,
        DAY,
    );
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: f.issuer_grant.clone(),
                leaf: widened
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::IssuerAttenuationBroadened
    );
    // Widened window: leaf outlives the issuer grant.
    let outlives = one_hop_leaf(
        &f,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ATTACH,
        NOW - 60,
        3 * DAY,
    );
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: f.issuer_grant.clone(),
                leaf: outlives
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::IssuerAttenuationBroadened
    );
}

#[test]
fn leaf_as_issuer_and_second_hop_fail() {
    let f = one_hop_fixture();
    let floors = SubnetFloorRegistry::new();
    let cfg = config(&f.root, &[&f.root]);

    // Leaf signed by a key OTHER than the empowered issuer (a leaf
    // subject trying to act as an issuer).
    let interloper = kp(6);
    let forged = SubnetGrant::try_issue(
        &interloper,
        f.root.entity_id().clone(),
        TopologySubnetId::new(&[3, 7]),
        0,
        f.subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: f.issuer_grant.clone(),
                leaf: forged
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::IssuerNotAuthorized
    );

    // Second provisioning hop: an issuer grant signed by the
    // delegated issuer (not a root) empowering a further issuer.
    let second_issuer = kp(7);
    let second_hop = SubnetIssuerGrant::try_issue(
        &f.issuer, // NOT a configured root
        f.root.entity_id().clone(),
        TopologySubnetId::new(&[3, 7]),
        0,
        second_issuer.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    let leaf = SubnetGrant::try_issue(
        &second_issuer,
        f.root.entity_id().clone(),
        TopologySubnetId::new(&[3, 7, 2]),
        0,
        f.subject.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        NOW - 60,
        DAY,
    )
    .expect("issue");
    assert_eq!(
        verify_credential_set(
            &SubnetCredentialSet::OneHop {
                issuer_grant: second_hop,
                leaf
            },
            f.subject.entity_id(),
            &cfg,
            0,
            &floors,
            NOW,
            60
        )
        .unwrap_err(),
        SubnetAuthError::IssuerNotAuthorized
    );
}

// ---------------------------------------------------------------------------
// Credential-set framing
// ---------------------------------------------------------------------------

#[test]
fn credential_set_framing_round_trips_and_rejects_malformed() {
    let f = one_hop_fixture();
    let leaf = one_hop_leaf(
        &f,
        TopologySubnetId::new(&[3, 7]),
        SubnetRights::ATTACH,
        NOW - 60,
        DAY,
    );
    let direct = SubnetCredentialSet::Direct(leaf.clone());
    let one_hop = SubnetCredentialSet::OneHop {
        issuer_grant: f.issuer_grant.clone(),
        leaf,
    };
    for set in [&direct, &one_hop] {
        let bytes = set.to_bytes();
        let decoded = SubnetCredentialSet::from_bytes(&bytes).expect("round trip");
        assert_eq!(&decoded, set);
        assert_eq!(decoded.credential_set_hash(), set.credential_set_hash());
    }
    // Distinct shapes hash differently.
    assert_ne!(direct.credential_set_hash(), one_hop.credential_set_hash());

    // Unknown tag / trailing byte / empty all fail.
    assert!(SubnetCredentialSet::from_bytes(&[]).is_err());
    let mut bad_tag = direct.to_bytes();
    bad_tag[0] = 9;
    assert!(SubnetCredentialSet::from_bytes(&bad_tag).is_err());
    let mut trailing = one_hop.to_bytes();
    trailing.push(0);
    assert!(SubnetCredentialSet::from_bytes(&trailing).is_err());
}

// ---------------------------------------------------------------------------
// Cross-domain unusability
// ---------------------------------------------------------------------------

/// Channel tokens and org certs cannot decode as any subnet
/// credential, and vice versa — typed envelopes with distinct
/// domains and lengths, not hash-space luck.
#[test]
fn cross_domain_bytes_do_not_decode() {
    let root = kp(1);
    let subject = kp(2);

    // A real channel token (169 bytes, no domain prefix).
    let token = PermissionToken::try_issue(
        &root,
        subject.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        0x1234,
        3600,
        1,
    )
    .expect("issue token");
    let token_bytes = token.to_bytes();
    assert!(SubnetGrant::from_bytes(&token_bytes).is_err());
    assert!(SubnetIssuerGrant::from_bytes(&token_bytes).is_err());
    assert!(net::adapter::net::subnet::SubnetRevocationFloor::from_bytes(&token_bytes).is_err());

    // A real org membership cert.
    let org = OrgKeypair::from_bytes([0x42; 32]);
    let cert = OrgMembershipCert::try_issue(&org, subject.entity_id().clone(), 1, 3600)
        .expect("issue org cert");
    let cert_bytes = cert.to_bytes();
    assert!(SubnetGrant::from_bytes(&cert_bytes).is_err());
    assert!(SubnetIssuerGrant::from_bytes(&cert_bytes).is_err());
    assert!(net::adapter::net::subnet::SubnetRevocationFloor::from_bytes(&cert_bytes).is_err());

    // And the reverse: subnet grant bytes decode as neither.
    let grant_bytes = direct_leaf(
        &root,
        &subject,
        TopologySubnetId::new(&[3]),
        SubnetRights::ATTACH,
    )
    .to_bytes();
    assert!(PermissionToken::from_bytes(&grant_bytes).is_err());
    assert!(OrgMembershipCert::from_bytes(&grant_bytes).is_err());
}
