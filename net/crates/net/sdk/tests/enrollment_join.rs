// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 membership bundle and device-side durable join, ending in a live mesh
//! attach with the delivered PSK. Loopback, single process: proves the
//! mechanism, not multi-host or NAT behavior.
#![cfg(feature = "net")]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::bundle::{
    BundleError, MembershipBundle, MembershipIssuer, MembershipReceipt, MeshContact,
};
use net_sdk::enrollment::device::{DeviceJoin, DeviceJoinError, JoinStatus};
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteSpec, MembershipInvite, RedemptionIntent, Relation,
    RelayLocator,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::{RedeemError, Refusal, ResponderKey};
use net_sdk::enrollment::service::{BundleIssuer, EnrollmentService, ServiceConfig, SharedLedger};
use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits};
use net_sdk::identity::Identity;
use net_sdk::{Mesh, MeshBuilder};

const T: Duration = Duration::from_secs(5);
const PSK: [u8; 32] = [0x5A; 32];

/// A well-formed one-hop subnet credential set (for any subject).
fn some_subnet_credentials() -> net::adapter::net::subnet::SubnetCredentialSet {
    use net::adapter::net::identity::EntityKeypair;
    use net::adapter::net::subnet::{
        SubnetGrant, SubnetIssuerGrant, SubnetRights, TopologySubnetId,
    };
    let root = EntityKeypair::generate();
    let issuer = EntityKeypair::generate();
    let issuer_grant = SubnetIssuerGrant::try_issue(
        &root,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3]),
        0,
        issuer.entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        now() - 60,
        3600,
    )
    .unwrap();
    let leaf = SubnetGrant::try_issue(
        &issuer,
        root.entity_id().clone(),
        TopologySubnetId::new(&[3, 7]),
        0,
        EntityKeypair::generate().entity_id().clone(),
        SubnetRights::ATTACH,
        1,
        now() - 60,
        600,
    )
    .unwrap();
    net::adapter::net::subnet::SubnetCredentialSet::OneHop { issuer_grant, leaf }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn invite_for(
    issuer: &Identity,
    endpoint: &str,
    key: EnrollmentKey,
    psk: &Psk,
) -> MembershipInvite {
    MembershipInvite::sign(
        issuer,
        InviteSpec {
            trust_domain_name: "lab".into(),
            trust_domain: psk.trust_domain(),
            endpoint: Some(EnrollmentEndpoint::parse(endpoint).unwrap()),
            relay: None,
            enrollment_key: key,
            subnet: None,
            org: None,
            relations: vec![Relation::Mesh],
            intended_subject: None,
            policy: InvitationPolicy::with_options(
                now(),
                Duration::from_secs(3600),
                ApprovalMode::Preauthorized,
            )
            .unwrap(),
        },
    )
    .unwrap()
}

/// An operator: a started mesh node plus an enrollment service delivering
/// that node's PSK and contact.
struct Operator {
    _tmp: tempfile::TempDir,
    identity: Identity,
    mesh: Mesh,
    service: EnrollmentService,
}

impl Operator {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let identity = Identity::generate();
        let mesh = MeshBuilder::new("127.0.0.1:0", &PSK)
            .unwrap()
            .identity(identity.clone())
            .build()
            .await
            .unwrap();
        mesh.start();
        let contact = MeshContact {
            addr: Some(mesh.local_addr()),
            noise_pubkey: *mesh.public_key(),
            node_id: mesh.node_id(),
            relay: None,
        };
        let ledger = EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            identity.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap();
        let ledger: SharedLedger = Arc::new(parking_lot::Mutex::new(ledger));
        let issuer = MembershipIssuer::new(identity.clone(), Psk::new(PSK), contact);
        let service = EnrollmentService::bind(
            "127.0.0.1:0".parse().unwrap(),
            &identity,
            ledger,
            Arc::new(issuer),
            ServiceConfig::default(),
        )
        .await
        .unwrap();
        Self {
            _tmp: tmp,
            identity,
            mesh,
            service,
        }
    }

    fn invite(&self) -> MembershipInvite {
        let invite = invite_for(
            &self.identity,
            &self.service.local_addr().to_string(),
            self.service.enrollment_key(),
            &Psk::new(PSK),
        );
        self.service
            .ledger()
            .lock()
            .offer(invite.offer_spec(), now())
            .unwrap();
        invite
    }
}

