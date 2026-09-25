// SPDX-License-Identifier: MIT OR Apache-2.0
//! V3 enrollment through a blind relay (R2 phase 2): the device registers with
//! the relay from its mesh socket and accepts byte-stream splices; a joiner
//! that cannot reach the device directly redeems its invite over a splice. The
//! relay copies Noise ciphertext only, and the invite-pinned responder key
//! still decides who can answer. Loopback; the natsim rows cover NAT behavior.
#![cfg(feature = "nat-traversal")]

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::adapter::net::traversal::blind_relay::{
    open_splice, BlindRelay, RelayConfig, RelayCore, RelayRegistration,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net_sdk::bootstrap_credential::Psk;
use net_sdk::enrollment::invite::{
    EnrollmentEndpoint, InviteSpec, MembershipInvite, RedemptionIntent, Relation, RelayLocator,
};
use net_sdk::enrollment::policy::{ApprovalMode, InvitationPolicy};
use net_sdk::enrollment::redeem::{
    redeem_over, redeem_with_path, RedeemError, RedeemOutcome, RedeemPath, Refusal,
};
use net_sdk::enrollment::service::{BundleIssuer, EnrollmentService, ServiceConfig, SharedLedger};
use net_sdk::enrollment::store::{EnrollmentLedger, LedgerLimits, OfferState};
use net_sdk::identity::Identity;

const T: Duration = Duration::from_secs(10);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Bundles;

impl BundleIssuer for Bundles {
    fn issue(
        &self,
        _invite: &MembershipInvite,
        intent: &RedemptionIntent,
    ) -> Result<Vec<u8>, Refusal> {
        let mut b = b"BUNDLE:".to_vec();
        b.extend_from_slice(intent.subject().as_bytes());
        Ok(b)
    }

    fn may_recover(
        &self,
        _invite: &MembershipInvite,
        _intent: &RedemptionIntent,
    ) -> Result<(), Refusal> {
        Ok(())
    }
}

async fn relay() -> (SocketAddr, Arc<RelayCore>, tokio::task::JoinHandle<()>) {
    let relay = BlindRelay::bind("127.0.0.1:0".parse().unwrap(), RelayConfig::default())
        .await
        .unwrap();
    let addr = relay.local_addr().unwrap();
    let core = relay.core().clone();
    (addr, core, tokio::spawn(async move { relay.run().await }))
}

/// An enrolling device: issuer, ledger, service, and a mesh node registered
/// with the relay whose accepted splices feed the service.
struct Device {
    _tmp: tempfile::TempDir,
    issuer: Identity,
    service: Arc<EnrollmentService>,
    _node: Arc<MeshNode>,
    registration: RelayRegistration,
    _pump: tokio::task::JoinHandle<()>,
}

impl Device {
    async fn start(relay: SocketAddr) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let issuer = Identity::generate();
        let ledger = EnrollmentLedger::create(
            &tmp.path().join("ledger"),
            issuer.entity_id().clone(),
            LedgerLimits::default(),
        )
        .unwrap();
        let ledger: SharedLedger = Arc::new(parking_lot::Mutex::new(ledger));
        let service = Arc::new(
            EnrollmentService::bind(
                "127.0.0.1:0".parse().unwrap(),
                &issuer,
                ledger,
                Arc::new(Bundles),
                ServiceConfig::default(),
            )
            .await
            .unwrap(),
        );

        let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), [0x42; 32])
            .with_heartbeat_interval(Duration::from_millis(500))
            .with_session_timeout(Duration::from_secs(5))
            .with_handshake(3, Duration::from_secs(3));
        cfg.socket_buffers = SocketBufferConfig {
            send_buffer_size: 256 * 1024,
            recv_buffer_size: 256 * 1024,
        };
        let node = Arc::new(MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap());
        node.start();
        let mut registration = node.relay_register(relay).await.unwrap();
        let mut splices = registration.accept_splices(8);
        let sink = service.clone();
        let pump = tokio::spawn(async move {
            while let Some(stream) = splices.recv().await {
                sink.serve_stream(stream);
            }
        });
        Self {
            _tmp: tmp,
            issuer,
            service,
            _node: node,
            registration,
            _pump: pump,
        }
    }

    /// Sign and record a pre-authorized invite. The endpoint is a direct
    /// address the joiner deliberately does not use.
    fn invite(&self) -> MembershipInvite {
        self.invite_with(Some("192.0.2.1:9".to_string()), None)
    }

    /// Sign and record a pre-authorized invite naming `direct` and `relay`.
    fn invite_with(&self, direct: Option<String>, relay: Option<SocketAddr>) -> MembershipInvite {
        let invite = MembershipInvite::sign(
            &self.issuer,
            InviteSpec {
                trust_domain_name: "test".into(),
                trust_domain: Psk::new([1; 32]).trust_domain(),
                endpoint: direct.map(|d| EnrollmentEndpoint::parse(&d).unwrap()),
                relay: relay.map(|r| RelayLocator {
                    endpoint: EnrollmentEndpoint::parse(&r.to_string()).unwrap(),
                    registration: self.registration.id(),
                }),
                enrollment_key: self.service.enrollment_key(),
                subnet: None,
                org: None,
                channel: None,
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
        .unwrap();
        self.service
            .ledger()
            .lock()
            .offer(invite.offer_spec(), now())
            .unwrap();
        invite
    }

    fn state(&self) -> OfferState {
        let statuses = self.service.ledger().lock().statuses().unwrap();
        statuses[0].state.clone()
    }
}

async fn join_through(
    relay: SocketAddr,
    registration: &RelayRegistration,
    invite: &MembershipInvite,
    joiner: &Identity,
) -> Result<RedeemOutcome, RedeemError> {
    let intent = RedemptionIntent::for_invite(invite, joiner.entity_id().clone()).unwrap();
    let stream = open_splice(relay, registration.id())
        .await
        .expect("the relay splices to the registered device");
    redeem_over(stream, invite, joiner, &intent, T).await
}

/// The whole redemption crosses the relay: the joiner never dials the
/// device, the bundle arrives, and the ledger records the issuance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invite_is_redeemed_over_a_blind_relay_splice() {
    let (relay, core, task) = relay().await;
    let device = Device::start(relay).await;
    let invite = device.invite();
    let joiner = Identity::generate();

    let outcome = join_through(relay, &device.registration, &invite, &joiner)
        .await
        .unwrap();
    let RedeemOutcome::Issued {
        recovered: false,
        bundle,
        ..
    } = outcome
    else {
        panic!("expected first issuance, got {outcome:?}");
    };
    assert!(bundle.ends_with(joiner.entity_id().as_bytes()));
    assert!(matches!(device.state(), OfferState::Issued { .. }));
    assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 1);
    task.abort();
}

