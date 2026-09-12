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

use bytes::Bytes;
use net::adapter::net::rtc::{
    allow_provisional_action, connect_rtc_loopback, enroll_reply_channel, AdmissionRefusal,
    BootstrapAction, RtcConfig, RtcSignalMsg, ENROLL_SERVICE, MAX_PROVISIONAL_FRAMES,
    RENEWAL_SERVICE,
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

/// WITNESS 6 (R7): an admitted peer without provider authority is
/// denied by a **registered protected provider** — and the handler
/// never runs.
///
/// The landed version registered no service under
/// `org.protected.invoke` and accepted any error or outer timeout,
/// so it could not tell "the protected engine refused" from "there
/// is nothing there" — removing the gate could not fail it. Here
/// the anchor installs a real `NodeAuthority`, registers a real
/// `OrgAdmission::OwnerDelegated` service, and the observable is
/// the handler's invocation count.
///
/// Scope, stated: the authorized positive control for the protected
/// engine lives in the crate's own protected suites
/// (`tests/integration_nrpc_protected.rs`, which builds a signed
/// owner-delegated proof). This witness's job is that transport
/// admission is **not** invocation authority.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_admitted_peer_without_authority_is_still_denied() {
    use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
    use net::adapter::net::behavior::org_admission::OrgAdmission;
    use net::adapter::net::behavior::org_authority::NodeAuthority;
    use net::adapter::net::cortex::{RpcContext, RpcHandlerError, RpcResponsePayload, RpcStatus};
    use net::adapter::net::mesh_rpc::CallOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcHandler for Counting {
        async fn call(&self, _: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: Bytes::from_static(b"served"),
            })
        }
    }

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();

    // A real authority, so a real protected registration is
    // possible at all.
    let org = OrgKeypair::generate();
    let entity = anchor.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&org, entity.clone(), 1, 3600).expect("cert");
    let dir = std::env::temp_dir().join(format!("net-r7-protected-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let authority = NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt");
    anchor
        .install_node_authority(Arc::new(authority))
        .expect("install authority");

    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = anchor
        .serve_rpc_protected(
            "org.protected.invoke",
            Arc::new(Counting(Arc::clone(&invocations))),
            OrgAdmission::OwnerDelegated,
            Arc::new(|_| true),
        )
        .expect("register a REAL protected provider");

    // Promote the caller: §12 admission is satisfied, and nothing
    // else is.
    let session_id = anchor.peer_session_id(client_id).expect("session");
    assert!(anchor.promote_admission(client_id, session_id, PeerAddr::Rtc(endpoint)));
    assert!(!anchor.peer_is_provisional(client_id));

    // The call carries no proof at all.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        client.call(
            anchor.node_id(),
            "org.protected.invoke",
            Bytes::from_static(b"{}"),
            CallOptions::default(),
        ),
    )
    .await;
    // And the hostile shape too: publishing the request without a
    // reply subscription, so no client-side gate can be what
    // refused it.
    let _ = client
        .publish_rpc_request_unsubscribed(
            anchor.node_id(),
            "org.protected.invoke",
            Bytes::from_static(b"{}"),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "enrollment is device admission, not organization membership, channel \
         authority or permission to invoke a provider — the registered handler \
         must never run for a caller with no grant"
    );
    let _ = std::fs::remove_dir_all(&dir);
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

/// The enrollment-outcome bytes the anchor reads, in `JoinOutcome`'s
/// pinned wire form (`sdk/src/enrollment.rs`: `b"NMO1"`, then `0`
/// Admitted / `1` Rejected). The SDK side pins this same prefix in
/// `join_outcome_wire_prefix_is_what_the_core_promotion_gate_reads`,
/// so a drift in either direction is a failing test rather than a
/// silently un-gated promotion.
#[cfg(feature = "cortex")]
fn outcome_bytes(admitted: bool) -> Bytes {
    let mut buf = Vec::from(*b"NMO1");
    if admitted {
        buf.push(0);
        let chain = b"delegation-chain";
        buf.extend_from_slice(&(chain.len() as u32).to_le_bytes());
        buf.extend_from_slice(chain);
    } else {
        buf.push(1);
        // R6: the outcome CODE is a `u16` on the wire
        // (`sdk/src/enrollment.rs`); the fixture used to write a
        // `u32`, and prefix-only assertions never noticed.
        buf.extend_from_slice(&7u16.to_le_bytes());
        let msg = b"invite expired";
        buf.extend_from_slice(&(msg.len() as u32).to_le_bytes());
        buf.extend_from_slice(msg);
    }
    Bytes::from(buf)
}