async fn attach(identity: Identity, psk: &Psk, contact: &MeshContact) -> Result<Mesh, String> {
    let mesh = MeshBuilder::new("127.0.0.1:0", psk.expose_bytes())
        .unwrap()
        .identity(identity)
        .build()
        .await
        .map_err(|e| e.to_string())?;
    mesh.start();
    match tokio::time::timeout(
        Duration::from_secs(3),
        mesh.connect_via(
            &contact.addr.unwrap().to_string(),
            &contact.noise_pubkey,
            contact.node_id,
        ),
    )
    .await
    {
        Ok(Ok(())) => Ok(mesh),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".into()),
    }
}

#[tokio::test]
async fn a_clean_device_joins_by_link_survives_restart_and_attaches_with_the_delivered_psk() {
    let op = Operator::start().await;
    let link = op.invite().encode();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("join");

    // Device: no PSK anywhere. Decode the link, persist, redeem.
    let invite = MembershipInvite::decode(&link).unwrap();
    let mut join = DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap();
    let device_id = join.identity().entity_id().clone();
    assert!(matches!(
        join.redeem(T).await.unwrap(),
        JoinStatus::Installed {
            receipt_id: Some(_)
        }
    ));
    drop(join);

    // Restart: same identity and installed bundle, no network needed.
    let Operator {
        _tmp,
        identity: op_identity,
        mesh: op_mesh,
        service,
    } = op;
    service.shutdown().await;
    let mut join = DeviceJoin::open(&dir).unwrap();
    assert_eq!(join.identity().entity_id(), &device_id);
    assert_eq!(
        join.redeem(T).await.unwrap(),
        JoinStatus::Installed { receipt_id: None }
    );
    let bundle = join.bundle().unwrap().clone();
    assert_eq!(bundle.receipt().subject(), &device_id);
    assert_eq!(bundle.receipt().issuer(), op_identity.entity_id());
    assert_eq!(bundle.receipt().relations(), &[Relation::Mesh]);
    assert_eq!(bundle.psk().trust_domain(), Psk::new(PSK).trust_domain());

    // Live admission is observed separately from installation.
    let before = op_mesh.peer_count();
    let device_mesh = attach(join.identity().clone(), bundle.psk(), bundle.contact())
        .await
        .unwrap();
    assert!(op_mesh.peer_count() > before);
    assert_eq!(device_mesh.peer_count(), 1);

    // Negative control: the same contact with a different PSK is not admitted.
    assert!(attach(
        Identity::generate(),
        &Psk::new([0x11; 32]),
        bundle.contact()
    )
    .await
    .is_err());
}

