// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3-2 S2: the subnet relation in invites and bundles, and delegated leaf
//! issuance. The invite signs exactly one subnet offer (scope, epoch,
//! rights); the enrollment node issues a leaf for exactly the redeeming
//! device under its root-signed issuer grant; the device refuses anything
//! other than what it was offered; and the credential set is one the core
//! verifier accepts.
#![cfg(feature = "net")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::subnet::auth::verify_credential_set;
use net::adapter::net::subnet::{
    SubnetAuthorityConfig, SubnetCredentialSet, SubnetFloorRegistry, SubnetIssuerGrant, SubnetRef,
    SubnetRights, TopologySubnetId,
};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::bundle::{
    BundleError, MembershipBundle, MembershipIssuer, MembershipReceipt, MeshContact,
    SubnetLeafIssuer,
};
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteError, InviteSpec, MembershipInvite, RedemptionIntent,
    Relation, SubnetOffer,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::Refusal;
use net_sdk::enrollment::service::BundleIssuer;
use net_sdk::identity::Identity;

const PSK: [u8; 32] = [0x44; 32];
const DAY: u64 = 24 * 60 * 60;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn root() -> EntityKeypair {
    EntityKeypair::from_bytes([0xD1; 32])
}

fn offer(path: &[u8], rights: SubnetRights) -> SubnetOffer {
    SubnetOffer {
        scope: SubnetRef {
            authority: root().entity_id().clone(),
            path: TopologySubnetId::new(path),
        },
        topology_epoch: 0,
        rights,
    }
}

fn spec(relations: Vec<Relation>, subnet: Option<SubnetOffer>) -> InviteSpec {
    InviteSpec {
        trust_domain_name: "lab".into(),
        trust_domain: Psk::new(PSK).trust_domain(),
        endpoint: Some(EnrollmentEndpoint::parse("127.0.0.1:9").unwrap()),
        relay: None,
        enrollment_key: EnrollmentKey([5; 32]),
        subnet,
        relations,
        intended_subject: None,
        policy: InvitationPolicy::with_options(
            now(),
            Duration::from_secs(3600),
            ApprovalMode::Preauthorized,
        )
        .unwrap(),
    }
}

/// Root-signed issuer grant for `issuer` over `[3]` with ATTACH|ROUTE.
fn leaf_issuer() -> SubnetLeafIssuer {
    let root = root();
    let issuer = EntityKeypair::generate();
    let grant = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ATTACH.union(SubnetRights::ROUTE),
        1,
        now() - 120,
        7 * DAY,
    )
    .unwrap();
    SubnetLeafIssuer::new(grant, issuer, 1, DAY).unwrap()
}

fn contact() -> MeshContact {
    MeshContact {
        addr: Some("127.0.0.1:9".parse().unwrap()),
        noise_pubkey: [1; 32],
        node_id: 7,
        relay: None,
    }
}

#[test]
fn the_subnet_offer_is_signed_and_must_match_its_relation() {
    let operator = Identity::generate();
    let o = offer(&[3, 7], SubnetRights::ATTACH);
    let invite = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh, Relation::Subnet], Some(o.clone())),
    )
    .unwrap();
    let back = MembershipInvite::decode(&invite.encode()).unwrap();
    assert_eq!(back.subnet(), Some(&o));
    assert_eq!(back.relations(), &[Relation::Mesh, Relation::Subnet]);

    // The relation and the offer come together or not at all.
    assert!(matches!(
        MembershipInvite::sign(
            &operator,
            spec(vec![Relation::Mesh, Relation::Subnet], None)
        ),
        Err(InviteError::Relations(_))
    ));
    assert!(matches!(
        MembershipInvite::sign(&operator, spec(vec![Relation::Mesh], Some(o))),
        Err(InviteError::Relations(_))
    ));

    // Widening the offered rights breaks the signature.
    let mut bytes = invite.to_bytes().to_vec();
    let rights_at = bytes
        .windows(4)
        .position(|w| w == TopologySubnetId::new(&[3, 7]).raw().to_le_bytes())
        .unwrap()
        + 4
        + 4;
    bytes[rights_at] = SubnetRights::ALL.bits();
    assert_eq!(
        MembershipInvite::from_bytes(&bytes),
        Err(InviteError::BadSignature)
    );
}