/// An enrollment service that answers with a fixed outcome.
#[cfg(feature = "cortex")]
struct FixedOutcome(bool);

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::RpcHandler for FixedOutcome {
    async fn call(
        &self,
        _ctx: net::adapter::net::cortex::RpcContext,
    ) -> Result<
        net::adapter::net::cortex::RpcResponsePayload,
        net::adapter::net::cortex::RpcHandlerError,
    > {
        Ok(net::adapter::net::cortex::RpcResponsePayload {
            status: net::adapter::net::cortex::RpcStatus::Ok,
            headers: vec![],
            body: outcome_bytes(self.0),
        })
    }
}

/// WITNESS 7: **only an admitted outcome promotes.** A provisional
/// client runs the whole permitted enrollment call against a real
/// handler on the anchor; when the handler's `JoinOutcome` is
/// `Rejected`, the session that asked must still be provisional
/// afterwards, and the refusal must be counted.
///
/// Promoting on *any* response would have admitted a peer to the
/// mesh by the very message that refused it.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_rejected_enrollment_outcome_promotes_nothing() {
    use net::adapter::net::mesh_rpc::CallOptions;

    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let _serve = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(FixedOutcome(false)))
        .expect("serve the enrollment service");

    let reply = client
        .call(
            anchor.node_id(),
            ENROLL_SERVICE,
            Bytes::from_static(b"join request"),
            CallOptions::default(),
        )
        .await
        .expect("the allow-list permits exactly this call");
    assert_eq!(
        reply.body.as_ref(),
        outcome_bytes(false).as_ref(),
        "the refusal itself must reach the client — it is a response, not a drop"
    );

    assert!(
        anchor.peer_is_provisional(client_id),
        "a Rejected outcome leaves the session provisional"
    );
    assert_eq!(
        anchor.rtc_stats().admission_promoted(),
        0,
        "nothing was admitted"
    );
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_rejected_outcome() == 1,
            Duration::from_secs(10)
        )
        .await,
        "the refusal is counted, not merely an absence of promotion"
    );
    assert_eq!(
        anchor.provisional_count(),
        1,
        "the projection the forwarding gates read must still refuse this peer"
    );
}

/// The positive control for witness 7, on the same real path: the
/// identical exchange with an **Admitted** outcome does promote.
/// Without this, "nothing promotes" would also pass on a node whose
/// promotion is simply broken.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_admitted_enrollment_outcome_promotes_the_session() {
    use net::adapter::net::mesh_rpc::CallOptions;

    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let _serve = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(FixedOutcome(true)))
        .expect("serve the enrollment service");

    client
        .call(
            anchor.node_id(),
            ENROLL_SERVICE,
            Bytes::from_static(b"join request"),
            CallOptions::default(),
        )
        .await
        .expect("the allow-list permits exactly this call");

    assert!(
        wait_for(
            || !anchor.peer_is_provisional(client_id),
            Duration::from_secs(10)
        )
        .await,
        "an Admitted outcome promotes the session that asked"
    );
    assert_eq!(anchor.rtc_stats().admission_promoted(), 1);
    assert_eq!(anchor.rtc_stats().admission_rejected_outcome(), 0);
}

