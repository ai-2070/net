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
    anchor.start();
    client.start();

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
    let device = Identity::generate();
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
    anchor.start();
    client.start();
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
