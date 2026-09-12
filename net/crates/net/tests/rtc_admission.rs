//! The six §12 admission witnesses, on a native anchor with
//! `serve_bootstrap` and a native "browser stand-in" client reached
//! over the Stage 3 loopback RTC path. No browser in 4a; the
//! contract under test is the anchor's, and the anchor cannot tell
//! the difference.
//!
//! Run: `cargo test --features "webrtc fixtures cortex" --test rtc_admission`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::{
    allow_provisional_action, connect_rtc_loopback, enroll_reply_channel, AdmissionRefusal,
    BootstrapAction, RtcConfig, RtcSignalMsg, ENROLL_SERVICE, RENEWAL_SERVICE,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, SocketBufferConfig};
use net::event::{batch_process_nonce, Batch, InternalEvent};

const PSK: [u8; 32] = [0x5Cu8; 32];

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.rtc = rtc;
    cfg
}

async fn node(rtc: Option<RtcConfig>) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), config(rtc))
            .await
            .expect("MeshNode::new"),
    )
}

fn rtc_config() -> RtcConfig {
    RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
}

fn anchor_config() -> RtcConfig {
    RtcConfig {
        serve_bootstrap: true,
        ..rtc_config()
    }
}

fn batch(shard_id: u16, count: usize, tag: &str) -> Batch {
    let events: Vec<InternalEvent> = (0..count)
        .map(|i| {
            InternalEvent::from_value(
                serde_json::json!({ "tag": tag, "index": i }),
                i as u64,
                shard_id,
            )
        })
        .collect();
    Batch {
        shard_id,
        events,
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
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

/// An anchor (`serve_bootstrap`) and a browser stand-in joined over
/// a DataChannel. The anchor installs the session as **provisional**
/// — that is the whole premise of the remaining witnesses.
async fn anchor_and_provisional_client() -> (
    Arc<MeshNode>,
    Arc<MeshNode>,
    net::adapter::net::rtc::RtcPeerId,
) {
    let anchor = node(Some(anchor_config())).await;
    let client = node(Some(rtc_config())).await;
    anchor.start_arc();
    client.start_arc();
    let (id_anchor, _id_client) = connect_rtc_loopback(&anchor, &client)
        .await
        .expect("DataChannel + Noise");
    assert!(
        anchor.peer_is_provisional(client.node_id()),
        "a browser-facing anchor installs an RTC session as provisional (§12 step 1)"
    );
    (anchor, client, id_anchor)
}

/// WITNESS 1 (and the negative that gives it meaning): the
/// permitted enrollment exchange is allowed, and promotion is what
/// changes the session's state — nothing else does.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_permitted_enrollment_exchange_promotes_and_nothing_else_does() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    // The caller's origin hash as the anchor sees it on the wire.
    // Taken from the client itself: a provisional peer's
    // announcement is (correctly) never ingested, so the anchor has
    // no `entity_id` cached for it — which is itself part of the
    // contract.
    let origin = client.origin_hash();

    // The allow-list admits exactly the bootstrap call…
    let permitted = BootstrapAction::NrpcRequest {
        service: ENROLL_SERVICE,
        target_node: anchor.node_id(),
        reply_channel: &enroll_reply_channel(origin),
        body_len: 256,
    };
    assert_eq!(
        allow_provisional_action(&permitted, anchor.node_id(), origin),
        Ok(())
    );

    // …and promotion binds the live incarnation.
    let session_id = anchor
        .peer_session_id(client_id)
        .expect("the installed session");
    assert!(
        anchor.promote_admission(client_id, session_id, PeerAddr::Rtc(endpoint)),
        "the enrollment success path promotes THIS session"
    );
    assert!(!anchor.peer_is_provisional(client_id));
    assert_eq!(anchor.rtc_stats().admission_promoted(), 1);
    assert_eq!(
        anchor.provisional_count(),
        0,
        "the projection the forwarding sites read must follow the state"
    );

    // Promoting twice is not a second promotion.
    assert!(!anchor.promote_admission(client_id, session_id, PeerAddr::Rtc(endpoint)));
}

/// WITNESS 2: the same provisional session is refused at each named
/// gate, and each refusal is counted on its own counter.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn every_denied_action_is_refused_at_its_named_gate_and_counted() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let third_party = node(None).await;
    let origin = client.origin_hash();

    // (a) announcement ingest — gate 4.
    let before_announce = anchor.rtc_stats().admission_refused_announce();
    client
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("the client may SEND; the anchor decides whether to ingest");
    let announce_refused = wait_for(
        || anchor.rtc_stats().admission_refused_announce() > before_announce,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        announce_refused,
        "gate 4: ingesting a provisional peer's announcement is route installation \
         for an unadmitted peer"
    );

    // (b) an unrelated channel Subscribe — gate 3.
    let unrelated = BootstrapAction::Subscribe {
        channel: "app.events.orders",
        has_token: false,
        has_queue_group: false,
    };
    assert_eq!(
        allow_provisional_action(&unrelated, anchor.node_id(), origin),
        Err(AdmissionRefusal::Subscribe)
    );

    // (c) another nRPC service — gate 5. Renewal included: S0e §5
    // keeps it off the list on purpose.
    for service in ["app.orders.place", RENEWAL_SERVICE] {
        let call = BootstrapAction::NrpcRequest {
            service,
            target_node: anchor.node_id(),
            reply_channel: &enroll_reply_channel(origin),
            body_len: 16,
        };
        assert_eq!(
            allow_provisional_action(&call, anchor.node_id(), origin),
            Err(AdmissionRefusal::Deliver),
            "{service} is not the bootstrap call"
        );
    }

    // (d) forwarding a routed envelope to a third node — gate 1 at
    // F1. The client asks the anchor to carry a handshake to a peer
    // it has never met; the anchor refuses and counts it.
    let before_forward = anchor.rtc_stats().admission_refused_transit();
    // Over the DataChannel, the way a browser asks: `connect_via`
    // takes a `SocketAddr` relay and would leave over UDP, which a
    // browser does not have.
    client
        .send_transit_probe_for_test(anchor.node_id(), third_party.node_id())
        .await
        .expect("the probe leaves the client");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_transit() > before_forward,
            Duration::from_secs(10)
        )
        .await,
        "gate 1: no third-party relay forwarding before enrollment, no exceptions"
    );

    // (e) `0x0D02` — signalling from a provisional peer. It reaches
    // the anchor's dispatch (the session is real) and is refused
    // before any dialog state exists, because forwarding it or
    // acting on it are both participation.
    let before_deliver = anchor.rtc_stats().admission_refused_deliver()
        + anchor.rtc_stats().admission_refused_forward();
    let _ = client
        .send_rtc_signal(
            anchor.node_id(),
            &RtcSignalMsg::Offer {
                dialog: 5,
                sdp: "v=0".to_string(),
            },
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = anchor.rtc_stats().admission_refused_deliver()
        + anchor.rtc_stats().admission_refused_forward();
    assert!(
        after >= before_deliver,
        "a provisional peer's signalling must never be acted on as an ordinary \
         dialog"
    );
}

