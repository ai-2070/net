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
        org: None,
        channel: None,
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

/// V3-2 task 3: a standalone subnet link (subnet relation only) is redeemed
/// over the device's existing session. Credentials go only to the entity the
/// delivering session proved, for exactly the offer, through the ledger's
/// claim, approval and issue; a redemption repeated by the same device gets
/// fresh credentials, another device gets nothing.
#[test]
fn a_standalone_subnet_link_issues_only_to_the_proven_session_entity() {
    use net_sdk::enrollment::service::SharedLedger;
    use net_sdk::enrollment::standalone::{
        answer_subnet_redeem, is_standalone_subnet, SubnetRedeemReply, SubnetRedeemRequest,
    };
    use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits};

    const NODE: u64 = 0x0A11_CE00;
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
    let sign = |relations, subnet, approval| {
        let mut s = spec(relations, subnet);
        s.policy =
            InvitationPolicy::with_options(now(), Duration::from_secs(3600), approval).unwrap();
        let invite = MembershipInvite::sign(&operator, s).unwrap();
        let offer_id = ledger.lock().offer(invite.offer_spec(), now()).unwrap();
        (invite, offer_id)
    };
    let issuer = leaf_issuer();
    let device = Identity::generate();
    let (link, _) = sign(
        vec![Relation::Subnet],
        Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ApprovalMode::Preauthorized,
    );
    assert!(is_standalone_subnet(&link));
    let answer = |req: &SubnetRedeemRequest, proven: Option<&net_sdk::identity::EntityId>| {
        answer_subnet_redeem(&req.to_bytes(), proven, NODE, &ledger, &issuer, now())
    };
    let fresh = || SubnetRedeemRequest::sign(&device, &link, NODE, now()).unwrap();

    // The delivering session must have proven this very device.
    let other = Identity::generate();
    assert_eq!(answer(&fresh(), None), Err(Refusal::Unavailable));
    assert_eq!(
        answer(&fresh(), Some(other.entity_id())),
        Err(Refusal::Conflict)
    );
    // Bound to its destination node, fresh, and signed by the device.
    let elsewhere = SubnetRedeemRequest::sign(&device, &link, NODE + 1, now()).unwrap();
    assert_eq!(
        answer(&elsewhere, Some(device.entity_id())),
        Err(Refusal::Invalid)
    );
    let stale = SubnetRedeemRequest::sign(&device, &link, NODE, now() - 10_000).unwrap();
    assert_eq!(
        answer(&stale, Some(device.entity_id())),
        Err(Refusal::Expired)
    );
    let mut forged = fresh().to_bytes();
    let last = forged.len() - 1;
    forged[last] ^= 1;
    assert_eq!(
        answer_subnet_redeem(
            &forged,
            Some(device.entity_id()),
            NODE,
            &ledger,
            &issuer,
            now()
        ),
        Err(Refusal::Invalid)
    );

    // Issued: exactly the offer, for this device.
    let set = match answer(&fresh(), Some(device.entity_id())).unwrap() {
        SubnetRedeemReply::Issued(set) => set,
        other => panic!("expected issued, got {other:?}"),
    };
    assert_eq!(set.leaf().subject, *device.entity_id());
    assert_eq!(set.leaf().scope, TopologySubnetId::new(&[3, 7]));
    assert_eq!(set.leaf().rights, SubnetRights::ATTACH);
    // Asked again (a lost reply): the same device gets fresh credentials.
    assert!(matches!(
        answer(&fresh(), Some(device.entity_id())),
        Ok(SubnetRedeemReply::Issued(_))
    ));
    // Another device, even over its own proven session, gets nothing.
    let theirs = SubnetRedeemRequest::sign(&other, &link, NODE, now()).unwrap();
    assert_eq!(
        answer(&theirs, Some(other.entity_id())),
        Err(Refusal::Conflict)
    );

    // A link that also carries the mesh relation is not standalone: it
    // belongs to the enrollment endpoint (it delivers the PSK).
    let (full, _) = sign(
        vec![Relation::Mesh, Relation::Subnet],
        Some(offer(&[3, 8], SubnetRights::ATTACH)),
        ApprovalMode::Preauthorized,
    );
    let req = SubnetRedeemRequest::sign(&device, &full, NODE, now()).unwrap();
    assert_eq!(
        answer(&req, Some(device.entity_id())),
        Err(Refusal::Invalid)
    );

    // Approval-gated: pending until the operator approves this claimant.
    let (gated, gated_offer) = sign(
        vec![Relation::Subnet],
        Some(offer(&[3, 9], SubnetRights::ATTACH)),
        ApprovalMode::RequireApproval,
    );
    let req = || SubnetRedeemRequest::sign(&device, &gated, NODE, now()).unwrap();
    assert_eq!(
        answer(&req(), Some(device.entity_id())),
        Ok(SubnetRedeemReply::PendingApproval)
    );
    let claimant = ledger.lock().pending_claim(&gated_offer).unwrap().unwrap();
    ledger
        .lock()
        .approve(&gated_offer, &claimant, now())
        .unwrap();
    assert!(matches!(
        answer(&req(), Some(device.entity_id())),
        Ok(SubnetRedeemReply::Issued(_))
    ));
}