/// Leaving erases the delivered credentials durably and fences redemption:
/// restart stays left, redeem refuses, and only an explicit rejoin goes back
/// to the issuer (recovering the committed issuance under its current
/// authority). The device identity survives throughout.
#[tokio::test]
async fn leaving_erases_the_credentials_and_only_an_explicit_rejoin_recovers() {
    let op = Operator::start().await;
    let invite = MembershipInvite::decode(&op.invite().encode()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("join");
    let mut join = DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap();
    let device_id = join.identity().entity_id().clone();
    join.redeem(T).await.unwrap();
    let psk = *join.bundle().unwrap().psk().expose_bytes();

    assert!(join.leave(1_000).unwrap(), "first leave is new");
    assert!(join.bundle().is_none());
    assert!(!join.leave(2_000).unwrap(), "repeated leave is idempotent");
    assert_eq!(join.left_at(), Some(1_000), "the original time is kept");
    // A subnet leaf renewal completing after leave installs nothing.
    assert!(matches!(
        join.replace_subnet_credentials(&some_subnet_credentials()),
        Err(DeviceJoinError::Left { at: 1_000 })
    ));
    assert!(join.bundle().is_none());
    drop(join);

    // Restart: still left, credentials gone, redemption fenced.
    let mut join = DeviceJoin::open(&dir).unwrap();
    assert_eq!(join.identity().entity_id(), &device_id);
    assert_eq!(join.left_at(), Some(1_000));
    assert!(join.bundle().is_none());
    assert!(matches!(
        join.redeem(T).await,
        Err(DeviceJoinError::Left { at: 1_000 })
    ));
    let raw = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap_or_default())
        .any(|bytes| bytes.windows(32).any(|w| w == psk));
    assert!(
        !raw,
        "the PSK must not remain in the join state after leaving"
    );

    // Explicit rejoin: the issuer re-delivers under its current authority.
    join.rejoin().unwrap();
    assert_eq!(join.left_at(), None);
    assert!(matches!(
        join.redeem(T).await.unwrap(),
        JoinStatus::Installed {
            receipt_id: Some(_)
        }
    ));
    assert_eq!(join.bundle().unwrap().psk().expose_bytes(), &psk);
    drop(op);
}

#[tokio::test]
async fn identity_and_intent_are_persisted_before_the_first_redeem_attempt() {
    let issuer = Identity::generate();
    // Nothing listens here: the first attempt fails after state is durable.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = closed.local_addr().unwrap().to_string();
    drop(closed);
    let key = ResponderKey::derive(&issuer).public();
    let invite = invite_for(&issuer, &endpoint, key, &Psk::new(PSK));

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("join");
    let mut join = DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap();
    let id = join.identity().entity_id().clone();
    let err = join.redeem(Duration::from_secs(2)).await.unwrap_err();
    assert!(
        matches!(
            err,
            DeviceJoinError::Redeem(RedeemError::Io(_) | RedeemError::Timeout)
        ),
        "{err:?}"
    );
    assert!(join.bundle().is_none());
    drop(join);

    let join = DeviceJoin::open(&dir).unwrap();
    assert_eq!(join.identity().entity_id(), &id);
    assert!(join.bundle().is_none());
    // A second begin never replaces the persisted identity.
    assert!(DeviceJoin::begin(&dir, &invite, Identity::generate()).is_err());
}

#[test]
fn corrupt_device_state_refuses_to_open() {
    let issuer = Identity::generate();
    let invite = invite_for(
        &issuer,
        "127.0.0.1:9",
        ResponderKey::derive(&issuer).public(),
        &Psk::new(PSK),
    );
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("join");
    drop(DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap());
    let snapshot = dir.join("enrollment.snapshot");
    let mut bytes = std::fs::read(&snapshot).unwrap();
    bytes[10] ^= 1;
    std::fs::write(&snapshot, &bytes).unwrap();
    assert!(matches!(
        DeviceJoin::open(&dir),
        Err(DeviceJoinError::Corrupt)
    ));
}

#[tokio::test]
async fn an_altered_unsigned_contact_field_in_installed_state_refuses_to_open() {
    let op = Operator::start().await;
    let invite = op.invite();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("join");
    let mut join = DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap();
    join.redeem(T).await.unwrap();
    drop(join);
    // The contact node id is the last field before the 32-byte checksum and is
    // not covered by the receipt signature: only the snapshot checksum guards it.
    let snapshot = dir.join("enrollment.snapshot");
    let mut bytes = std::fs::read(&snapshot).unwrap();
    let last_node_id_byte = bytes.len() - 33;
    bytes[last_node_id_byte] ^= 1;
    std::fs::write(&snapshot, &bytes).unwrap();
    assert!(matches!(
        DeviceJoin::open(&dir),
        Err(DeviceJoinError::Corrupt)
    ));
}

/// Signs a valid receipt but delivers a PSK from another trust domain,
/// bypassing `MembershipIssuer`'s own check (a misconfigured or hostile path).
struct WrongDomainIssuer(Identity);