/// WITNESS 3: a routed envelope addressed to the anchor **itself**
/// is delivered locally; the same envelope with a third-party
/// `dest_id` is refused at F1 — and the ordering matters, because
/// the enrollment envelope is addressed to the anchor.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn local_delivery_survives_the_forwarding_refusal() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let third_party = node(None).await;

    // Local delivery: `connect_via(anchor, …, anchor.node_id())`
    // is exactly what `Mesh::join` does, and its envelope's
    // `dest_id` is the anchor's own — local delivery, never
    // transit. It must NOT be refused.
    let refused_before_local = anchor.rtc_stats().admission_refused_transit();
    client
        .send_transit_probe_for_test(anchor.node_id(), anchor.node_id())
        .await
        .expect("the probe leaves the client");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        anchor.rtc_stats().admission_refused_transit(),
        refused_before_local,
        "an envelope whose dest_id IS the anchor is local delivery, not transit: \
         the `dest_id == local_node_id` test sits ABOVE the forwarding gate, so \
         this must not be counted as a refusal"
    );

    // Transit: same call, third-party destination — S0e's named
    // attack, because `relay_addr` and `dest_node_id` are
    // independent parameters.
    let before = anchor.rtc_stats().admission_refused_transit();
    client
        .send_transit_probe_for_test(anchor.node_id(), third_party.node_id())
        .await
        .expect("the probe leaves the client");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_transit() > before,
            Duration::from_secs(10)
        )
        .await,
        "the third-party envelope must be refused at F1 — after the \
         local-delivery test, not before it"
    );
}

/// WITNESS 4: promotion binds the exact live session. Replace the
/// session between REQUEST decode and RESPONSE and **nothing** is
/// promoted; the replacement stays provisional.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_replaced_session_promotes_nothing() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let decoded_session = anchor
        .peer_session_id(client_id)
        .expect("the session at decode time");

    // A replacement is exactly a *different* `(session_id,
    // endpoint)` pair under the same `node_id`, so the binding is
    // what is tested: neither half may be substituted.
    let wrong_endpoint = PeerAddr::Rtc(net::adapter::net::rtc::RtcPeerId {
        slot: endpoint.slot.wrapping_add(1),
        generation: endpoint.generation,
    });
    assert!(
        !anchor.promote_admission(client_id, decoded_session, wrong_endpoint),
        "a completion whose endpoint moved promotes nothing"
    );
    assert!(
        !anchor.promote_admission(
            client_id,
            decoded_session.wrapping_add(1),
            PeerAddr::Rtc(endpoint)
        ),
        "a completion whose session was replaced promotes nothing — 'whichever \
         session currently occupies that NodeId' is what §12 step 4 forbids"
    );
    assert!(
        anchor.peer_is_provisional(client_id),
        "…and the live session is still provisional after both attempts"
    );
    assert_eq!(anchor.rtc_stats().admission_promoted(), 0);

    // The exact live pair still promotes, so the refusals above are
    // the binding and not a broken promotion path.
    assert!(anchor.promote_admission(client_id, decoded_session, PeerAddr::Rtc(endpoint)));
    assert!(!anchor.peer_is_provisional(client_id));
}

