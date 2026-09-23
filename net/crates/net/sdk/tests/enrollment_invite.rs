// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 signed membership invite and redemption intent. Offline codec and
//! integrity witnesses only: no listener, connection proof or credential delivery.
#![cfg(feature = "net")]

use std::time::Duration;

use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteError, InviteSpec, MembershipInvite, RedemptionIntent,
    Relation, RelayLocator, JOIN_TOKEN_PREFIX, MAX_INVITE_BYTES,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::store::{ClaimOutcome, EnrollmentLedger, LedgerLimits};
use net_sdk::enrollment::InviteToken;
use net_sdk::identity::{EntityId, Identity};

const T0: u64 = 1_700_000_000;

fn psk() -> Psk {
    Psk::new([7; 32])
}

fn spec(intended: Option<EntityId>, mode: ApprovalMode) -> InviteSpec {
    InviteSpec {
        trust_domain_name: "home-lab".into(),
        trust_domain: psk().trust_domain(),
        endpoint: Some(EnrollmentEndpoint::parse("enroll.example.net:7443").unwrap()),
        relay: None,
        enrollment_key: EnrollmentKey([3; 32]),
        relations: vec![Relation::Mesh],
        intended_subject: intended,
        policy: InvitationPolicy::with_options(T0, Duration::from_secs(86_400), mode).unwrap(),
    }
}

fn mint(issuer: &Identity, intended: Option<EntityId>) -> MembershipInvite {
    MembershipInvite::sign(issuer, spec(intended, ApprovalMode::Preauthorized)).unwrap()
}

#[test]
fn a_signed_link_round_trips_every_bound_field() {
    let issuer = Identity::generate();
    let invite = mint(&issuer, None);
    let link = invite.encode();
    assert!(link.starts_with(JOIN_TOKEN_PREFIX), "{link}");
    // Not URL-shaped: no scheme separator, no visible address.
    assert!(
        !link.contains("://") && !link.contains("enroll.example.net"),
        "{link}"
    );

    let back = MembershipInvite::decode(&format!("  {link}\n")).unwrap();
    assert_eq!(back, invite);
    assert_eq!(back.issuer(), issuer.entity_id());
    assert_eq!(back.trust_domain_name(), "home-lab");
    assert_eq!(back.trust_domain(), psk().trust_domain());
    assert_eq!(back.endpoint().unwrap().as_str(), "enroll.example.net:7443");
    assert_eq!(back.enrollment_key(), EnrollmentKey([3; 32]));
    assert_eq!(back.policy().expires_at(), T0 + 86_400);
    assert_eq!(back.policy().approval_mode(), ApprovalMode::Preauthorized);
    assert_eq!(back.relations(), &[Relation::Mesh]);
    assert!(back.is_bearer());
    assert_eq!(back.digest(), invite.digest());
    // Fresh CSPRNG identifiers: two otherwise identical invites differ.
    let other = mint(&issuer, None);
    assert_ne!(other.invitation_id(), invite.invitation_id());
    assert_ne!(other.digest(), invite.digest());
    assert_eq!(other.scope_digest(), invite.scope_digest());
}

