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

/// O1b: an org-only link redeemed over the device's session. Pending until
/// the operator approves with a certificate for exactly this claim; then the
/// device proven on the session gets that certificate (again, on a repeat);
/// another device, a mesh+org link, or a node holding no approved
/// certificates gets nothing.
#[test]
fn a_standalone_org_link_delivers_the_approved_certificate_over_the_session() {
    use net_sdk::enrollment::service::SharedLedger;
    use net_sdk::enrollment::standalone::{
        answer_standalone_redeem, is_standalone_org, SubnetRedeemReply, SubnetRedeemRequest,
    };

    const NODE: u64 = 0x0A11_CE01;
    let operator = Identity::generate();
    let org = OrgKeypair::generate();
    let offer = OrgOffer { org: org.org_id() };
    let tmp = tempfile::tempdir().unwrap();
    let ledger: SharedLedger = Arc::new(parking_lot::Mutex::new(
        EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            operator.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap(),
    ));
    let sign = |relations| {
        let invite = MembershipInvite::sign(
            &operator,
            spec(relations, Some(offer), ApprovalMode::RequireApproval),
        )
        .unwrap();
        let offer_id = ledger.lock().offer(invite.offer_spec(), now()).unwrap();
        (invite, offer_id)
    };
    let (link, offer_id) = sign(vec![Relation::Org]);
    assert!(is_standalone_org(&link));
    let stash = OrgCertStash::new(tmp.path().join("org-certs"));
    let device = Identity::generate();
    let request = |who: &Identity, invite: &MembershipInvite| {
        SubnetRedeemRequest::sign(who, invite, NODE, now())
            .unwrap()
            .to_bytes()
    };
    let answer = |bytes: &[u8], proven: &Identity, source: Option<&OrgCertStash>| {
        answer_standalone_redeem(
            bytes,
            Some(proven.entity_id()),
            NODE,
            &ledger,
            None,
            source.map(|s| s as &dyn net_sdk::enrollment::bundle::OrgCertSource),
            now(),
        )
    };

    // First ask: pending operator approval.
    assert_eq!(
        answer(&request(&device, &link), &device, Some(&stash)),
        Ok(SubnetRedeemReply::PendingApproval)
    );
    // The operator signs for exactly this claim, then approves it.
    let claimant = ledger.lock().pending_claim(&offer_id).unwrap().unwrap();
    assert_eq!(&claimant.subject, device.entity_id());
    ledger.lock().approve(&offer_id, &claimant, now()).unwrap();
    // A certificate of another org, even for this device, is not delivered.
    let foreign =
        OrgMembershipCert::try_issue(&OrgKeypair::generate(), device.entity_id().clone(), 0, YEAR)
            .unwrap();
    stash.put(&claimant, &foreign).unwrap();
    assert_eq!(
        answer(&request(&device, &link), &device, Some(&stash)),
        Err(Refusal::Unavailable)
    );
    let cert = OrgMembershipCert::try_issue(&org, device.entity_id().clone(), 0, YEAR).unwrap();
    stash.put(&claimant, &cert).unwrap();

    // A node holding no approved certificates cannot deliver one.
    assert_eq!(
        answer(&request(&device, &link), &device, None),
        Err(Refusal::Unavailable)
    );
    let delivered = answer(&request(&device, &link), &device, Some(&stash)).unwrap();
    assert_eq!(
        delivered,
        SubnetRedeemReply::OrgIssued(Box::new(cert.clone()))
    );
    // Survives the wire.
    assert_eq!(
        SubnetRedeemReply::from_bytes(&delivered.to_bytes()).unwrap(),
        delivered
    );
    // Asked again (a lost reply): the same certificate.
    assert_eq!(
        answer(&request(&device, &link), &device, Some(&stash)),
        Ok(SubnetRedeemReply::OrgIssued(Box::new(cert)))
    );
    // Another device, over its own proven session, gets nothing.
    let other = Identity::generate();
    assert_eq!(
        answer(&request(&other, &link), &other, Some(&stash)),
        Err(Refusal::Conflict)
    );
    // A link that also carries the mesh relation is not standalone.
    let (full, _) = sign(vec![Relation::Mesh, Relation::Org]);
    assert_eq!(
        answer(&request(&device, &full), &device, Some(&stash)),
        Err(Refusal::Invalid)
    );
}