impl BundleIssuer for WrongDomainIssuer {
    fn issue(
        &self,
        invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal> {
        let receipt = MembershipReceipt::sign(&self.0, invite, intent, now());
        let contact = MeshContact {
            addr: Some("127.0.0.1:1".parse().unwrap()),
            noise_pubkey: [0; 32],
            node_id: 1,
            relay: None,
        };
        Ok(MembershipBundle::new(receipt, Psk::new([0x11; 32]), contact).to_bytes())
    }

    fn may_recover(&self, _: &MembershipInvite, _: &RedemptionIntent) -> Result<(), Refusal> {
        Ok(())
    }
}

#[tokio::test]
async fn the_device_refuses_to_install_a_bundle_that_does_not_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let issuer = Identity::generate();
    let ledger = EnrollmentLedger::create(
        &tmp.path().join("ledger"),
        issuer.entity_id().clone(),
        LedgerLimits::default(),
    )
    .unwrap();
    let service = EnrollmentService::bind(
        "127.0.0.1:0".parse().unwrap(),
        &issuer,
        Arc::new(parking_lot::Mutex::new(ledger)),
        Arc::new(WrongDomainIssuer(issuer.clone())),
        ServiceConfig::default(),
    )
    .await
    .unwrap();
    let invite = invite_for(
        &issuer,
        &service.local_addr().to_string(),
        service.enrollment_key(),
        &Psk::new(PSK),
    );
    service
        .ledger()
        .lock()
        .offer(invite.offer_spec(), now())
        .unwrap();

    let dir = tmp.path().join("join");
    let mut join = DeviceJoin::begin(&dir, &invite, Identity::generate()).unwrap();
    let err = join.redeem(T).await.unwrap_err();
    assert!(
        matches!(err, DeviceJoinError::Bundle(BundleError::TrustDomain)),
        "{err:?}"
    );
    assert!(join.bundle().is_none());
    drop(join);
    assert!(DeviceJoin::open(&dir).unwrap().bundle().is_none());
}

fn fixture() -> (Identity, MembershipInvite, RedemptionIntent, MeshContact) {
    let issuer = Identity::generate();
    let invite = invite_for(
        &issuer,
        "127.0.0.1:9",
        ResponderKey::derive(&issuer).public(),
        &Psk::new(PSK),
    );
    let intent =
        RedemptionIntent::for_invite(&invite, Identity::generate().entity_id().clone()).unwrap();
    let contact = MeshContact {
        addr: Some("127.0.0.1:7000".parse().unwrap()),
        noise_pubkey: [4; 32],
        node_id: 42,
        relay: None,
    };
    (issuer, invite, intent, contact)
}

/// A contact may name only a relay (a device with no known direct address);
/// the locator round-trips, and a contact with neither is refused.
#[test]
fn a_bundle_contact_carries_its_relay_and_may_omit_the_direct_address() {
    let (issuer, invite, intent, _) = fixture();
    let relayed = MeshContact {
        addr: None,
        noise_pubkey: [4; 32],
        node_id: 42,
        relay: Some(RelayLocator {
            endpoint: EnrollmentEndpoint::parse("relay.example.net:3478").unwrap(),
            registration: [7; 16],
        }),
    };
    let receipt = MembershipReceipt::sign(&issuer, &invite, &intent, now());
    let bundle = MembershipBundle::new(receipt.clone(), Psk::new(PSK), relayed.clone());
    let back = MembershipBundle::from_bytes(&bundle.to_bytes()).unwrap();
    assert_eq!(back.contact(), &relayed);
    back.verify_for(&invite, &intent).unwrap();

    let nowhere = MeshContact {
        relay: None,
        ..relayed
    };
    let bytes = MembershipBundle::new(receipt, Psk::new(PSK), nowhere).to_bytes();
    assert!(matches!(
        MembershipBundle::from_bytes(&bytes),
        Err(BundleError::Malformed(_))
    ));
}

