//! R6: the **real SDK enrollment exchange** over the native RTC
//! stand-in.
//!
//! `serve_enrollment(_auto)` registered a typed JSON handler
//! returning `Vec<u8>`, and the typed adapter JSON-encoded that
//! vector — so the body reaching the core was a JSON array of
//! numbers while the core's promotion gate reads the response
//! body's own raw `NMO1` prefix. A real SDK enrollment could
//! therefore never promote its RTC session; only the hand-built raw
//! fixture the landed 4a witness used could.
//!
//! Both sides here are the unchanged intended API, through the real
//! codec path and the real channel registry.
//!
//! Run: `cargo test -p net-mesh-sdk --test enrollment_over_rtc --features "net webrtc"`
#![cfg(all(feature = "net", feature = "webrtc"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig, ENROLL_SERVICE};
use net::adapter::net::PeerAddr;
use net_sdk::delegation::DEFAULT_DELEGATION_DEPTH;
use net_sdk::enrollment::{JoinOutcome, JoinRequest};
use net_sdk::{Identity, Mesh, OperatorEnrollment};

const PSK: [u8; 32] = [0x5Cu8; 32];

fn rtc(serve_bootstrap: bool) -> RtcConfig {
    RtcConfig {
        serve_bootstrap,
        ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
    }
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    predicate()
}

/// A real signed `JoinRequest` against a real single-use invite,
/// answered by the real SDK handler, promotes the exact provisional
/// session — and a rejected one leaves it provisional.
///
/// Inverse: restore `serve_rpc_typed(.., Codec::Json, ..)` in
/// `serve_enrollment_auto` (or in the device's call) — the body is a
/// JSON array, the core's `NMO1` check fails, and nothing promotes.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_real_sdk_enrollment_promotes_the_exact_session() {
    let root = Identity::generate();
    let anchor = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(root.clone())
        .rtc(rtc(true))
        .build()
        .await
        .expect("anchor");
    let client = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .rtc(rtc(false))
        .build()
        .await
        .expect("client");
    anchor.start().expect("mesh start");
    client.start().expect("mesh start");
    // The device that enrolls is the client node itself: a grant
    // promotes only the session of the participant it names
    // (ANCHOR-03), and a browser leaf enrolls its own key.
    let device = node_identity(&client);

    let (_id_client, endpoint) = connect_rtc_loopback(client.node(), anchor.node())
        .await
        .expect("DataChannel + Noise");
    let client_id = client.node().node_id();
    assert!(
        anchor.node().peer_is_provisional(client_id),
        "a browser-facing anchor installs an RTC session as provisional"
    );
    let provisional_session = anchor
        .node()
        .peer_session_id(client_id)
        .expect("the provisional session");

    // Operator side: the SDK's own registration, unchanged.
    let dir = tempfile::tempdir().expect("tempdir");
    let operator = Arc::new(OperatorEnrollment::new(
        root.clone(),
        dir.path().join("devices.json"),
        dir.path().join("revocations.json"),
    ));
    let _serve = anchor
        .serve_enrollment_auto(
            Arc::clone(&operator),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .expect("serve the SDK enrollment service");

    // Device side: a real signed request against a real single-use
    // invite, sent over the RTC session that is already installed —
    // the bootstrap direction §12 gates.
    let invite = operator.invite(anchor.rendezvous_string(), Duration::from_secs(300));
    let request = JoinRequest::create(&device, "device", vec![], &invite);
    let outcome_bytes = client
        .call_raw_bytes(anchor.node().node_id(), ENROLL_SERVICE, request.to_bytes())
        .await
        .expect("the enrollment call must reach the SDK handler");

    assert_eq!(
        &outcome_bytes[..4],
        b"NMO1",
        "the response body itself must carry the raw outcome magic — a JSON \
         array of bytes is what made promotion impossible"
    );
    let outcome = JoinOutcome::from_bytes(&outcome_bytes).expect("decode the outcome");
    assert!(
        matches!(outcome, JoinOutcome::Admitted { .. }),
        "a valid single-use invite must be admitted by the real handler: {outcome:?}"
    );

    assert!(
        wait_for(
            || !anchor.node().peer_is_provisional(client_id),
            Duration::from_secs(10)
        )
        .await,
        "a real SDK Admitted outcome must promote the session that asked"
    );
    assert_eq!(
        anchor.node().peer_session_id(client_id),
        Some(provisional_session),
        "the promoted session is the exact incarnation that enrolled"
    );
    assert_eq!(
        anchor.node().peer_endpoint(client_id),
        Some(PeerAddr::Rtc(endpoint)),
        "on its own endpoint"
    );
}