/// The device's per-link membership store: persists only credentials that
/// are exactly the signed offer for this device, survives reopen, and once
/// left holds no credentials and installs none (a late renewal included).
#[test]
fn a_standalone_membership_installs_only_its_offer_and_leave_fences_it() {
    use net_sdk::enrollment::device::DeviceJoinError;
    use net_sdk::enrollment::standalone::SubnetMembership;

    let operator = Identity::generate();
    let link = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Subnet],
            Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    let device = Identity::generate();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("subnets")).unwrap();
    let dir = tmp.path().join("subnets").join("one");
    let mut membership =
        SubnetMembership::begin(&dir, &link, device.entity_id().clone(), 42).unwrap();
    assert!(membership.credentials().is_none());

    // A mesh+subnet invite is not a standalone link.
    let full = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Subnet],
            Some(offer(&[3, 7], SubnetRights::ATTACH)),
        ),
    )
    .unwrap();
    assert!(SubnetMembership::begin(
        &tmp.path().join("subnets").join("two"),
        &full,
        device.entity_id().clone(),
        42
    )
    .is_err());

    // Credentials minted by the real issuer for a given offer and subject.
    let issuer = leaf_issuer();
    let issue = |path: &[u8], rights, subject: &Identity| {
        let invite = MembershipInvite::sign(
            &operator,
            spec(
                vec![Relation::Mesh, Relation::Subnet],
                Some(offer(path, rights)),
            ),
        )
        .unwrap();
        let intent = RedemptionIntent::for_invite(&invite, subject.entity_id().clone()).unwrap();
        let bundle = MembershipIssuer::new(operator.clone(), Psk::new(PSK), contact())
            .with_subnet_issuer(issuer.clone())
            .issue(&invite, &intent)
            .unwrap();
        MembershipBundle::from_bytes(&bundle)
            .unwrap()
            .subnet_credentials()
            .unwrap()
    };
    // Wrong scope, wrong rights, wrong subject: refused, nothing installed.
    for wrong in [
        issue(&[3, 8], SubnetRights::ATTACH, &device),
        issue(
            &[3, 7],
            SubnetRights::ATTACH.union(SubnetRights::ROUTE),
            &device,
        ),
        issue(&[3, 7], SubnetRights::ATTACH, &Identity::generate()),
    ] {
        assert!(membership.install(&wrong).is_err());
        assert!(membership.credentials().is_none());
    }
    let right = issue(&[3, 7], SubnetRights::ATTACH, &device);
    membership.install(&right).unwrap();
    drop(membership);

    let mut membership = SubnetMembership::open(&dir).unwrap();
    assert_eq!(membership.credentials(), Some(&right));
    assert_eq!(membership.issuer_node(), 42);
    assert!(membership.leave(1_000).unwrap());
    assert!(!membership.leave(2_000).unwrap(), "leave is idempotent");
    assert!(membership.credentials().is_none());
    assert!(matches!(
        membership.install(&right),
        Err(DeviceJoinError::Left { at: 1_000 })
    ));
    drop(membership);
    let membership = SubnetMembership::open(&dir).unwrap();
    assert_eq!(membership.left_at(), Some(1_000));
    assert!(membership.credentials().is_none());
}