/// Whoever answers a splice must still prove the invite's pinned enrollment
/// key: a different enrolling node reached through the same relay fails the
/// handshake, and the genuine device's invite stays unclaimed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_splice_answered_by_another_responder_fails_the_handshake() {
    let (relay, _core, task) = relay().await;
    let genuine = Device::start(relay).await;
    let impostor = Device::start(relay).await;
    let invite = genuine.invite();

    let err = join_through(
        relay,
        &impostor.registration,
        &invite,
        &Identity::generate(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RedeemError::Handshake), "{err:?}");
    assert!(
        matches!(genuine.state(), OfferState::Offered),
        "{:?}",
        genuine.state()
    );
    task.abort();
}

/// A loopback TCP port with nothing listening: connecting is refused at once.
fn closed_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

async fn redeem_path(
    invite: &MembershipInvite,
) -> Result<(RedeemOutcome, RedeemPath), RedeemError> {
    let joiner = Identity::generate();
    let intent = RedemptionIntent::for_invite(invite, joiner.entity_id().clone()).unwrap();
    redeem_with_path(invite, &joiner, &intent, T).await
}

/// Direct first: with both paths available the session runs direct and the
/// relay is not touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reachable_direct_endpoint_is_used_before_the_relay() {
    let (relay, core, task) = relay().await;
    let device = Device::start(relay).await;
    let invite = device.invite_with(Some(device.service.local_addr().to_string()), Some(relay));
    let (outcome, path) = redeem_path(&invite).await.unwrap();
    assert!(
        matches!(outcome, RedeemOutcome::Issued { .. }),
        "{outcome:?}"
    );
    assert_eq!(path, RedeemPath::Direct);
    assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 0);
    task.abort();
}

/// Relay fallback: when the direct endpoint cannot be reached, the same
/// redemption completes through the relay automatically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_direct_endpoint_falls_back_to_the_relay() {
    let (relay, core, task) = relay().await;
    let device = Device::start(relay).await;
    let invite = device.invite_with(Some(closed_port().to_string()), Some(relay));
    let (outcome, path) = redeem_path(&invite).await.unwrap();
    assert!(
        matches!(outcome, RedeemOutcome::Issued { .. }),
        "{outcome:?}"
    );
    assert_eq!(path, RedeemPath::Relay);
    assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 1);

    // A token that names only the relay redeems through it too.
    let relay_only = device.invite_with(None, Some(relay));
    let (_, path) = redeem_path(&relay_only).await.unwrap();
    assert_eq!(path, RedeemPath::Relay);
    task.abort();
}

/// Relay availability is never a prerequisite: a dead relay does not stop
/// or delay a reachable direct endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_relay_does_not_block_a_reachable_direct_endpoint() {
    let (relay, _core, task) = relay().await;
    let device = Device::start(relay).await;
    task.abort();
    let invite = device.invite_with(
        Some(device.service.local_addr().to_string()),
        Some(closed_port()),
    );
    let started = std::time::Instant::now();
    let (_, path) = redeem_path(&invite).await.unwrap();
    assert_eq!(path, RedeemPath::Direct);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// Without a relay, an unreachable direct endpoint is simply a failure; an
/// answer from the service (a refusal) is never retried through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failures_are_not_rerouted_without_cause() {
    let (relay, core, task) = relay().await;
    let device = Device::start(relay).await;
    let direct_only = device.invite_with(Some(closed_port().to_string()), None);
    assert!(matches!(
        redeem_path(&direct_only).await,
        Err(RedeemError::Io(_))
    ));

    let invite = device.invite_with(Some(device.service.local_addr().to_string()), Some(relay));
    redeem_path(&invite).await.unwrap();
    // A second device: the bearer link is spent; the refusal comes back direct.
    let err = redeem_path(&invite).await.unwrap_err();
    assert!(
        matches!(err, RedeemError::Refused(Refusal::Conflict)),
        "{err:?}"
    );
    assert_eq!(core.stats().splices_opened.load(Ordering::Relaxed), 0);
    task.abort();
}