#[test]
fn a_subnet_invite_delivers_a_delegated_credential_for_exactly_this_device() {
    let operator = Identity::generate();
    let invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Subnet],
            Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    let device = Identity::generate();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    let issuer = MembershipIssuer::new(operator.clone(), Psk::new(PSK), contact())
        .with_subnet_issuer(leaf_issuer());
    let bytes = issuer.issue(&invite, &intent).unwrap();
    let bundle = MembershipBundle::from_bytes(&bytes).unwrap();
    bundle.verify_for(&invite, &intent).unwrap();

    let set = bundle.subnet_credentials().expect("credentials delivered");
    assert!(matches!(set, SubnetCredentialSet::OneHop { .. }));
    assert_eq!(set.leaf().subject, *device.entity_id());
    assert_eq!(set.leaf().rights, SubnetRights::ATTACH);
    // A verifier anchored on the root accepts the chain for this device.
    let config = SubnetAuthorityConfig {
        authority: root().entity_id().clone(),
        roots: vec![root().entity_id().clone()],
        maximum_grant_lifetime_secs: 7 * DAY,
    };
    verify_credential_set(
        &set,
        device.entity_id(),
        &config,
        0,
        &SubnetFloorRegistry::new(),
        now(),
        0,
    )
    .expect("the delegated chain verifies");

    // Without a leaf issuer the node refuses rather than deliver half a bundle.
    let plain = MembershipIssuer::new(operator, Psk::new(PSK), contact());
    assert_eq!(plain.issue(&invite, &intent), Err(Refusal::Unavailable));
}

#[test]
fn the_device_refuses_subnet_credentials_it_was_not_offered() {
    let operator = Identity::generate();
    let subnet_invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Subnet],
            Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    let mesh_invite = MembershipInvite::sign(&operator, spec(vec![Relation::Mesh], None)).unwrap();
    let device = Identity::generate();
    let intent = RedemptionIntent::for_invite(&subnet_invite, device.entity_id().clone()).unwrap();
    let mesh_intent =
        RedemptionIntent::for_invite(&mesh_invite, device.entity_id().clone()).unwrap();
    let receipt = MembershipReceipt::sign(&operator, &subnet_invite, &intent, now());

    // Credentials for another scope, another subject or wider rights.
    let other = |path: &[u8], subject: &Identity, rights: SubnetRights| {
        let other_invite = MembershipInvite::sign(
            &operator,
            spec(
                vec![Relation::Mesh, Relation::Subnet],
                Some(offer(path, rights)),
            ),
        )
        .unwrap();
        let other_intent =
            RedemptionIntent::for_invite(&other_invite, subject.entity_id().clone()).unwrap();
        let bytes = MembershipIssuer::new(operator.clone(), Psk::new(PSK), contact())
            .with_subnet_issuer(leaf_issuer())
            .issue(&other_invite, &other_intent)
            .unwrap();
        MembershipBundle::from_bytes(&bytes)
            .unwrap()
            .subnet_credentials()
            .unwrap()
    };
    for set in [
        other(&[3, 8], &device, SubnetRights::ATTACH),
        other(&[3, 7], &Identity::generate(), SubnetRights::ATTACH),
        other(
            &[3, 7],
            &device,
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
        ),
    ] {
        let bundle = MembershipBundle::new(receipt.clone(), Psk::new(PSK), contact())
            .with_subnet_credentials(&set);
        assert_eq!(
            bundle.verify_for(&subnet_invite, &intent),
            Err(BundleError::Mismatch("subnet credentials"))
        );
    }

    // Missing for a subnet invite; present for a mesh-only one.
    let bare = MembershipBundle::new(receipt, Psk::new(PSK), contact());
    assert_eq!(
        bare.verify_for(&subnet_invite, &intent),
        Err(BundleError::Mismatch("subnet credentials"))
    );
    let mesh_receipt = MembershipReceipt::sign(&operator, &mesh_invite, &mesh_intent, now());
    let smuggled = MembershipBundle::new(mesh_receipt, Psk::new(PSK), contact())
        .with_subnet_credentials(&other(&[3, 7], &device, SubnetRights::ATTACH));
    assert_eq!(
        smuggled.verify_for(&mesh_invite, &mesh_intent),
        Err(BundleError::Mismatch("subnet credentials"))
    );
}