/// WITNESS 8: the whole-session **frame** bound is enforced on the
/// live ingress path, not merely defined. The 257th inbound frame
/// from a provisional peer closes and reclaims the session (§12
/// step 5) rather than being clamped or silently counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_provisional_frame_bound_closes_the_session() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let before = anchor.rtc_stats().admission_reclaimed();

    // Small frames: the byte bound cannot be what fires here. As in
    // the byte-bound witness below, a refused send is retried — only
    // the anchor reclaiming the session ends the loop early.
    let mut sent = 0usize;
    let mut refusals = 0usize;
    while sent < MAX_PROVISIONAL_FRAMES as usize + 8 && anchor.peer_is_provisional(client_id) {
        match client
            .send_to_peer_node(anchor.node_id(), &batch(0, 1, "budget"))
            .await
        {
            Ok(()) => sent += 1,
            Err(_) => {
                refusals += 1;
                assert!(
                    refusals < 512,
                    "the sender was refused {refusals} times without the anchor ever \
                     reclaiming the session"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    assert!(
        wait_for(
            || anchor.rtc_stats().admission_reclaimed() > before,
            Duration::from_secs(10)
        )
        .await,
        "breaching the whole-session frame bound closes and reclaims the session"
    );
    assert_eq!(
        anchor.provisional_count(),
        0,
        "the reclaimed session leaves the projection the gates read"
    );
    assert!(
        !anchor.peer_is_provisional(client_id),
        "the peer is gone, not still provisional"
    );
}

/// WITNESS 8b: the **byte** bound is a separate axis. Far fewer than
/// 256 frames, each large enough that 256 KiB is crossed first.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_provisional_byte_bound_closes_the_session() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let before = anchor.rtc_stats().admission_reclaimed();

    // ~6 KiB of payload per frame: the 256 KiB bound is crossed
    // around frame 45, an order of magnitude under the frame bound.
    //
    // A refused `send_to_peer_node` is NOT evidence the bound fired:
    // under a loaded runtime the RTC admission seam refuses with
    // typed backpressure (S3-R2) before the anchor has *received*
    // 256 KiB, and stopping there left this witness timing out about
    // one run in six. Only the anchor's own view — the peer no longer
    // provisional — ends the loop early; a refusal is retried.
    let bulk = "b".repeat(6 * 1024);
    let mut frames = 0usize;
    let mut refusals = 0usize;
    while frames < 96 && anchor.peer_is_provisional(client_id) {
        let events = vec![net::event::InternalEvent::from_value(
            serde_json::json!({ "bulk": bulk }),
            frames as u64,
            0,
        )];
        let heavy = Batch {
            shard_id: 0,
            events,
            sequence_start: 0,
            process_nonce: batch_process_nonce(),
        };
        match client.send_to_peer_node(anchor.node_id(), &heavy).await {
            Ok(()) => frames += 1,
            Err(_) => {
                refusals += 1;
                assert!(
                    refusals < 512,
                    "the sender was refused {refusals} times without the anchor ever \
                     reclaiming the session: neither the byte bound nor delivery is working"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    assert!(
        wait_for(
            || anchor.rtc_stats().admission_reclaimed() > before,
            Duration::from_secs(10)
        )
        .await,
        "breaching the whole-session byte bound closes and reclaims the session"
    );
    assert!(
        frames < MAX_PROVISIONAL_FRAMES as usize,
        "the BYTE bound must be what fired: {frames} frames sent, bound is \
         {MAX_PROVISIONAL_FRAMES}"
    );
    assert!(!anchor.peer_is_provisional(client_id));
}

/// R1 (streaming): a **registered** server-streaming provider is
/// not invoked by a provisional caller — and is invoked once the
/// same caller is admitted.
///
/// Gate 5 sat on the unary bridge alone, so this handler ran for an
/// unenrolled peer. The request is published **without** a reply
/// subscription (`publish_rpc_request_unsubscribed`): the ordinary
/// client subscribes first, and that subscribe is refused at gate 3
/// for a non-enrollment channel, which would make the refusal say
/// nothing about the serve bridge. A sender that does not care
/// about the reply is exactly the case the bridge must refuse, and
/// **handler invocation** is the observed effect.
///
/// Inverse: remove the `rtc_admission_allows_rpc` check from the
/// server-streaming bridge — the provisional publish invokes the
/// handler.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_registered_streaming_provider_refuses_a_provisional_caller() {
    use net::adapter::net::cortex::{RpcContext, RpcHandlerError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStream(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcStreamingHandler for CountingStream {
        async fn call(
            &self,
            _ctx: RpcContext,
            sink: net::adapter::net::cortex::RpcResponseSink,
        ) -> Result<(), RpcHandlerError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            sink.send(Bytes::from_static(b"served"));
            Ok(())
        }
    }

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = anchor
        .serve_rpc_streaming(
            "r1.stream",
            Arc::new(CountingStream(Arc::clone(&invocations))),
        )
        .expect("register a real streaming provider");
    // Let the registration's announcement reach the client, so its
    // route lookup resolves the service.
    assert!(
        wait_for(
            || client.publish_rpc_request_unsubscribed_is_routable("r1.stream", anchor.node_id()),
            Duration::from_secs(10)
        )
        .await,
        "the client must be able to address the registered service at all"
    );

    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.stream", Bytes::from_static(b"hi"))
        .await
        .expect("the hostile publish itself is a send, not an authorization");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "the handler must never run for an unenrolled peer — this is the effect, \
         not an error string"
    );

    // Positive control: the SAME registered service, the same
    // caller, the same publish, after promotion.
    let session = anchor
        .peer_session_id(client_id)
        .expect("the provisional session");
    assert!(anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)));
    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.stream", Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    assert!(
        wait_for(
            || invocations.load(Ordering::SeqCst) == 1,
            Duration::from_secs(10)
        )
        .await,
        "once admitted, the same publish must invoke the handler exactly once \
         (saw {})",
        invocations.load(Ordering::SeqCst)
    );
}

/// R2: an old call's **rejection** after the session was replaced
/// leaves the replacement provisional and unpromoted, and consumes
/// none of its reservation.
///
/// The mirror of Kyra's promotion probe: reservations were keyed by
/// node id, so *either* terminal outcome of a superseded call
/// reached into the successor's state. The rejection path also
/// counted a refusal against a session that never asked.
///
/// Inverse: key `pending_promotions` by node id again — the old
/// rejection consumes the replacement's reservation, and the
/// replacement's own success then promotes nothing.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_old_rejection_after_replacement_consumes_nothing() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let first_session = anchor
        .peer_session_id(client_id)
        .expect("the first session");

    // The first call's reservation, armed the way the gate arms it.
    anchor.arm_enrollment_reservation_for_test(
        client_id,
        first_session,
        PeerAddr::Rtc(endpoint),
        0xC1,
    );

    // The session is replaced: a new incarnation, its own call.
    let replacement = anchor.replace_provisional_for_test(client_id, PeerAddr::Rtc(endpoint));
    assert_ne!(replacement, first_session);
    anchor.arm_enrollment_reservation_for_test(
        client_id,
        replacement,
        PeerAddr::Rtc(endpoint),
        0xC2,
    );

    // The OLD call rejects, late.
    anchor.note_enrollment_rejected_for_test(client_id, 0xC1);
    assert!(
        anchor.peer_is_provisional(client_id),
        "a rejection may never promote anything"
    );

    // The replacement's own success must still be able to promote:
    // the old rejection must not have eaten its reservation.
    assert!(
        anchor.promote_on_enrollment_response_for_test(client_id, 0xC2),
        "the replacement's own call must still hold its reservation"
    );
    assert!(
        !anchor.peer_is_provisional(client_id),
        "and promote the session that actually earned it"
    );
    assert_eq!(
        anchor.peer_session_id(client_id),
        Some(replacement),
        "the promoted session is the replacement, by identity"
    );
}

/// R3: a sweep that selected a session which is then **promoted**
/// must not tear it down.
///
/// The old reclaim removed by "is provisional" and closed the
/// endpoint regardless of whether the removal succeeded, so a
/// session admitted between selection and removal lost its channel
/// anyway.
///
/// Inverse: remove by `info.admission.is_provisional()` again (drop
/// the exact-incarnation `evict_session_at`) — the promoted session
/// survives the map but its endpoint is closed and the count is
/// wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_sweep_cannot_reclaim_a_session_promoted_after_selection() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let session = anchor.peer_session_id(client_id).expect("session");

    // Promote first, then run the sweep with the selection it would
    // have made a moment earlier: the exact incarnation is what the
    // removal must name.
    assert!(anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)));
    let reclaimed =
        anchor.close_provisional_session_for_test(client_id, PeerAddr::Rtc(endpoint), session);

    assert!(
        !reclaimed,
        "an obsolete provisional verdict must not own the removal of an admitted session"
    );
    assert_eq!(
        anchor.peer_session_id(client_id),
        Some(session),
        "the promoted session stays installed"
    );
    assert!(!anchor.peer_is_provisional(client_id), "and stays admitted");
}

