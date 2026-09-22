// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 PSK-free Noise enrollment session over real loopback TCP: service,
//! client, ledger and a caller-supplied bundle issuer. The bundle bytes are an
//! opaque fixture here; the membership bundle format is a later slice.
#![cfg(feature = "net")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, EnrollmentKey, InviteSpec, MembershipInvite, RedemptionIntent, Relation,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::{
    redeem, RedeemError, RedeemOutcome, Refusal, ResponderKey, NOISE_PROTOCOL,
};
use net_sdk::enrollment::service::{BundleIssuer, EnrollmentService, ServiceConfig, SharedLedger};
use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits, OfferState};
use net_sdk::identity::Identity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const T: Duration = Duration::from_secs(5);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[derive(Default)]
struct Bundles {
    issued: AtomicUsize,
    refuse_recovery: AtomicBool,
}

impl BundleIssuer for Bundles {
    fn issue(
        &self,
        _invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal> {
        self.issued.fetch_add(1, Ordering::SeqCst);
        let mut b = b"BUNDLE:".to_vec();
        b.extend_from_slice(intent.subject().as_bytes());
        Ok(b)
    }

    fn may_recover(
        &self,
        _invite: &MembershipInvite,
        _intent: &RedemptionIntent,
    ) -> Result<(), Refusal> {
        if self.refuse_recovery.load(Ordering::SeqCst) {
            Err(Refusal::Revoked)
        } else {
            Ok(())
        }
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    issuer: Identity,
    service: EnrollmentService,
    bundles: Arc<Bundles>,
}

impl Fixture {
    async fn start(config: ServiceConfig) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let issuer = Identity::generate();
        let ledger = EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            issuer.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap();
        let ledger: SharedLedger = Arc::new(parking_lot::Mutex::new(ledger));
        let bundles = Arc::new(Bundles::default());
        let service = EnrollmentService::bind(
            "127.0.0.1:0".parse().unwrap(),
            &issuer,
            ledger,
            bundles.clone(),
            config,
        )
        .await
        .unwrap();
        Self {
            _tmp: tmp,
            issuer,
            service,
            bundles,
        }
    }

    fn sign(&self, mode: ApprovalMode, key: EnrollmentKey) -> MembershipInvite {
        sign_by(&self.issuer, self.service.local_addr().port(), mode, key)
    }

    /// Sign and record an invite pinned to this service's key.
    fn invite(&self, mode: ApprovalMode) -> MembershipInvite {
        let invite = self.sign(mode, self.service.enrollment_key());
        self.service
            .ledger()
            .lock()
            .offer(invite.offer_spec(), now())
            .unwrap();
        invite
    }

    fn state(&self, invite: &MembershipInvite) -> OfferState {
        let ledger = self.service.ledger().lock();
        let statuses = ledger.statuses().unwrap();
        let digest = ledger.invite_digest(&invite.invitation_id()).unwrap();
        assert_eq!(digest, invite.digest());
        assert_eq!(statuses.len(), 1, "fixture records one invite");
        statuses[0].state.clone()
    }
}

fn sign_by(
    issuer: &Identity,
    port: u16,
    mode: ApprovalMode,
    key: EnrollmentKey,
) -> MembershipInvite {
    MembershipInvite::sign(
        issuer,
        InviteSpec {
            trust_domain_name: "test".into(),
            trust_domain: Psk::new([1; 32]).trust_domain(),
            endpoint: EnrollmentEndpoint::parse(&format!("127.0.0.1:{port}")).unwrap(),
            enrollment_key: key,
            relations: vec![Relation::Mesh],
            intended_subject: None,
            policy: InvitationPolicy::with_options(now(), Duration::from_secs(3600), mode).unwrap(),
        },
    )
    .unwrap()
}

async fn join(invite: &MembershipInvite, device: &Identity) -> Result<RedeemOutcome, RedeemError> {
    // Received as a link, exactly as a device would.
    let invite = MembershipInvite::decode(&invite.encode()).unwrap();
    let intent = RedemptionIntent::for_invite(&invite, device.entity_id().clone()).unwrap();
    redeem(&invite, device, &intent, T).await
}

#[tokio::test]
async fn a_default_link_redeems_without_approval_and_a_retry_recovers_the_same_bytes() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let device = Identity::generate();

    let first = join(&invite, &device).await.unwrap();
    let RedeemOutcome::Issued {
        receipt_id,
        recovered: false,
        bundle,
    } = first
    else {
        panic!("expected first issuance, got {first:?}");
    };
    assert!(bundle.ends_with(device.entity_id().as_bytes()));

    // Lost response: a fresh session and fresh proof from the same device
    // recovers the committed bytes; the issuer is not asked again.
    let again = join(&invite, &device).await.unwrap();
    assert_eq!(
        again,
        RedeemOutcome::Issued {
            receipt_id,
            recovered: true,
            bundle
        }
    );
    assert_eq!(fx.bundles.issued.load(Ordering::SeqCst), 1);
    assert!(matches!(fx.state(&invite), OfferState::Issued { .. }));
}