#[test]
fn an_issuer_cannot_exceed_its_grant() {
    let issuer = leaf_issuer();
    assert!(issuer.covers(&offer(&[3, 7], SubnetRights::ROUTE)));
    assert!(
        !issuer.covers(&offer(&[4], SubnetRights::ATTACH)),
        "outside the subtree"
    );
    assert!(
        !issuer.covers(&offer(&[3], SubnetRights::EXPORT)),
        "beyond the ceiling"
    );

    let operator = Identity::generate();
    let invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Subnet],
            Some(offer(&[4, 1], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    let intent =
        RedemptionIntent::for_invite(&invite, Identity::generate().entity_id().clone()).unwrap();
    assert_eq!(
        MembershipIssuer::new(operator, Psk::new(PSK), contact())
            .with_subnet_issuer(issuer.clone())
            .issue(&invite, &intent),
        Err(Refusal::Unavailable)
    );

    // The key must be the issuer the grant names.
    assert!(
        SubnetLeafIssuer::new(issuer.grant().clone(), EntityKeypair::generate(), 1, DAY).is_err()
    );
}

/// Renewal (V3-2 task 5): the issuing node re-issues a fresh leaf for
/// exactly the original offer, only to the device its ledger issued that
/// invite to, and only for a fresh request that device signed.
#[test]
fn only_the_enrolled_device_renews_its_subnet_leaf() {
    use net_sdk::enrollment::renew::{answer_renewal, SubnetRenewRequest};
    use net_sdk::enrollment::service::SharedLedger;
    use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits};

    let operator = Identity::generate();
    let tmp = tempfile::tempdir().unwrap();
    let ledger: SharedLedger = std::sync::Arc::new(parking_lot::Mutex::new(
        EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            operator.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap(),
    ));
    let sign = |relations, subnet| {
        let invite = MembershipInvite::sign(&operator, spec(relations, subnet)).unwrap();
        ledger.lock().offer(invite.offer_spec(), now()).unwrap();
        invite
    };
    let invite = sign(
        vec![Relation::Mesh, Relation::Subnet],
        Some(offer(&[3, 7], SubnetRights::ATTACH)),
    );
    let device = Identity::generate();
    let issuer = leaf_issuer();

    // Not issued yet: nothing to renew.
    let early = SubnetRenewRequest::sign(&device, &invite, now()).unwrap();
    assert_eq!(
        answer_renewal(&early.to_bytes(), &ledger, &issuer, now()),
        Err(Refusal::Invalid)
    );

    // Issue it to `device` through the ledger, as redemption would.
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    {
        let mut l = ledger.lock();
        let claimant = intent.claimant();
        l.claim(&invite.invitation_id(), &claimant, now()).unwrap();
        l.issue(&invite.invitation_id(), &claimant, b"bundle", now())
            .unwrap();
    }

    let fresh = SubnetRenewRequest::sign(&device, &invite, now()).unwrap();
    let set = answer_renewal(&fresh.to_bytes(), &ledger, &issuer, now()).expect("renewed");
    assert_eq!(set.leaf().subject, *device.entity_id());
    assert_eq!(set.leaf().scope, TopologySubnetId::new(&[3, 7]));
    assert_eq!(set.leaf().rights, SubnetRights::ATTACH);

    // Another device cannot renew someone else's enrollment.
    let other = SubnetRenewRequest::sign(&Identity::generate(), &invite, now()).unwrap();
    assert_eq!(
        answer_renewal(&other.to_bytes(), &ledger, &issuer, now()),
        Err(Refusal::Conflict)
    );
    // Stale requests and tampered signatures are refused.
    let stale = SubnetRenewRequest::sign(&device, &invite, now() - 10_000).unwrap();
    assert_eq!(
        answer_renewal(&stale.to_bytes(), &ledger, &issuer, now()),
        Err(Refusal::Expired)
    );
    let mut forged = fresh.to_bytes();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    assert_eq!(
        answer_renewal(&forged, &ledger, &issuer, now()),
        Err(Refusal::Invalid)
    );
    // A mesh-only invite, or one this ledger never recorded, renews nothing.
    let mesh_only = sign(vec![Relation::Mesh], None);
    let req = SubnetRenewRequest::sign(&device, &mesh_only, now()).unwrap();
    assert_eq!(
        answer_renewal(&req.to_bytes(), &ledger, &issuer, now()),
        Err(Refusal::Invalid)
    );
    let foreign = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Subnet],
            Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    let req = SubnetRenewRequest::sign(&device, &foreign, now()).unwrap();
    assert_eq!(
        answer_renewal(&req.to_bytes(), &ledger, &issuer, now()),
        Err(Refusal::Invalid)
    );
}