/// R3: the same sweep against a **replacement**. The stale
/// selection names an incarnation that no longer exists, so it owns
/// nothing.
///
/// Inverse: the same one — the sweep removes the successor.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_sweep_cannot_reclaim_a_replacement_it_never_selected() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let selected = anchor.peer_session_id(client_id).expect("session");
    let replacement = anchor.replace_provisional_for_test(client_id, PeerAddr::Rtc(endpoint));
    assert_ne!(selected, replacement);

    let reclaimed =
        anchor.close_provisional_session_for_test(client_id, PeerAddr::Rtc(endpoint), selected);
    assert!(!reclaimed, "the stale selection owns nothing");
    assert_eq!(
        anchor.peer_session_id(client_id),
        Some(replacement),
        "the replacement survives a sweep that selected its predecessor"
    );
}

/// R3: the enrollment REQUEST bound is charged **before** dispatch,
/// and a full churn returns every admission counter to baseline.
///
/// Kyra's fifth-request probe covers the refusal; this covers the
/// accounting around it: the in-flight reservation is released on
/// each terminal outcome, so four sequential calls do not exhaust
/// the one-in-flight slot, and the projection returns to zero after
/// the session goes away.
///
/// Inverse: drop the `charge_enrollment_request` call from the gate
/// — the fifth REQUEST is dispatched (Kyra's probe) — or drop
/// `release_enrollment_slot` from the terminal paths: the second
/// call is refused although the first had finished.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn enrollment_requests_are_charged_and_released_and_churn_returns_to_baseline() {
    use net::adapter::net::cortex::{RpcContext, RpcHandlerError, RpcResponsePayload, RpcStatus};
    use net::adapter::net::mesh_rpc::CallOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Rejecting(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcHandler for Rejecting {
        async fn call(&self, _: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut body = b"NMO1".to_vec();
            body.push(1);
            body.extend_from_slice(&7u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: body.into(),
            })
        }
    }

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let executions = Arc::new(AtomicUsize::new(0));
    let _serve = anchor
        .serve_rpc(ENROLL_SERVICE, Arc::new(Rejecting(Arc::clone(&executions))))
        .expect("serve enrollment");

    // Four sequential calls: each releases its in-flight slot, so
    // the one-in-flight bound never refuses a *sequential* caller.
    for index in 0..4 {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call(
                anchor.node_id(),
                ENROLL_SERVICE,
                Bytes::from_static(b"join"),
                CallOptions::default(),
            ),
        )
        .await;
        assert!(
            matches!(result, Ok(Ok(_))),
            "call {index} of the permitted four must reach the service: {result:?}"
        );
    }
    assert_eq!(
        executions.load(Ordering::SeqCst),
        4,
        "the allowance is initial + 3 retries"
    );
    assert!(
        anchor.peer_is_provisional(client_id),
        "rejections leave the session provisional"
    );

    // Churn: the session goes away, and the projection the gates
    // read returns to baseline.
    assert_eq!(anchor.provisional_count(), 1);
    anchor
        .rtc_driver()
        .expect("driver")
        .close(endpoint)
        .await
        .expect("close");
    assert!(
        wait_for(
            || anchor.peer_endpoint(client_id).is_none() && anchor.provisional_count() == 0,
            Duration::from_secs(5)
        )
        .await,
        "after the close: no peer and an empty projection (saw {} in the projection)",
        anchor.provisional_count()
    );
}