/// The negative half, same real path: a **spent** invite is
/// rejected, and the session stays provisional.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_rejected_sdk_enrollment_leaves_the_session_provisional() {
    let root = Identity::generate();
    let anchor = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(root.clone())
        .rtc(rtc(true))
        .build()
        .await
        .expect("anchor");
    let client = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .rtc(rtc(false))
        .build()
        .await
        .expect("client");
    anchor.start().expect("mesh start");
    client.start().expect("mesh start");
    connect_rtc_loopback(client.node(), anchor.node())
        .await
        .expect("DataChannel + Noise");
    let client_id = client.node().node_id();

    let dir = tempfile::tempdir().expect("tempdir");
    let operator = Arc::new(OperatorEnrollment::new(
        root.clone(),
        dir.path().join("devices.json"),
        dir.path().join("revocations.json"),
    ));
    let _serve = anchor
        .serve_enrollment_auto(
            Arc::clone(&operator),
            Duration::from_secs(3600),
            DEFAULT_DELEGATION_DEPTH,
        )
        .expect("serve");

    // An invite minted for a DIFFERENT root: a real request the real
    // authority refuses.
    let stranger = Identity::generate();
    let bad_invite = net_sdk::enrollment::InviteToken::mint(
        stranger.entity_id(),
        anchor.rendezvous_string(),
        Duration::from_secs(300),
    );
    let device = Identity::generate();
    let request = JoinRequest::create(&device, "device", vec![], &bad_invite);
    let outcome_bytes = client
        .call_raw_bytes(anchor.node().node_id(), ENROLL_SERVICE, request.to_bytes())
        .await
        .expect("the call itself succeeds; the OUTCOME is the refusal");
    let outcome = JoinOutcome::from_bytes(&outcome_bytes).expect("decode the outcome");
    assert!(
        matches!(outcome, JoinOutcome::Rejected { .. }),
        "an invite for another root must be refused: {outcome:?}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        anchor.node().peer_is_provisional(client_id),
        "a rejected enrollment must leave the session provisional"
    );
}

/// The SDK identity of `mesh`'s own node key — the participant its
/// sessions are. (A provisional RTC client is built without a
/// configured identity, as a browser stand-in.)
fn node_identity(mesh: &Mesh) -> Identity {
    Identity::from_seed(*mesh.node().entity_keypair().secret_bytes())
}

/// A provisional client on its own RTC session, and the identity it
/// enrolls as.
async fn provisional_client(anchor: &Mesh) -> (Mesh, Identity, u64) {
    let client = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .rtc(rtc(false))
        .build()
        .await
        .expect("client");
    client.start().expect("mesh start");
    connect_rtc_loopback(client.node(), anchor.node())
        .await
        .expect("DataChannel + Noise");
    let id = client.node().node_id();
    assert!(
        anchor.node().peer_is_provisional(id),
        "installed provisional"
    );
    let identity = node_identity(&client);
    assert_eq!(identity.node_id(), id, "the node key is the participant");
    (client, identity, id)
}