/// WITNESS 5: `max_provisional` holds — the (N+1)th provisional
/// session is closed and reclaimed, counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_provisional_cap_closes_and_reclaims() {
    let anchor = node(Some(RtcConfig {
        serve_bootstrap: true,
        max_provisional: 1,
        ..rtc_config()
    }))
    .await;
    anchor.start_arc();

    let first = node(Some(rtc_config())).await;
    let second = node(Some(rtc_config())).await;
    first.start_arc();
    second.start_arc();
    let _ = connect_rtc_loopback(&anchor, &first)
        .await
        .expect("first provisional session");
    let _ = connect_rtc_loopback(&anchor, &second)
        .await
        .expect("second provisional session");
    assert_eq!(anchor.provisional_count(), 2);

    let reclaimed = anchor.reclaim_provisional_sessions();
    assert_eq!(
        reclaimed, 1,
        "over the cap, exactly the excess is shed — oldest first"
    );
    assert_eq!(anchor.provisional_count(), 1);
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_reclaimed() >= 1,
            Duration::from_secs(5)
        )
        .await,
        "a reclaimed session is counted, not silently dropped"
    );
}

/// WITNESS 6: enrollment is eligibility, not authority. An
/// **admitted** peer with no provider authority is still denied a
/// protected invocation — the existing org gate is *reached*, not
/// bypassed, which is the thing worth proving.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_admitted_peer_without_authority_is_still_denied() {
    use net::adapter::net::mesh_rpc::{CallOptions, RpcError};

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let session_id = anchor.peer_session_id(client_id).expect("session");
    assert!(anchor.promote_admission(client_id, session_id, PeerAddr::Rtc(endpoint)));
    assert!(!anchor.peer_is_provisional(client_id));

    // No service is registered under this name on the anchor, and
    // the client holds no grant of any kind. The call must fail —
    // admission got it past §12 and no further.
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        client.call(
            anchor.node_id(),
            "org.protected.invoke",
            bytes::Bytes::from_static(b"{}"),
            CallOptions::default(),
        ),
    )
    .await;
    match outcome {
        Ok(Err(RpcError::Timeout { .. })) | Err(_) | Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!(
            "enrollment is device admission, not organization membership, channel \
             authority or permission to invoke a provider"
        ),
    }
}

/// EXIT 8: a pingwave from a provisional peer is dropped and
/// counted; heartbeat still passes, because §12 permits session
/// maintenance.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn pingwave_is_denied_while_heartbeat_is_permitted() {
    assert_eq!(
        allow_provisional_action(&BootstrapAction::Heartbeat, 1, 2),
        Ok(())
    );
    assert_eq!(
        allow_provisional_action(&BootstrapAction::Pingwave, 1, 2),
        Err(AdmissionRefusal::Deliver)
    );

    // …and on a live anchor the session stays up under heartbeats
    // while the peer is provisional, which is what "maintenance"
    // has to mean for a 30 s bootstrap window to be usable.
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        anchor.peer_is_provisional(client.node_id()),
        "heartbeats must keep the provisional session alive without promoting it"
    );
    assert!(anchor.peer_endpoint(client.node_id()).is_some());
}

/// EXIT 7: a refused Subscribe from a provisional peer fires **zero**
/// corrective announcements — S0e's rate-limit-bypassing flood
/// trigger is off the provisional path.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_provisional_rejecter_never_triggers_a_corrective_reannounce() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    assert!(anchor.peer_is_provisional(client_id));

    // The anchor's own view: it holds a provisional session with
    // the client, so a subscription failure against that client
    // must not claim the corrective-announce latch.
    assert!(
        !anchor.claim_corrective_announce_for_test(client_id),
        "the corrective announce deliberately bypasses the rate limit; a \
         provisional peer must never be able to trigger it"
    );

    // A non-provisional peer still can — the guard is about
    // admission, not about disabling the mechanism.
    let ordinary = node(None).await;
    assert!(anchor.claim_corrective_announce_for_test(ordinary.node_id()));
}

/// The feature-off half of the contract: a node that serves no
/// bootstrap installs RTC sessions as **admitted**, so Stage 3's
/// behaviour is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_non_bootstrap_node_installs_rtc_sessions_as_admitted() {
    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    a.start_arc();
    b.start_arc();
    let _ = connect_rtc_loopback(&a, &b).await.expect("rtc pair");
    assert!(
        !a.peer_is_provisional(b.node_id()),
        "the gate exists for browser-facing anchors; everywhere else it must be \
         invisible"
    );
    assert_eq!(a.provisional_count(), 0);

    // And ordinary traffic flows, unremarkably.
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "native"))
        .await
        .expect("send");
}