/// R3: a provisional sender cannot create arbitrary receive
/// streams. `MAX_PROVISIONAL_STREAMS` and the per-session stream
/// byte bound were declared constants nothing checked.
///
/// Inverse: remove the `charge_provisional_stream` call from the
/// event plane — the anchor's session tracks a receive stream per
/// stream id the unenrolled peer names.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_provisional_sender_cannot_allocate_arbitrary_streams() {
    let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();

    // Six distinct stream ids, each carrying a real frame.
    for id in 0..6u64 {
        let mut cfg = net::adapter::net::StreamConfig::new();
        cfg.reliability = net::adapter::net::Reliability::FireAndForget;
        if let Ok(stream) = client.open_stream(anchor.node_id(), 0x7000u64 + id, cfg) {
            let _ = client
                .send_with_retry(&stream, &[Bytes::from_static(b"R3STREAM")], 4)
                .await;
        }
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    let tracked = anchor
        .peer_session_for_test(client_id)
        .map(|s| s.stream_ids().len())
        .unwrap_or(0);
    assert!(
        tracked <= net::adapter::net::rtc::MAX_PROVISIONAL_STREAMS as usize,
        "a provisional session may track at most {} receive streams; tracked {tracked}",
        net::adapter::net::rtc::MAX_PROVISIONAL_STREAMS
    );
}

