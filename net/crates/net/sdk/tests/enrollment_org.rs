// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3-2 org half (O1a): the organization relation in invites and bundles.
//! An org invite names exactly one org and always requires operator
//! approval; the issuing node delivers only the membership certificate the
//! operator signed (with the offline org root) for exactly the claiming
//! device; the device accepts only a valid membership of the offered org for
//! itself.
#![cfg(feature = "net")]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::bundle::{BundleError, MembershipBundle, MembershipIssuer, MeshContact};
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteError, InviteSpec, MembershipInvite, OrgOffer,
    RedemptionIntent, Relation,
};
use net_sdk::enrollment::org::OrgCertStash;
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::Refusal;
use net_sdk::enrollment::service::BundleIssuer;
use net_sdk::enrollment::store::{ClaimOutcome, EnrollmentLedger, LedgerLimits};
use net_sdk::identity::Identity;

const PSK: [u8; 32] = [0x44; 32];
const YEAR: u64 = 365 * 24 * 60 * 60;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn spec(relations: Vec<Relation>, org: Option<OrgOffer>, mode: ApprovalMode) -> InviteSpec {
    InviteSpec {
        trust_domain_name: "lab".into(),
        trust_domain: Psk::new(PSK).trust_domain(),
        endpoint: Some(EnrollmentEndpoint::parse("127.0.0.1:9").unwrap()),
        relay: None,
        enrollment_key: EnrollmentKey([5; 32]),
        subnet: None,
        org,
        relations,
        intended_subject: None,
        policy: InvitationPolicy::with_options(now(), Duration::from_secs(3600), mode).unwrap(),
    }
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
fn an_org_invite_names_one_org_and_always_requires_approval() {
    let operator = Identity::generate();
    let org = OrgKeypair::generate();
    let offer = OrgOffer { org: org.org_id() };
    let approve = ApprovalMode::RequireApproval;

    // Relation and offer must agree, and approval is mandatory.
    for (relations, offer, mode) in [
        (vec![Relation::Mesh, Relation::Org], None, approve),
        (vec![Relation::Mesh], Some(offer), approve),
        (
            vec![Relation::Mesh, Relation::Org],
            Some(offer),
            ApprovalMode::Preauthorized,
        ),
    ] {
        assert!(matches!(
            MembershipInvite::sign(&operator, spec(relations, offer, mode)),
            Err(InviteError::Relations(_))
        ));
    }
    let invite = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh, Relation::Org], Some(offer), approve),
    )
    .unwrap();
    let back = MembershipInvite::from_bytes(invite.to_bytes()).unwrap();
    assert_eq!(back.org(), Some(&offer));
    assert_eq!(back.relations(), [Relation::Mesh, Relation::Org]);
    // The offer is signed: flipping a byte of the org id breaks the invite.
    let mut tampered = invite.to_bytes().to_vec();
    let at = tampered
        .windows(32)
        .position(|w| w == org.org_id().0)
        .unwrap();
    tampered[at] ^= 1;
    assert!(MembershipInvite::from_bytes(&tampered).is_err());
}