#[test]
fn every_single_byte_alteration_is_refused() {
    let issuer = Identity::generate();
    let bound = mint(&issuer, Some(Identity::generate().entity_id().clone()));
    let bytes = bound.to_bytes().to_vec();
    for i in 0..bytes.len() {
        let mut altered = bytes.clone();
        altered[i] ^= 0x01;
        assert!(
            MembershipInvite::from_bytes(&altered).is_err(),
            "alteration at byte {i} was accepted"
        );
    }
    let mut extended = bytes.clone();
    extended.push(0);
    assert!(MembershipInvite::from_bytes(&extended).is_err());
    assert!(MembershipInvite::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    assert_eq!(MembershipInvite::from_bytes(&bytes).unwrap(), bound);
}

#[test]
fn a_different_issuer_cannot_reuse_the_signature() {
    let issuer = Identity::generate();
    let impostor = Identity::generate();
    let mut bytes = mint(&issuer, None).to_bytes().to_vec();
    bytes[4..36].copy_from_slice(impostor.entity_id().as_bytes());
    assert_eq!(
        MembershipInvite::from_bytes(&bytes),
        Err(InviteError::BadSignature)
    );
}

#[test]
fn malformed_oversize_and_legacy_links_are_refused() {
    let issuer = Identity::generate();
    let link = mint(&issuer, None).encode();
    let body = link.strip_prefix(JOIN_TOKEN_PREFIX).unwrap();
    for bad in [
        body.to_string(),
        format!("net-join:{body}"),
        format!("net-invite:{body}"),
        format!("net-mesh://enroll.example.net:7443/join/{body}"),
        format!("NETMESH-JOIN_{body}"),
        format!("{JOIN_TOKEN_PREFIX}{body}!"),
        format!("{JOIN_TOKEN_PREFIX}{body}?x=1"),
        format!("{JOIN_TOKEN_PREFIX}{}", "A".repeat(MAX_INVITE_BYTES * 2)),
        InviteToken::mint_at(
            issuer.entity_id(),
            "127.0.0.1:1",
            Duration::from_secs(60),
            T0,
        )
        .encode(),
    ] {
        assert!(MembershipInvite::decode(&bad).is_err(), "{bad}");
    }
    assert_eq!(
        MembershipInvite::from_bytes(&vec![0; MAX_INVITE_BYTES + 1]),
        Err(InviteError::TooLarge)
    );
}

#[test]
fn only_strict_host_port_endpoints_are_accepted() {
    for ok in [
        "enroll.example.net:7443",
        "10.0.0.1:1",
        "[::1]:65535",
        "[fe80::1]:7443",
        "localhost:7443",
    ] {
        EnrollmentEndpoint::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in [
        "enroll.example.net",
        "enroll.example.net:",
        "enroll.example.net:0",
        "enroll.example.net:65536",
        "enroll.example.net:+443",
        "https://enroll.example.net:7443",
        "enroll.example.net:7443/path",
        "user@enroll.example.net:7443",
        "enroll example.net:7443",
        "-bad.example.net:7443",
        "bad..example.net:7443",
        "::1:7443",
        "[::1:7443",
        "[nothex]:7443",
        ":7443",
    ] {
        assert!(EnrollmentEndpoint::parse(bad).is_err(), "{bad}");
    }
    let long = format!("{}.net:7443", "a".repeat(260));
    assert!(EnrollmentEndpoint::parse(&long).is_err());
}

#[test]
fn trust_domain_names_and_relation_sets_are_validated_before_signing() {
    let issuer = Identity::generate();
    for name in ["", "has space", "ctl\u{7}", &"x".repeat(65)] {
        let s = InviteSpec {
            trust_domain_name: name.into(),
            ..spec(None, ApprovalMode::Preauthorized)
        };
        assert_eq!(
            MembershipInvite::sign(&issuer, s).unwrap_err(),
            InviteError::TrustDomainName
        );
    }
    for relations in [vec![], vec![Relation::Mesh, Relation::Mesh]] {
        let s = InviteSpec {
            relations,
            ..spec(None, ApprovalMode::Preauthorized)
        };
        assert!(matches!(
            MembershipInvite::sign(&issuer, s),
            Err(InviteError::Relations(_))
        ));
    }
}

#[test]
fn the_delivered_trust_domain_must_be_the_one_signed() {
    let invite = mint(&Identity::generate(), None);
    invite.check_trust_domain(psk().trust_domain()).unwrap();
    assert_eq!(
        invite.check_trust_domain(Psk::new([8; 32]).trust_domain()),
        Err(InviteError::TrustDomainMismatch)
    );
}

#[test]
fn redemption_intent_binds_invite_subject_and_relations() {
    let issuer = Identity::generate();
    let device = Identity::generate().entity_id().clone();
    let stranger = Identity::generate().entity_id().clone();
    let bound = mint(&issuer, Some(device.clone()));

    assert_eq!(
        RedemptionIntent::for_invite(&bound, stranger.clone()),
        Err(InviteError::WrongSubject)
    );
    let intent = RedemptionIntent::for_invite(&bound, device.clone()).unwrap();
    let back = RedemptionIntent::from_bytes(&intent.to_bytes()).unwrap();
    assert_eq!(back, intent);
    assert_eq!(back.digest(), intent.digest());
    back.check_against(&bound).unwrap();

    // Same device, different invitation: the intent does not transfer.
    let other = mint(&issuer, Some(device.clone()));
    assert_eq!(
        intent.check_against(&other),
        Err(InviteError::IntentMismatch)
    );

    // A bearer invite accepts any subject, but each subject's digest differs.
    let bearer = mint(&issuer, None);
    let a = RedemptionIntent::for_invite(&bearer, device.clone()).unwrap();
    let b = RedemptionIntent::for_invite(&bearer, stranger).unwrap();
    assert_ne!(a.digest(), b.digest());
    assert_eq!(a.claimant().subject, device);
    assert_eq!(a.claimant().intent_digest, a.digest());

    let mut bytes = intent.to_bytes();
    bytes.push(0);
    assert!(RedemptionIntent::from_bytes(&bytes).is_err());
    let mut unknown = intent.to_bytes();
    *unknown.last_mut().unwrap() = 0xFF;
    assert!(matches!(
        RedemptionIntent::from_bytes(&unknown),
        Err(InviteError::Relations(_))
    ));
}

#[test]
fn a_decoded_invite_drives_the_durable_ledger_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let issuer = Identity::generate();
    let mut ledger = EnrollmentLedger::create(
        &tmp.path().join("ledger"),
        issuer.entity_id().clone(),
        LedgerLimits::default(),
    )
    .unwrap();
    let invite =
        MembershipInvite::sign(&issuer, spec(None, ApprovalMode::RequireApproval)).unwrap();
    let offer = ledger.offer(invite.offer_spec(), T0).unwrap();

    // Device side: decode the link and build its intent.
    let received = MembershipInvite::decode(&invite.encode()).unwrap();
    let device = Identity::generate().entity_id().clone();
    let intent = RedemptionIntent::for_invite(&received, device).unwrap();

    // Owner side: recheck the intent against its own invite, then claim.
    intent.check_against(&invite).unwrap();
    let id = received.invitation_id();
    let claimant = intent.claimant();
    assert_eq!(
        ledger.claim(&id, &claimant, T0 + 1).unwrap(),
        ClaimOutcome::PendingApproval
    );
    ledger.approve(&offer, &claimant, T0 + 2).unwrap();
    let receipt = ledger.issue(&id, &claimant, b"bundle", T0 + 3).unwrap();
    assert_eq!(
        ledger.recover(&id, &claimant, T0 + 4).unwrap().receipt_id,
        receipt
    );
    // Replaying the same signed link as a new offer is a duplicate.
    assert!(ledger.offer(received.offer_spec(), T0 + 5).is_err());
}

#[test]
fn debug_output_redacts_the_invitation_identifier_and_link() {
    let invite = mint(&Identity::generate(), None);
    let text = format!("{invite:?}");
    assert!(text.contains("<redacted>"), "{text}");
    let link = invite.encode();
    let body = link.strip_prefix(JOIN_TOKEN_PREFIX).unwrap();
    assert!(!text.contains(&body[..24]), "{text}");
    let id = format!("{:?}", invite.invitation_id().as_bytes());
    assert!(!text.contains(&id[1..id.len() - 1]), "{text}");
}

/// The relay locator is signed with everything else; the direct endpoint may
/// be omitted when a relay is named, but a token must name at least one.
#[test]
fn a_relay_locator_is_signed_and_the_direct_endpoint_is_optional() {
    let issuer = Identity::generate();
    let relay = RelayLocator {
        endpoint: EnrollmentEndpoint::parse("relay.example.net:3478").unwrap(),
        registration: [0x9C; 16],
    };

    let mut both = spec(None, ApprovalMode::Preauthorized);
    both.relay = Some(relay.clone());
    let back =
        MembershipInvite::decode(&MembershipInvite::sign(&issuer, both).unwrap().encode()).unwrap();
    assert_eq!(back.relay(), Some(&relay));
    assert_eq!(
        back.endpoint().map(|e| e.as_str()),
        Some("enroll.example.net:7443")
    );

    let mut relay_only = spec(None, ApprovalMode::Preauthorized);
    relay_only.endpoint = None;
    relay_only.relay = Some(relay.clone());
    let signed = MembershipInvite::sign(&issuer, relay_only).unwrap();
    let back = MembershipInvite::decode(&signed.encode()).unwrap();
    assert_eq!(back.endpoint(), None);
    assert_eq!(back.relay(), Some(&relay));

    // Redirecting the relay registration breaks the issuer signature.
    let mut bytes = signed.to_bytes().to_vec();
    let at = bytes
        .windows(16)
        .position(|w| w == [0x9C; 16])
        .expect("registration id in the signed body");
    bytes[at] ^= 1;
    assert_eq!(
        MembershipInvite::from_bytes(&bytes),
        Err(InviteError::BadSignature)
    );

    let mut neither = spec(None, ApprovalMode::Preauthorized);
    neither.endpoint = None;
    assert!(matches!(
        MembershipInvite::sign(&issuer, neither),
        Err(InviteError::Endpoint(_))
    ));
}