/// ANCHOR-03: **one device's join body promotes only that device's
/// session.**
///
/// A game invite is bound to its first device, which may re-enroll
/// with it — so device D's signed join body is reusable by design. A
/// custom client opened provisional sessions as other participants
/// (E1, E2, …) and sent D's identical body on each: the registry
/// verified D, bound the nonce to D, answered Admitted, and the core
/// promoted every one of those sessions, because promotion bound the
/// call, the session and the tenant but never compared the grant's
/// subject with the participant being admitted.
///
/// Now the replayed body is still answered (the handler cannot know
/// which session asked), but the session it arrived on stays
/// provisional and the refusal is counted; D's own session, sending
/// the same body, is promoted and bound to D.
///
/// Inverse: drop `set_enrollment_subject_binding(true)` from
/// `serve_game_enrollment` (or the subject check from the core's
/// response path) and the stranger's session is promoted.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn one_devices_join_body_promotes_only_that_devices_session() {
    use net_sdk::game_anchor::{serve_game_enrollment, GameConfig, GameRegistry};

    let issuer = Identity::generate();
    let anchor = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(issuer.clone())
        .rtc(rtc(true))
        .build()
        .await
        .expect("anchor");
    anchor.start().expect("mesh start");
    let registry = Arc::new(
        GameRegistry::from_identity(&issuer, vec![GameConfig::new("chess")]).expect("registry"),
    );
    let _serve = serve_game_enrollment(&anchor, Arc::clone(&registry), Duration::from_secs(3600))
        .expect("serve game enrollment");

    // D: the client whose own key signs the reusable join body.
    let (own, device, d_id) = provisional_client(&anchor).await;
    let invite = registry
        .mint_invite_at(
            "chess",
            anchor.rendezvous_string(),
            Duration::from_secs(3600),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs(),
        )
        .expect("invite");
    let body = JoinRequest::create(&device, "tab", vec![], &invite).to_bytes();

    // A stranger's session replays D's genuine body.
    let (stranger, _, stranger_id) = provisional_client(&anchor).await;
    let outcome = stranger
        .call_raw_bytes(anchor.node().node_id(), ENROLL_SERVICE, body.clone())
        .await
        .expect("the call reaches the handler");
    assert!(
        matches!(
            JoinOutcome::from_bytes(&outcome).expect("decode"),
            JoinOutcome::Admitted { .. }
        ),
        "the premise: D's body verifies, so the handler answers Admitted"
    );
    assert!(
        wait_for(
            || anchor.node().rtc_stats().admission_rejected_outcome() >= 1,
            Duration::from_secs(10)
        )
        .await,
        "the grant names another participant: refused and counted"
    );
    assert!(
        anchor.node().peer_is_provisional(stranger_id),
        "a grant for device D must not promote a session that is not D"
    );
    assert_eq!(anchor.node().rtc_stats().admission_promoted(), 0);

    // D's own session, the same body: promoted, and bound to D.
    let outcome = own
        .call_raw_bytes(anchor.node().node_id(), ENROLL_SERVICE, body)
        .await
        .expect("the call reaches the handler");
    assert!(matches!(
        JoinOutcome::from_bytes(&outcome).expect("decode"),
        JoinOutcome::Admitted { .. }
    ));
    assert!(
        wait_for(
            || !anchor.node().peer_is_provisional(d_id),
            Duration::from_secs(10)
        )
        .await,
        "the device the grant names is promoted"
    );
    assert_eq!(
        anchor.node().peer_granted_device(d_id),
        Some(*device.entity_id().as_bytes()),
        "the admitted session is bound to the granted device"
    );
    assert!(
        anchor.node().peer_is_provisional(stranger_id),
        "and the stranger is still not promoted"
    );
}

/// A second `serve_game_enrollment` on a node already serving enrollment
/// fails to register, and must leave the running service's node-wide
/// admission policy alone. It used to install its own registry's tenant
/// resolver and player cap before failing, so the running service's
/// grants were then resolved by a registry that never issued them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_duplicate_game_enrollment_serve_keeps_the_running_policy() {
    use net_sdk::game_anchor::{serve_game_enrollment, GameConfig, GameRegistry};

    let issuer = Identity::generate();
    let anchor = Mesh::builder("127.0.0.1:0", &PSK)
        .expect("builder")
        .identity(issuer.clone())
        .rtc(rtc(true))
        .build()
        .await
        .expect("anchor");
    anchor.start().expect("mesh start");
    let first = Arc::new(
        GameRegistry::from_identity(&issuer, vec![GameConfig::new("chess")]).expect("registry"),
    );
    let _serve = serve_game_enrollment(&anchor, Arc::clone(&first), Duration::from_secs(3600))
        .expect("serve game enrollment");
    let second = Arc::new(
        GameRegistry::from_identity(&Identity::generate(), vec![GameConfig::new("go")])
            .expect("registry"),
    );
    assert!(
        serve_game_enrollment(&anchor, second, Duration::from_secs(3600)).is_err(),
        "enrollment is already served on this node"
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let invite = first
        .mint_invite_at(
            "chess",
            anchor.rendezvous_string(),
            Duration::from_secs(3600),
            now,
        )
        .expect("invite");
    let body = JoinRequest::create(&Identity::generate(), "tab", vec![], &invite).to_bytes();
    let JoinOutcome::Admitted { chain } =
        JoinOutcome::from_bytes(&first.handle_join_request(&body, Duration::from_secs(3600)))
            .expect("outcome")
    else {
        panic!("the first registry admits its own invite");
    };
    let resolver = anchor
        .node()
        .replace_enrollment_tenant_resolver(None)
        .expect("a resolver is installed");
    assert!(
        resolver(&chain).is_some(),
        "the running service's grants must still resolve to their game"
    );
    assert_eq!(resolver(&chain), first.tenant_of_chain(&chain));
}