#[tokio::test]
async fn a_second_device_cannot_redeem_a_claimed_bearer_link() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    join(&invite, &Identity::generate()).await.unwrap();
    let err = join(&invite, &Identity::generate()).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Refused(Refusal::Conflict)),
        "{err:?}"
    );
    assert_eq!(fx.bundles.issued.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn require_approval_pends_until_the_owner_approves_that_claim() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::RequireApproval);
    let device = Identity::generate();
    assert_eq!(
        join(&invite, &device).await.unwrap(),
        RedeemOutcome::PendingApproval
    );
    assert_eq!(fx.bundles.issued.load(Ordering::SeqCst), 0);

    let received = MembershipInvite::decode(&invite.encode()).unwrap();
    let claimant = RedemptionIntent::for_invite(&received, device.entity_id().clone())
        .unwrap()
        .claimant();
    {
        let mut ledger = fx.service.ledger().lock();
        let offer = ledger.statuses().unwrap()[0].offer_id;
        ledger.approve(&offer, &claimant, now()).unwrap();
    }
    assert!(matches!(
        join(&invite, &device).await.unwrap(),
        RedeemOutcome::Issued {
            recovered: false,
            ..
        }
    ));
}

#[tokio::test]
async fn a_responder_without_the_pinned_key_fails_the_handshake_and_claims_nothing() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let other = ResponderKey::derive(&Identity::generate()).public();
    let invite = fx.sign(ApprovalMode::Preauthorized, other);
    fx.service
        .ledger()
        .lock()
        .offer(invite.offer_spec(), now())
        .unwrap();
    let err = join(&invite, &Identity::generate()).await.unwrap_err();
    assert!(matches!(err, RedeemError::Handshake), "{err:?}");
    assert_eq!(fx.state(&invite), OfferState::Offered);
}

#[tokio::test]
async fn unrecorded_and_foreign_issuer_invites_are_invalid() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let key = fx.service.enrollment_key();
    let unrecorded = fx.sign(ApprovalMode::Preauthorized, key);
    let foreign = sign_by(
        &Identity::generate(),
        fx.service.local_addr().port(),
        ApprovalMode::Preauthorized,
        key,
    );
    for invite in [unrecorded, foreign] {
        let err = join(&invite, &Identity::generate()).await.unwrap_err();
        assert!(
            matches!(err, RedeemError::Refused(Refusal::Invalid)),
            "{err:?}"
        );
    }
    assert_eq!(fx.bundles.issued.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn revocation_blocks_issue_and_current_authority_blocks_recovery() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let revoked = fx.invite(ApprovalMode::Preauthorized);
    {
        let mut ledger = fx.service.ledger().lock();
        let offer = ledger.statuses().unwrap()[0].offer_id;
        ledger.revoke(&offer, now()).unwrap();
    }
    let err = join(&revoked, &Identity::generate()).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Refused(Refusal::Revoked)),
        "{err:?}"
    );

    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let device = Identity::generate();
    join(&invite, &device).await.unwrap();
    fx.bundles.refuse_recovery.store(true, Ordering::SeqCst);
    let err = join(&invite, &device).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Refused(Refusal::Revoked)),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_intent_for_another_device_is_refused_before_connecting() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let device = Identity::generate();
    let other =
        RedemptionIntent::for_invite(&invite, Identity::generate().entity_id().clone()).unwrap();
    let err = redeem(&invite, &device, &other, T).await.unwrap_err();
    assert!(matches!(err, RedeemError::Intent(_)), "{err:?}");
    assert_eq!(fx.state(&invite), OfferState::Offered);
}

// ---- raw sessions: a hand-written client following the documented protocol ----

const PROLOGUE: &[u8] = b"net-mesh enrollment session v1";
const TRANSCRIPT_DOMAIN: &[u8] = b"net-mesh enrollment redeem v1";

async fn frame(s: &mut TcpStream, b: &[u8]) {
    s.write_all(&(b.len() as u16).to_be_bytes()).await.unwrap();
    s.write_all(b).await.unwrap();
}

async fn read(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len).await.ok()?;
    let mut b = vec![0u8; u16::from_be_bytes(len) as usize];
    s.read_exact(&mut b).await.ok()?;
    Some(b)
}

/// Complete the handshake; return the stream, transport and handshake hash.
async fn handshake(
    addr: std::net::SocketAddr,
    key: EnrollmentKey,
) -> (TcpStream, snow::TransportState, [u8; 32]) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut hs = snow::Builder::new(NOISE_PROTOCOL.parse().unwrap())
        .prologue(PROLOGUE)
        .unwrap()
        .remote_public_key(&key.0)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut buf = [0u8; 128];
    let n = hs.write_message(&[], &mut buf).unwrap();
    frame(&mut s, &buf[..n]).await;
    let msg2 = read(&mut s).await.unwrap();
    hs.read_message(&msg2, &mut buf).unwrap();
    let hh: [u8; 32] = hs.get_handshake_hash().try_into().unwrap();
    (s, hs.into_transport_mode().unwrap(), hh)
}

