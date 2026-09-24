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
    // The org audience rides along only when it is the offered org's.
    use net::adapter::net::behavior::org_authority::OwnerAudienceCredential;
    let audience = OwnerAudienceCredential::generate(org.org_id());
    let with_audience = base()
        .with_org_membership(right.clone())
        .with_org_audience(&audience);
    with_audience.verify_for(&invite, &intent).unwrap();
    let back = MembershipBundle::from_bytes(&with_audience.to_bytes()).unwrap();
    assert_eq!(
        back.org_audience().unwrap().encode_config(),
        audience.encode_config()
    );
    assert!(format!("{back:?}").contains(r#"org_audience: Some("<redacted>")"#));
    let foreign = OwnerAudienceCredential::generate(OrgKeypair::generate().org_id());
    assert!(matches!(
        base()
            .with_org_membership(right.clone())
            .with_org_audience(&foreign)
            .verify_for(&invite, &intent),
        Err(BundleError::Mismatch("org audience"))
    ));
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
        SubnetRedeemReply::OrgIssued {
            cert: Box::new(cert.clone()),
            audience: None,
        }
    );
    // Survives the wire.
    assert_eq!(
        SubnetRedeemReply::from_bytes(&delivered.to_bytes()).unwrap(),
        delivered
    );
    // Asked again (a lost reply): the same certificate — now with the org
    // audience the operator supplied, which also survives the wire.
    let audience =
        net::adapter::net::behavior::org_authority::OwnerAudienceCredential::generate(org.org_id());
    stash
        .put_audience(&claimant, &org.org_id(), &audience)
        .unwrap();
    let again = answer(&request(&device, &link), &device, Some(&stash)).unwrap();
    assert_eq!(
        again,
        SubnetRedeemReply::OrgIssued {
            cert: Box::new(cert),
            audience: Some(audience.encode_config().to_vec()),
        }
    );
    assert_eq!(
        SubnetRedeemReply::from_bytes(&again.to_bytes()).unwrap(),
        again
    );
    // The stash refuses another org's audience.
    let foreign_audience =
        net::adapter::net::behavior::org_authority::OwnerAudienceCredential::generate(
            OrgKeypair::generate().org_id(),
        );
    assert!(stash
        .put_audience(&claimant, &org.org_id(), &foreign_audience)
        .is_err());
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
    use net::adapter::net::identity::EntityKeypair;
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
    /// The membership (and the org's audience) a device receives through
    /// enrollment with `org approve --audience`.
    fn enrolled_membership(
        org: &OrgKeypair,
        device: &Identity,
        audience: &OwnerAudienceCredential,
    ) -> (OrgMembershipCert, OwnerAudienceCredential) {
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
        stash
            .put_audience(&claimant, &org.org_id(), audience)
            .unwrap();
        ledger.approve(&offer_id, &claimant, now()).unwrap();
        let bytes = MembershipIssuer::new(operator, Psk::new(PSK), contact())
            .with_org_certs(stash)
            .issue(&invite, &intent)
            .unwrap();
        let bundle = MembershipBundle::from_bytes(&bytes).unwrap();
        bundle.verify_for(&invite, &intent).unwrap();
        (
            bundle.org_membership().unwrap().clone(),
            bundle
                .org_audience()
                .expect("the org audience was delivered"),
        )
    }

    /// The decisive witness for O1: a membership certificate delivered by
    /// enrollment, adopted through the production ceremony and installed
    /// from its directory (the path joined `up` takes), is admitted by a
    /// provider of that org for an org-protected (same-org) call. The
    /// dispatcher grant is issued separately (join never emits one).
    ///
    /// Private discovery keys on the org's shared owner audience: the
    /// operator mints it once (`org audience-keygen`), the device receives
    /// it through enrollment, and the provider adopted with the same file —
    /// no out-of-band pre-staging.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_enrollment_delivered_membership_is_admitted_for_an_org_protected_call() {
        let org = OrgKeypair::generate();
        let device_identity = Identity::generate();
        let org_audience = OwnerAudienceCredential::generate(org.org_id());
        let (cert, delivered) = enrolled_membership(&org, &device_identity, &org_audience);

        // Device: adopt the delivered certificate and audience, then install
        // from the dir (the path joined `up` takes).
        let tmp = tempfile::tempdir().unwrap();
        let device_dir = tmp.path().join("device-authority");
        NodeAuthority::adopt_with_audience(
            &device_dir,
            cert.clone(),
            device_identity.entity_id(),
            0,
            None,
            &delivered,
        )
        .unwrap();
        let (device_node, device_configs) = node_for(&device_identity).await;
        let device =
            Mesh::from_node_arc(device_node, device_configs, Some(device_identity.clone()));
        device.install_org_authority(&device_dir).unwrap();

        // Provider: a member of the same org, adopted with the org's
        // audience file (`node adopt --audience`).
        let provider_identity = Identity::generate();
        let provider_dir = tmp.path().join("provider-authority");
        NodeAuthority::adopt_with_audience(
            &provider_dir,
            OrgMembershipCert::try_issue(&org, provider_identity.entity_id().clone(), 1, YEAR)
                .unwrap(),
            provider_identity.entity_id(),
            0,
            None,
            &OwnerAudienceCredential::decode_config(&org_audience.encode_config()).unwrap(),
        )
        .unwrap();
        let (provider_node, provider_configs) = node_for(&provider_identity).await;
        let provider = Mesh::from_node_arc(
            provider_node,
            provider_configs,
            Some(provider_identity.clone()),
        );
        provider.install_org_authority(&provider_dir).unwrap();

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

    /// Connect `caller` to `provider` and wait until both pin each other's
    /// entity (org admission binds the caller's membership to it).
    async fn link_up(caller: &Mesh, provider: &Mesh) {
        let caller_id = caller.node_id();
        caller
            .node()
            .connect_via(
                provider.local_addr(),
                provider.public_key(),
                provider.node_id(),
            )
            .await
            .unwrap();
        for m in [caller, provider] {
            m.node()
                .announce_capabilities(CapabilitySet::new())
                .await
                .unwrap();
        }
        for _ in 0..100 {
            if caller.node().peer_entity_id(provider.node_id()).is_some()
                && provider.node().peer_entity_id(caller_id).is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("entity pins were not established");
    }

    /// A member mesh: adopted into `org` at generation 1 (in `dir`), with
    /// the org's shared owner audience pre-staged when given.
    async fn member(
        org: &OrgKeypair,
        identity: &Identity,
        dir: &std::path::Path,
        audience: Option<&[u8]>,
    ) -> Mesh {
        let adopted = NodeAuthority::adopt(
            dir,
            OrgMembershipCert::try_issue(org, identity.entity_id().clone(), 1, YEAR).unwrap(),
            identity.entity_id(),
            0,
            None,
        )
        .unwrap();
        let (node, configs) = node_for(identity).await;
        let authority = match audience {
            None => adopted,
            Some(bytes) => NodeAuthority {
                config: adopted.config.clone(),
                audience: OwnerAudienceCredential::decode_config(bytes).unwrap(),
                revocation: adopted.revocation.clone(),
            },
        };
        node.install_node_authority(Arc::new(authority)).unwrap();
        node.set_owner_cert_emission(true).unwrap();
        let mesh = Mesh::from_node_arc(node, configs, Some(identity.clone()));
        mesh.start();
        mesh
    }

    /// A provider serving `org.ping` to same-org callers.
    fn serve_ping(provider: &Mesh) -> net_sdk::mesh_rpc::ServeHandle {
        provider
            .serve_org(
                "org.ping",
                OrgAccess::SameOrg,
                |_caller: OrgCaller, req: Ping| async move { Ok(Pong { n: req.n + 1 }) },
            )
            .unwrap()
    }

    /// Call `org.ping` as `caller` (retrying while discovery converges);
    /// `Ok` when admitted, the last error otherwise.
    async fn ping(
        caller: &Mesh,
        provider: &Mesh,
        org: &OrgKeypair,
        who: &Identity,
    ) -> Result<(), String> {
        let dispatcher =
            OrgDispatcherGrant::try_issue(org, who.entity_id().clone(), DispatcherScope::Any, 3600)
                .unwrap();
        let cert = OrgMembershipCert::try_issue(org, who.entity_id().clone(), 1, YEAR).unwrap();
        let client = caller
            .org(OrgCredentials::new(cert, dispatcher, vec![], vec![]).unwrap())
            .map_err(|e| e.to_string())?;
        let mut last = String::new();
        for _ in 0..60 {
            provider
                .node()
                .announce_capabilities(CapabilitySet::new())
                .await
                .ok();
            match client.call::<Ping, Pong>("org.ping", &Ping { n: 1 }).await {
                Ok(_) => return Ok(()),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(last)
    }

    /// O2's decisive witness. A root-signed floor applied at the provider
    /// (through the attested floor service) revokes member B: B's calls with
    /// its old certificate are refused, while member C of the same org is
    /// still admitted; the provider's attestation, signed over the exact
    /// request, reports the floor applied; and after the provider restarts
    /// (a new node reopening its persisted authority) B is still refused and
    /// C still admitted.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_floor_revokes_one_member_at_the_provider_and_survives_its_restart() {
        use net::adapter::net::behavior::org::OrgRevocationBundle;
        use net_sdk::org::floors::{
            request_org_floor, serve_org_floor, OrgFloorOutcome, OrgFloorRequest,
        };

        let org = OrgKeypair::generate();
        let tmp = tempfile::tempdir().unwrap();
        let (p_id, b_id, c_id) = (
            Identity::generate(),
            Identity::generate(),
            Identity::generate(),
        );
        let provider = member(&org, &p_id, &tmp.path().join("p"), None).await;
        let audience = provider
            .node()
            .node_authority()
            .unwrap()
            .audience
            .encode_config();
        let b = member(&org, &b_id, &tmp.path().join("b"), Some(&audience)).await;
        let c = member(&org, &c_id, &tmp.path().join("c"), Some(&audience)).await;
        let _floor_service = serve_org_floor(provider.node()).unwrap();
        let _ping = serve_ping(&provider);
        link_up(&b, &provider).await;
        link_up(&c, &provider).await;
        ping(&b, &provider, &org, &b_id)
            .await
            .expect("B admitted before removal");
        ping(&c, &provider, &org, &c_id)
            .await
            .expect("C admitted before removal");

        // Remove B: floor 2 kills B's generation-1 certificate. Delivered by
        // C's node here — any peer may carry a root-signed floor.
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(b_id.entity_id().clone(), 2u32);
        let bundle = OrgRevocationBundle::try_issue(&org, &floors).unwrap();
        let request = OrgFloorRequest::new(&bundle, p_id.entity_id().clone()).unwrap();
        let attestation = request_org_floor(
            c.node(),
            provider.node_id(),
            &request,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        attestation.verify_for(&request).unwrap();
        assert_eq!(attestation.outcome, OrgFloorOutcome::Applied);
        assert_eq!(attestation.floor_of(b_id.entity_id()), Some(2));
        // An attestation does not verify against any other request.
        let other = OrgFloorRequest::new(&bundle, p_id.entity_id().clone()).unwrap();
        assert!(attestation.verify_for(&other).is_err());

        assert!(
            ping(&b, &provider, &org, &b_id).await.is_err(),
            "B's old membership is refused"
        );
        ping(&c, &provider, &org, &c_id)
            .await
            .expect("C still admitted");

        // The provider restarts: a new node reopening its persisted authority.
        drop(_ping);
        drop(_floor_service);
        let p_node_dir = tmp.path().join("p");
        provider.shutdown().await.unwrap();
        let (node, configs) = node_for(&p_id).await;
        let restarted = Mesh::from_node_arc(node, configs, Some(p_id.clone()));
        restarted.install_org_authority(&p_node_dir).unwrap();
        restarted.start();
        assert_eq!(
            restarted
                .node()
                .node_authority()
                .unwrap()
                .revocation
                .floor_for(&org.org_id(), b_id.entity_id()),
            2,
            "the floor was persisted"
        );
        let _ping = serve_ping(&restarted);
        link_up(&b, &restarted).await;
        link_up(&c, &restarted).await;
        assert!(
            ping(&b, &restarted, &org, &b_id).await.is_err(),
            "B stays refused across the provider's restart"
        );
        ping(&c, &restarted, &org, &c_id)
            .await
            .expect("C still admitted after restart");
    }

    /// A node that holds no org authority attests that it enforces nothing;
    /// a request naming another node is refused outright.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_node_without_org_authority_attests_not_member() {
        use net::adapter::net::behavior::org::OrgRevocationBundle;
        use net_sdk::org::floors::{answer_org_floor, OrgFloorOutcome, OrgFloorRequest};
        let org = OrgKeypair::generate();
        let node = EntityKeypair::generate();
        let mut floors = std::collections::BTreeMap::new();
        floors.insert(Identity::generate().entity_id().clone(), 2u32);
        let bundle = OrgRevocationBundle::try_issue(&org, &floors).unwrap();
        let request = OrgFloorRequest::new(&bundle, node.entity_id().clone()).unwrap();
        let attestation = answer_org_floor(&request.to_bytes(), &node, None, now()).unwrap();
        attestation.verify_for(&request).unwrap();
        assert_eq!(attestation.outcome, OrgFloorOutcome::NotMember);
        // A tampered attestation does not verify.
        let mut forged = attestation.to_bytes();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        let forged = net_sdk::org::floors::OrgFloorAttestation::from_bytes(&forged).unwrap();
        assert!(forged.verify_for(&request).is_err());
        let elsewhere =
            OrgFloorRequest::new(&bundle, EntityKeypair::generate().entity_id().clone()).unwrap();
        assert!(answer_org_floor(&elsewhere.to_bytes(), &node, None, now()).is_err());
    }
}