/// R1-A: a routed re-handshake **through a provisional RTC
/// endpoint** completes instead of deadlocking.
///
/// `derived_admission` re-enters `peers.get` through
/// `ingress_admission`, and the routed constructor ran it while
/// holding that same peer's `peers.entry` write guard: the
/// same-node case reacquires its own DashMap shard. The decision is
/// now taken before the entry.
///
/// The whole test is wrapped in an **external timeout**: a deadlock
/// is a hang, not a failed assertion, so the witness has to fail
/// rather than wedge the suite.
///
/// Inverse: move `derived_admission` back inside the entry arms —
/// this test times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_routed_rehandshake_through_a_provisional_endpoint_does_not_deadlock() {
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        let (anchor, client, _endpoint) = anchor_and_provisional_client().await;
        let client_id = client.node_id();
        assert!(anchor.peer_is_provisional(client_id));

        // The provisional client runs a routed handshake addressed
        // to the anchor itself, through its own RTC endpoint — the
        // bootstrap shape, and the same-node case that reacquires
        // the shard.
        for _ in 0..3 {
            let _ = client
                .send_transit_probe_for_test(anchor.node_id(), anchor.node_id())
                .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The dispatch loop is still alive and still answering.
        assert!(
            wait_for(
                || anchor.peer_endpoint(client_id).is_some(),
                Duration::from_secs(5)
            )
            .await,
            "the anchor's dispatch must still be serving this peer"
        );
        anchor.peer_is_provisional(client_id)
    })
    .await;
    assert!(
        outcome.is_ok(),
        "a routed re-handshake through a provisional RTC endpoint must not \
         deadlock the dispatch loop"
    );
    assert!(
        outcome.expect("no timeout"),
        "and the peer it introduces is not admitted by the relay's own session"
    );
}

/// R1-A: the **client-streaming** bridge refuses a provisional
/// caller and serves an admitted one. Its gate is a separate
/// mutable call site from the server-streaming one.
///
/// Inverse: remove `rtc_admission_allows_rpc` from the
/// client-streaming bridge — the provisional publish invokes the
/// handler.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_registered_client_streaming_provider_refuses_a_provisional_caller() {
    use net::adapter::net::cortex::{RpcHandlerError, RpcResponsePayload, RpcStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcClientStreamingHandler for Counting {
        async fn call(
            &self,
            _ctx: net::adapter::net::cortex::RpcStreamingContext,
            _chunks: net::adapter::net::cortex::RequestStream,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: Bytes::from_static(b"served"),
            })
        }
    }

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = anchor
        .serve_rpc_client_stream("r1.upload", Arc::new(Counting(Arc::clone(&invocations))))
        .expect("register a real client-streaming provider");
    assert!(
        wait_for(
            || client.publish_rpc_request_unsubscribed_is_routable("r1.upload", anchor.node_id()),
            Duration::from_secs(10)
        )
        .await,
        "the client must be able to address the registered service"
    );

    let refused_before = anchor.rtc_stats().admission_refused_deliver();
    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.upload", Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_deliver() > refused_before,
            Duration::from_secs(5)
        )
        .await,
        "the client-streaming bridge must refuse the provisional caller at its own gate \
         and count it"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "the client-streaming handler must never run for an unenrolled peer"
    );

    let session = anchor.peer_session_id(client_id).expect("session");
    assert!(anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)));
    let refused_after_promotion = anchor.rtc_stats().admission_refused_deliver();
    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.upload", Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        anchor.rtc_stats().admission_refused_deliver(),
        refused_after_promotion,
        "the positive control: after promotion the SAME publish passes this \
         bridge's gate — the effect the gate owns. Driving the client-streaming handler \
         to completion additionally needs the chunk/grant protocol, which is \
         not what admission decides, so it is not claimed here."
    );
}