fn request(invite: &MembershipInvite, device: &Identity, hh: &[u8; 32]) -> Vec<u8> {
    let intent = RedemptionIntent::for_invite(invite, device.entity_id().clone()).unwrap();
    let mut msg = TRANSCRIPT_DOMAIN.to_vec();
    msg.extend_from_slice(hh);
    msg.extend_from_slice(&invite.digest());
    msg.extend_from_slice(device.entity_id().as_bytes());
    msg.extend_from_slice(&intent.digest());
    let (inv, int) = (invite.to_bytes(), intent.to_bytes());
    let mut out = b"NMRQ".to_vec();
    out.extend_from_slice(&(inv.len() as u16).to_le_bytes());
    out.extend_from_slice(inv);
    out.extend_from_slice(&(int.len() as u16).to_le_bytes());
    out.extend_from_slice(&int);
    out.extend_from_slice(&device.sign(&msg));
    out
}

async fn exchange(
    s: &mut TcpStream,
    t: &mut snow::TransportState,
    plain: &[u8],
) -> Option<Vec<u8>> {
    let mut sealed = vec![0u8; plain.len() + 16];
    let n = t.write_message(plain, &mut sealed).unwrap();
    frame(s, &sealed[..n]).await;
    let reply = read(s).await?;
    let mut out = vec![0u8; reply.len()];
    let n = t.read_message(&reply, &mut out).unwrap();
    out.truncate(n);
    Some(out)
}

const REFUSED_INVALID: &[u8] = &[b'N', b'M', b'R', b'S', 2, 1];

#[tokio::test]
async fn a_proof_signed_for_another_session_is_refused_without_claiming() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let device = Identity::generate();
    let addr = fx.service.local_addr();
    let key = fx.service.enrollment_key();

    // Proof bound to session A, replayed inside session B.
    let (_a, _ta, hh_a) = handshake(addr, key).await;
    let captured = request(&invite, &device, &hh_a);
    let (mut b, mut tb, _hh_b) = handshake(addr, key).await;
    assert_eq!(
        exchange(&mut b, &mut tb, &captured).await.unwrap(),
        REFUSED_INVALID
    );
    assert_eq!(fx.state(&invite), OfferState::Offered);

    // The same device with a proof for the live session succeeds.
    let (mut c, mut tc, hh_c) = handshake(addr, key).await;
    let ok = exchange(&mut c, &mut tc, &request(&invite, &device, &hh_c))
        .await
        .unwrap();
    assert_eq!(ok[4], 0, "issued tag");
}

#[tokio::test]
async fn garbage_oversized_and_truncated_input_closes_the_session_only() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let addr = fx.service.local_addr();

    // Unauthenticated garbage in place of Noise msg1.
    let mut s = TcpStream::connect(addr).await.unwrap();
    frame(&mut s, &[0xAB; 48]).await;
    assert!(read(&mut s).await.is_none());

    // Oversized handshake frame length: refused before allocation.
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(&u16::MAX.to_be_bytes()).await.unwrap();
    assert!(read(&mut s).await.is_none());

    // Valid session, malformed request: refused as invalid, nothing claimed.
    let (mut s, mut t, _) = handshake(addr, fx.service.enrollment_key()).await;
    assert_eq!(
        exchange(&mut s, &mut t, b"NMRQ\x00").await.unwrap(),
        REFUSED_INVALID
    );
    assert_eq!(fx.state(&invite), OfferState::Offered);

    // The service still serves a real device afterwards.
    assert!(matches!(
        join(&invite, &Identity::generate()).await.unwrap(),
        RedeemOutcome::Issued { .. }
    ));
}

#[tokio::test]
async fn session_capacity_and_deadline_are_enforced() {
    let fx = Fixture::start(ServiceConfig {
        max_sessions: 1,
        session_timeout: Duration::from_millis(400),
    })
    .await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let device = Identity::generate();

    // An idle connection holds the only session slot...
    let _idle = TcpStream::connect(fx.service.local_addr()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let err = join(&invite, &device).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Handshake | RedeemError::Io(_)),
        "{err:?}"
    );
    assert_eq!(fx.state(&invite), OfferState::Offered);

    // ...until its deadline frees the slot.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(matches!(
        join(&invite, &device).await.unwrap(),
        RedeemOutcome::Issued { .. }
    ));
}

#[tokio::test]
async fn shutdown_stops_accepting_new_sessions() {
    let fx = Fixture::start(ServiceConfig::default()).await;
    let invite = fx.invite(ApprovalMode::Preauthorized);
    let Fixture { service, _tmp, .. } = fx;
    service.shutdown().await;
    let err = join(&invite, &Identity::generate()).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Io(_) | RedeemError::Handshake),
        "{err:?}"
    );
}