#[cfg(feature = "cortex")]
mod live {
    use super::*;
    use net::adapter::net::behavior::capability::CapabilitySet;
    use net::adapter::net::behavior::org_authority::{NodeAuthority, OwnerAudienceCredential};
    use net::adapter::net::{ChannelConfigRegistry, MeshNode, MeshNodeConfig};
    use net_sdk::org::{DispatcherScope, OrgAccess, OrgCaller, OrgCredentials, OrgDispatcherGrant};
    use net_sdk::Mesh;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Ping {
        n: u32,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    struct Pong {
        n: u32,
    }

    /// A node that re-announces promptly (the scoped envelope rides the
    /// announce path), with `identity`.
    async fn node_for(identity: &Identity) -> (Arc<MeshNode>, Arc<ChannelConfigRegistry>) {
        let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x52u8; 32])
            .with_heartbeat_interval(Duration::from_millis(200))
            .with_session_timeout(Duration::from_secs(5));
        cfg.min_announce_interval = Duration::from_millis(50);
        cfg.configured_identity = true;
        let mut node = MeshNode::new((**identity.keypair()).clone(), cfg)
            .await
            .unwrap();
        let configs = Arc::new(ChannelConfigRegistry::new());
        node.set_channel_configs(configs.clone());
        (Arc::new(node), configs)
    }

    /// The membership certificate a device receives through enrollment:
    /// an org invite, claimed, approved with a certificate the operator
    /// signed for exactly that claim, issued in the bundle, and verified by
    /// the device against its invite.
    fn enrolled_membership(org: &OrgKeypair, device: &Identity) -> OrgMembershipCert {
        let operator = Identity::generate();
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
                Some(OrgOffer { org: org.org_id() }),
                ApprovalMode::RequireApproval,
            ),
        )
        .unwrap();
        let offer_id = ledger.offer(invite.offer_spec(), now()).unwrap();
        let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
        let claimant = intent.claimant();
        ledger
            .claim(&invite.invitation_id(), &claimant, now())
            .unwrap();
        let stash = Arc::new(OrgCertStash::new(tmp.path().join("org-certs")));
        stash
            .put(
                &claimant,
                &OrgMembershipCert::try_issue(org, device.entity_id().clone(), 1, YEAR).unwrap(),
            )
            .unwrap();
        ledger.approve(&offer_id, &claimant, now()).unwrap();
        let bytes = MembershipIssuer::new(operator, Psk::new(PSK), contact())
            .with_org_certs(stash)
            .issue(&invite, &intent)
            .unwrap();
        let bundle = MembershipBundle::from_bytes(&bytes).unwrap();
        bundle.verify_for(&invite, &intent).unwrap();
        bundle.org_membership().unwrap().clone()
    }

    /// The decisive witness for O1: a membership certificate delivered by
    /// enrollment, adopted through the production ceremony and installed
    /// from its directory (the path joined `up` takes), is admitted by a
    /// provider of that org for an org-protected (same-org) call. The
    /// dispatcher grant is issued separately (join never emits one).
    ///
    /// Private discovery keys on the org's owner audience, which each
    /// adopting node mints for itself and nothing yet distributes; the
    /// provider here is pre-staged with the device's audience, as the
    /// existing live facade test does (§3.4 out-of-band pre-staging).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_enrollment_delivered_membership_is_admitted_for_an_org_protected_call() {
        let org = OrgKeypair::generate();
        let device_identity = Identity::generate();
        let cert = enrolled_membership(&org, &device_identity);

        // Device: adopt the delivered certificate, then install from the dir.
        let tmp = tempfile::tempdir().unwrap();
        let device_dir = tmp.path().join("device-authority");
        NodeAuthority::adopt(
            &device_dir,
            cert.clone(),
            device_identity.entity_id(),
            0,
            None,
        )
        .unwrap();
        let (device_node, device_configs) = node_for(&device_identity).await;
        let device =
            Mesh::from_node_arc(device_node, device_configs, Some(device_identity.clone()));
        device.install_org_authority(&device_dir).unwrap();

        // Provider: a member of the same org (the operator's own adoption),
        // pre-staged with the org's owner audience.
        let provider_identity = Identity::generate();
        let provider_dir = tmp.path().join("provider-authority");
        let adopted = NodeAuthority::adopt(
            &provider_dir,
            OrgMembershipCert::try_issue(&org, provider_identity.entity_id().clone(), 1, YEAR)
                .unwrap(),
            provider_identity.entity_id(),
            0,
            None,
        )
        .unwrap();
        let shared = device
            .node()
            .node_authority()
            .unwrap()
            .audience
            .encode_config();
        let (provider_node, provider_configs) = node_for(&provider_identity).await;
        provider_node
            .install_node_authority(Arc::new(NodeAuthority {
                config: adopted.config.clone(),
                audience: OwnerAudienceCredential::decode_config(&shared).unwrap(),
                revocation: adopted.revocation.clone(),
            }))
            .unwrap();
        provider_node.set_owner_cert_emission(true).unwrap();
        let provider = Mesh::from_node_arc(
            provider_node,
            provider_configs,
            Some(provider_identity.clone()),
        );

        // Live transport between them.
        let device_id = device.node_id();
        let p = provider.node().clone();
        let accept = tokio::spawn(async move { p.accept(device_id).await });
        device
            .connect(
                &provider.local_addr().to_string(),
                provider.public_key(),
                provider.node_id(),
            )
            .await
            .unwrap();
        accept.await.unwrap().unwrap();
        device.start();
        provider.start();
        for m in [&device, &provider] {
            m.node()
                .announce_capabilities(CapabilitySet::new())
                .await
                .unwrap();
        }

        let served_for = Arc::new(parking_lot::Mutex::new(None));
        let seen = served_for.clone();
        let _serve = provider
            .serve_org(
                "enroll.ping",
                OrgAccess::SameOrg,
                move |caller: OrgCaller, req: Ping| {
                    let seen = seen.clone();
                    async move {
                        *seen.lock() = Some((caller.entity.clone(), caller.acting_org));
                        Ok(Pong { n: req.n + 1 })
                    }
                },
            )
            .unwrap();

        // The device calls with its enrolled membership plus a separately
        // issued dispatcher grant.
        let dispatcher = OrgDispatcherGrant::try_issue(
            &org,
            device_identity.entity_id().clone(),
            DispatcherScope::Any,
            3600,
        )
        .unwrap();
        let client = device
            .org(OrgCredentials::new(cert, dispatcher, vec![], vec![]).unwrap())
            .unwrap();
        // Retry while the provider's scoped announcement converges; the
        // call itself is the observation (discovery, then admission).
        let mut outcome = None;
        for _ in 0..100 {
            provider
                .node()
                .announce_capabilities(CapabilitySet::new())
                .await
                .ok();
            match client
                .call::<Ping, Pong>("enroll.ping", &Ping { n: 41 })
                .await
            {
                Ok(pong) => {
                    outcome = Some(pong);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        let pong = outcome.expect("the org-protected call is admitted");
        assert_eq!(pong, Pong { n: 42 });
        let (caller, acting) = served_for.lock().clone().expect("the handler ran");
        assert_eq!(&caller, device_identity.entity_id());
        assert_eq!(acting, org.org_id());
    }
}