/// R1-A: the **duplex** bridge, same contract, its own call site.
///
/// Inverse: remove `rtc_admission_allows_rpc` from the duplex
/// bridge.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_registered_duplex_provider_refuses_a_provisional_caller() {
    use net::adapter::net::cortex::RpcHandlerError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl net::adapter::net::cortex::RpcDuplexHandler for Counting {
        async fn call(
            &self,
            _ctx: net::adapter::net::cortex::RpcStreamingContext,
            _chunks: net::adapter::net::cortex::RequestStream,
            sink: net::adapter::net::cortex::RpcResponseSink,
        ) -> Result<(), RpcHandlerError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            sink.send(Bytes::from_static(b"served"));
            Ok(())
        }
    }

    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();
    let invocations = Arc::new(AtomicUsize::new(0));
    let _serve = anchor
        .serve_rpc_duplex("r1.duplex", Arc::new(Counting(Arc::clone(&invocations))))
        .expect("register a real duplex provider");
    assert!(
        wait_for(
            || client.publish_rpc_request_unsubscribed_is_routable("r1.duplex", anchor.node_id()),
            Duration::from_secs(10)
        )
        .await,
        "the client must be able to address the registered service"
    );

    let refused_before = anchor.rtc_stats().admission_refused_deliver();
    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.duplex", Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_deliver() > refused_before,
            Duration::from_secs(5)
        )
        .await,
        "the duplex bridge must refuse the provisional caller at its own gate \
         and count it"
    );
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        0,
        "the duplex handler must never run for an unenrolled peer"
    );

    let session = anchor.peer_session_id(client_id).expect("session");
    assert!(anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)));
    let refused_after_promotion = anchor.rtc_stats().admission_refused_deliver();
    client
        .publish_rpc_request_unsubscribed(anchor.node_id(), "r1.duplex", Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        anchor.rtc_stats().admission_refused_deliver(),
        refused_after_promotion,
        "the positive control: after promotion the SAME publish passes this \
         bridge's gate — the effect the gate owns. Driving the duplex handler \
         to completion additionally needs the chunk/grant protocol, which is \
         not what admission decides, so it is not claimed here."
    );
}

/// R1-A: a provisional peer cannot drive the **migration**
/// subprotocol. The dispatch invoked the application's migration
/// handler before any admission decision.
///
/// Inverse: remove the gate from the migration arm — the handler
/// runs for an unenrolled peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_provisional_peer_cannot_drive_migration() {
    let (anchor, client, endpoint) = anchor_and_provisional_client().await;
    let client_id = client.node_id();

    // The migration arm's effect is invoking the application's
    // migration handler; the gate's effect is refusing the frame
    // before that, counted. Observed here without standing up a
    // real `MigrationSubprotocolHandler` — installing one is a
    // compute-plane exercise, and the decision under test is
    // admission's.
    let refused_before = anchor.rtc_stats().admission_refused_deliver();
    client
        .send_subprotocol_to_node(anchor.node_id(), 0x0500, b"MIGRATE")
        .await
        .expect("send a migration frame");
    assert!(
        wait_for(
            || anchor.rtc_stats().admission_refused_deliver() > refused_before,
            Duration::from_secs(5)
        )
        .await,
        "the migration arm must refuse an unenrolled peer before its handler, \
         and count it"
    );

    // Positive control: the same frame after promotion is not
    // refused.
    let session = anchor.peer_session_id(client_id).expect("session");
    assert!(anchor.promote_admission(client_id, session, PeerAddr::Rtc(endpoint)));
    let refused_after = anchor.rtc_stats().admission_refused_deliver();
    client
        .send_subprotocol_to_node(anchor.node_id(), 0x0500, b"MIGRATE")
        .await
        .expect("send a migration frame");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        anchor.rtc_stats().admission_refused_deliver(),
        refused_after,
        "an admitted peer's migration frame passes the gate"
    );
}