#[test]
fn a_bundle_verifies_only_for_its_own_invite_intent_issuer_and_trust_domain() {
    let (issuer, invite, intent, contact) = fixture();
    let receipt = MembershipReceipt::sign(&issuer, &invite, &intent, now());
    let good = MembershipBundle::new(receipt.clone(), Psk::new(PSK), contact.clone());
    let back = MembershipBundle::from_bytes(&good.to_bytes()).unwrap();
    assert_eq!(back, good);
    back.verify_for(&invite, &intent).unwrap();

    // Another device's intent for the same invite.
    let other =
        RedemptionIntent::for_invite(&invite, Identity::generate().entity_id().clone()).unwrap();
    assert_eq!(
        good.verify_for(&invite, &other),
        Err(BundleError::Mismatch("subject"))
    );
    // A receipt signed by someone other than the invite's issuer.
    let forged = MembershipReceipt::sign(&Identity::generate(), &invite, &intent, now());
    assert_eq!(
        MembershipBundle::new(forged, Psk::new(PSK), contact.clone()).verify_for(&invite, &intent),
        Err(BundleError::Mismatch("issuer"))
    );
    // Right receipt, PSK from a different trust domain.
    assert_eq!(
        MembershipBundle::new(receipt, Psk::new([0x11; 32]), contact).verify_for(&invite, &intent),
        Err(BundleError::TrustDomain)
    );

    // Every single-byte alteration of the signed receipt region is refused.
    let bytes = good.to_bytes();
    let receipt_len = good.receipt().to_bytes().len();
    for i in 8..8 + receipt_len {
        let mut altered = bytes.clone();
        altered[i] ^= 1;
        let parsed = MembershipBundle::from_bytes(&altered);
        assert!(
            parsed.is_err() || parsed.unwrap().verify_for(&invite, &intent).is_err(),
            "receipt byte {i} alteration accepted"
        );
    }
}

#[test]
fn the_issuer_never_delivers_a_psk_from_another_trust_domain() {
    let (issuer, invite, intent, contact) = fixture();
    let rotated = MembershipIssuer::new(issuer.clone(), Psk::new([0x11; 32]), contact.clone());
    assert_eq!(rotated.issue(&invite, &intent), Err(Refusal::Unavailable));
    assert_eq!(
        rotated.may_recover(&invite, &intent),
        Err(Refusal::RecoveryClosed)
    );
    let current = MembershipIssuer::new(issuer, Psk::new(PSK), contact);
    let bytes = current.issue(&invite, &intent).unwrap();
    MembershipBundle::from_bytes(&bytes)
        .unwrap()
        .verify_for(&invite, &intent)
        .unwrap();
    current.may_recover(&invite, &intent).unwrap();

    let stranger = MembershipIssuer::new(
        Identity::generate(),
        Psk::new(PSK),
        MeshContact {
            addr: Some("127.0.0.1:1".parse().unwrap()),
            noise_pubkey: [0; 32],
            node_id: 1,
            relay: None,
        },
    );
    assert_eq!(stranger.issue(&invite, &intent), Err(Refusal::Invalid));
}

#[test]
fn debug_output_never_contains_the_psk_or_device_seed() {
    let (issuer, invite, intent, contact) = fixture();
    let receipt = MembershipReceipt::sign(&issuer, &invite, &intent, now());
    let bundle = MembershipBundle::new(receipt, Psk::new([0xC3; 32]), contact.clone());
    let issuer_dbg = MembershipIssuer::new(issuer, Psk::new([0xC3; 32]), contact);
    let text = format!("{bundle:?} {issuer_dbg:?}");
    assert!(!text.contains("195, 195"), "{text}");
    assert!(!text.to_lowercase().contains("c3c3"), "{text}");
    assert!(text.contains("<redacted>"), "{text}");

    let tmp = tempfile::tempdir().unwrap();
    let device = Identity::from_seed([0xD7; 32]);
    let join = DeviceJoin::begin(&tmp.path().join("j"), &invite, device).unwrap();
    let text = format!("{join:?}");
    assert!(!text.contains("215, 215"), "{text}");
    assert!(!text.to_lowercase().contains("d7d7"), "{text}");
}