/// Claim → pending; the operator's approval-time certificate is stashed for
/// that exact claim; issuing delivers it; the device verifies it. Without an
/// approved certificate, or with one for another device, nothing is issued.
#[test]
fn only_the_certificate_approved_for_this_claim_is_delivered() {
    let operator = Identity::generate();
    let org = OrgKeypair::generate();
    let offer = OrgOffer { org: org.org_id() };
    let tmp = tempfile::tempdir().unwrap();
    let mut ledger = EnrollmentLedger::create(
        &tmp.path().join("ledger"),
        operator.entity_id().clone(),
        LedgerLimits::default(),
    )
    .unwrap();
    let invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Org],
            Some(offer),
            ApprovalMode::RequireApproval,
        ),
    )
    .unwrap();
    let offer_id = ledger.offer(invite.offer_spec(), now()).unwrap();
    let device = Identity::generate();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    let claimant = intent.claimant();
    assert_eq!(
        ledger
            .claim(&invite.invitation_id(), &claimant, now())
            .unwrap(),
        ClaimOutcome::PendingApproval
    );

    let stash = Arc::new(OrgCertStash::new(tmp.path().join("org-certs")));
    let issuer = MembershipIssuer::new(operator.clone(), Psk::new(PSK), contact())
        .with_org_certs(stash.clone());
    // Nothing approved yet: the issuer has nothing to deliver.
    assert_eq!(issuer.issue(&invite, &intent), Err(Refusal::Unavailable));

    // The stash refuses a certificate for another device.
    let stranger = Identity::generate();
    let wrong = OrgMembershipCert::try_issue(&org, stranger.entity_id().clone(), 0, YEAR).unwrap();
    assert!(stash.put(&claimant, &wrong).is_err());

    // Approve: the operator signs for exactly this claimant.
    let cert = OrgMembershipCert::try_issue(&org, device.entity_id().clone(), 0, YEAR).unwrap();
    stash.put(&claimant, &cert).unwrap();
    ledger.approve(&offer_id, &claimant, now()).unwrap();
    let bundle = MembershipBundle::from_bytes(&issuer.issue(&invite, &intent).unwrap()).unwrap();
    assert_eq!(bundle.org_membership(), Some(&cert));
    bundle.verify_for(&invite, &intent).unwrap();

    // A certificate of another org is not delivered for this offer.
    let other_org = OrgKeypair::generate();
    let foreign =
        OrgMembershipCert::try_issue(&other_org, device.entity_id().clone(), 0, YEAR).unwrap();
    stash.put(&claimant, &foreign).unwrap();
    assert_eq!(issuer.issue(&invite, &intent), Err(Refusal::Unavailable));
}

/// The device refuses a bundle whose org certificate is missing, is for
/// another org or device, or appears on an invite that offered no org.
#[test]
fn the_device_accepts_only_a_membership_of_the_offered_org_for_itself() {
    let operator = Identity::generate();
    let org = OrgKeypair::generate();
    let offer = OrgOffer { org: org.org_id() };
    let invite = MembershipInvite::sign(
        &operator,
        spec(
            vec![Relation::Mesh, Relation::Org],
            Some(offer),
            ApprovalMode::RequireApproval,
        ),
    )
    .unwrap();
    let device = Identity::generate();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    let base = || {
        let receipt = net_sdk::enrollment::bundle::MembershipReceipt::sign(
            &operator,
            &invite,
            &intent,
            now(),
        );
        MembershipBundle::new(receipt, Psk::new(PSK), contact())
    };
    let mismatch = |b: MembershipBundle| {
        matches!(
            b.verify_for(&invite, &intent),
            Err(BundleError::Mismatch("org membership"))
        )
    };
    assert!(mismatch(base()), "missing certificate");
    let for_other_device =
        OrgMembershipCert::try_issue(&org, Identity::generate().entity_id().clone(), 0, YEAR)
            .unwrap();
    assert!(mismatch(base().with_org_membership(for_other_device)));
    let other_org =
        OrgMembershipCert::try_issue(&OrgKeypair::generate(), device.entity_id().clone(), 0, YEAR)
            .unwrap();
    assert!(mismatch(base().with_org_membership(other_org)));
    let right = OrgMembershipCert::try_issue(&org, device.entity_id().clone(), 0, YEAR).unwrap();
    let good = base().with_org_membership(right.clone());
    good.verify_for(&invite, &intent).unwrap();
    // Survives the wire.
    let back = MembershipBundle::from_bytes(&good.to_bytes()).unwrap();
    assert_eq!(back.org_membership(), Some(&right));

    // A mesh-only invite's bundle must not carry a membership.
    let mesh_only = MembershipInvite::sign(
        &operator,
        spec(vec![Relation::Mesh], None, ApprovalMode::Preauthorized),
    )
    .unwrap();
    let mesh_intent = RedemptionIntent::for_invite(&mesh_only, device.entity_id().clone()).unwrap();
    let receipt = net_sdk::enrollment::bundle::MembershipReceipt::sign(
        &operator,
        &mesh_only,
        &mesh_intent,
        now(),
    );
    let smuggled =
        MembershipBundle::new(receipt, Psk::new(PSK), contact()).with_org_membership(right);
    assert!(matches!(
        smuggled.verify_for(&mesh_only, &mesh_intent),
        Err(BundleError::Mismatch("org membership"))
    ));
}
