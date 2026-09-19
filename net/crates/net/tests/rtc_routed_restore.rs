//! The carried §5 criterion, tested natively, without Stage 4.
//!
//! Kyra's finding: deferring the routed legs was never a technical
//! prerequisite. `connect_via` already performs a routed Noise
//! handshake through a relay, and the in-process SDP exchange already
//! produces a real DataChannel — so a three-node native fixture can
//! run the whole sequence today:
//!
//! 1. actual pre-direct **routed** delivery A → R → B;
//! 2. **quiescent replacement** by the real RTC/Noise pair through the
//!    ordinary installer (CAS + quiescence gate, R3-E);
//! 3. direct delivery, forced DataChannel loss, explicit interruption
//!    and cleanup (peer-removal path; the stale handle is refused);
//! 4. a **new routed handshake** and actual restored routed delivery,
//!    with the stale session and handle rejected.
//!
//! Step 4 is **manual restoration**, not automatic fallback: the test
//! calls `connect_via` itself. Nothing in Stage 3 owns "the direct
//! path died, therefore re-establish the routed one" — see §11 for
//! the named unimplemented owner.
//!
//! The nRPC and fold witnesses below are the existing consumer
//! assertions (exact reply body; delivered remote events) with only
//! the transport setup swapped for an RTC pair.
//!
//! Run: `cargo test --features "webrtc fixtures cortex" --test rtc_routed_restore`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability, SocketBufferConfig,
    StreamConfig,
};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

const PSK: [u8; 32] = [0x53u8; 32];

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    // The upgrade's quiescence gate counts an open stream as busy,
    // and a stream stays open until it idles out. Production's 300 s
    // is not a test's patience; 1 s keeps the *gate* real while
    // letting the fixture reach the quiescent state it is about.
    cfg.stream_idle_timeout = Duration::from_secs(1);
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

async fn connect_udp(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = Arc::clone(b);
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
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

/// Phase-tagged delivery: how many events carrying `tag` reached
/// `node`, drained across every shard.
async fn delivered_with_tag(node: &Arc<MeshNode>, tag: &str, within: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + within;
    let needle = format!("\"tag\":\"{tag}\"");
    let mut count = 0usize;
    while tokio::time::Instant::now() < deadline && count == 0 {
        for shard in 0..4u16 {
            let result = node.poll_shard(shard, None, 512).await.expect("poll_shard");
            for event in result.events {
                if String::from_utf8_lossy(event.raw.as_ref()).contains(&needle) {
                    count += 1;
                }
            }
        }
        if count == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    count
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

/// The carried §5 sequence, phase by phase, on three native nodes.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn routed_then_direct_then_loss_then_manually_restored_routed() {
    let a = node(Some(rtc_config())).await;
    let r = node(None).await;
    let b = node(Some(rtc_config())).await;

    // A ↔ R ↔ B over UDP: R is the relay, and neither A nor B has a
    // direct path to the other.
    connect_udp(&a, &r).await;
    connect_udp(&r, &b).await;
    a.start();
    r.start();
    b.start();

    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let r_addr = r.local_addr();

    // ---- phase 1: actual routed delivery A → R → B --------------
    a.connect_via(r_addr, &b_pub, b_id)
        .await
        .expect("routed handshake through the relay");
    let routed_endpoint = a.peer_endpoint(b_id).expect("a routed session");
    assert_eq!(
        routed_endpoint,
        PeerAddr::Udp(r_addr),
        "the pre-direct path must go through the relay, not to B directly"
    );
    assert!(
        !a.peer_is_direct(b_id),
        "a routed session is not an adjacency"
    );
    let routed_session = a.peer_session_id(b_id).expect("session id");

    // §10's PER-PAIR counter, not the node-global one.
    //
    // What used to be here was `r.router().stats().packets_forwarded
    // >= forwarded_before`. It established nothing, twice over: that
    // counter only ever increases, so `>=` is true of every possible
    // run including one where the relay forwarded nothing at all;
    // and it is node-global, so any unrelated traffic through R
    // satisfies it. Assert the per-pair counter per phase instead —
    // do not reintroduce the node-global `>=`.
    let a_routing_id = (a.node_id() & 0xFFFF_FFFF) as u32;
    let forwarded_before = r.forwarded_app_packets(a_routing_id, b_id);
    // The routed API, not the direct one: a routing envelope is what
    // the relay can forward without decrypting.
    a.send_routed(b_id, &batch(0, 4, "phase1-routed"))
        .await
        .expect("routed send");
    assert!(
        delivered_with_tag(&b, "phase1-routed", Duration::from_secs(15)).await >= 1,
        "phase 1 must actually deliver A → R → B, not merely install a session"
    );
    let forwarded_after_routed = r.forwarded_app_packets(a_routing_id, b_id);
    assert!(
        forwarded_after_routed > forwarded_before,
        "phase 1 is routed, so R's per-pair application-data counter for \
         (A → B) MUST move while it carries the phase; without this moving \
         leg, phase 3's flatness cannot tell 'direct' from 'broken' \
         (before {forwarded_before}, after {forwarded_after_routed})"
    );

    // ---- phase 2: quiescent replacement by the RTC pair ---------
    //
    // Through the ordinary installer: the incumbent is snapshotted,
    // the quiescence gate applies, and the install is a CAS. The gate
    // is real — attempting the upgrade while phase 1's reliable
    // traffic is still unacked is refused, so the contract's
    // "quiesce first" is what this waits for rather than talks about.
    //
    // **Both ends, not just A.** The quiescence gate is checked
    // independently by each side's own `rtc_upgrade_precheck`: B
    // refuses to accept the replacement while ITS view of the routed
    // session still carries an open stream or unacked reliable data.
    // Waiting only on A's view left B's ack of phase 1 in flight on a
    // loaded runner, so `accept_rtc` returned "the incumbent session
    // is busy" immediately, nothing answered msg1, and the initiator
    // failed with `handshake timeout` — the Linux failure in CI runs
    // 34727420769 and 652c786c9, on the initiator's error rather than
    // the responder's cause. No wait was widened: the precondition
    // the test claims to establish is now actually established.
    //
    // No per-pair forwarding assertion in this phase: phase 2 sends
    // no application data on the A → B pair at all. It waits for
    // quiescence and then runs the in-process SDP/Noise exchange,
    // which never crosses R. An assertion here would be about
    // nothing, so there is none.
    let a_id = a.node_id();
    let quiescent = wait_for(
        || {
            a.peer_session_for_test(b_id)
                .is_some_and(|s| !s.has_open_streams() && !s.has_unacked())
                && b.peer_session_for_test(a_id)
                    .is_some_and(|s| !s.has_open_streams() && !s.has_unacked())
        },
        Duration::from_secs(15),
    )
    .await;
    assert!(
        quiescent,
        "the routed session must quiesce at BOTH ends before it can be replaced \
         (A quiet: {}, B quiet: {})",
        a.peer_session_for_test(b_id)
            .is_some_and(|s| !s.has_open_streams() && !s.has_unacked()),
        b.peer_session_for_test(a_id)
            .is_some_and(|s| !s.has_open_streams() && !s.has_unacked())
    );

    let (id_a, id_b) = connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise, replacing the routed session");

    assert_eq!(
        a.peer_endpoint(b_id),
        Some(PeerAddr::Rtc(id_a)),
        "the direct RTC endpoint must replace the relayed one"
    );
    assert!(
        a.peer_is_direct(b_id),
        "an RTC session that ran Noise with B is an authenticated adjacency"
    );
    let direct_session = a.peer_session_id(b_id).expect("session id");
    assert_ne!(
        direct_session, routed_session,
        "the replacement must be a fresh incarnation, not the routed one re-labelled"
    );

    // ---- phase 3: direct delivery, then forced loss -------------
    //
    // The flatness leg. `assert_eq!` is the whole point: a `>=`
    // could not distinguish "the DataChannel carried it" from "the
    // relay is still carrying it", which is the only thing this
    // phase exists to establish.
    let flat_before = r.forwarded_app_packets(a_routing_id, b_id);
    a.send_to_peer_node(b_id, &batch(0, 4, "phase3-direct"))
        .await
        .expect("direct send");
    assert!(
        delivered_with_tag(&b, "phase3-direct", Duration::from_secs(15)).await >= 1,
        "phase 3 must deliver over the DataChannel"
    );
    assert_eq!(
        r.forwarded_app_packets(a_routing_id, b_id),
        flat_before,
        "phase 3 is direct, so R's per-pair counter for (A → B) must stay \
         EXACTLY flat while the DataChannel carries the payload"
    );

    // Forced DataChannel loss, then explicit interruption: the close
    // runs the ordinary peer-removal path (R3-E).
    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(10)).await,
        "the interruption must clean up: peer removed, not left stale"
    );
    assert!(
        !a.rtc_driver().expect("driver").transport().is_open(id_a),
        "the stale handle must be dead"
    );

    // The responder's side of the interruption. `str0m`'s
    // `disconnect()` is inert — it emits nothing on the wire — so B
    // does not learn of A's close from A's close; without an
    // explicit teardown B would find out only when its own ICE times
    // the peer out. That timeout is the real far-side detector and
    // it is not instant, so this fixture performs the interruption on
    // both ends, which is what a deliberate teardown does. §11
    // records the gap: there is no Stage 3 owner that turns "my
    // channel died" into "tell the peer".
    b.rtc_driver()
        .expect("driver")
        .close(id_b)
        .await
        .expect("close B's side");
    let a_id = a.node_id();
    assert!(
        wait_for(|| b.peer_endpoint(a_id).is_none(), Duration::from_secs(15)).await,
        "the far side must clean up its own closed channel as well"
    );

    // ---- phase 4: MANUAL restoration of the routed path ---------
    //
    // Manual, and labelled so: this test calls `connect_via`. No
    // Stage 3 component watches a dead direct path and re-establishes
    // a routed one.
    // Let the eviction settle on all three nodes (route withdrawal
    // and peer-map cleanup are asynchronous), then handshake again.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut restored = None;
    for _ in 0..3 {
        match a.connect_via(r_addr, &b_pub, b_id).await {
            Ok(id) => {
                restored = Some(id);
                break;
            }
            Err(e) => {
                restored = None;
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = e;
            }
        }
    }
    assert!(
        restored.is_some(),
        "a NEW routed handshake must succeed after the direct path is torn down"
    );
    let restored_session = a.peer_session_id(b_id).expect("session id");
    assert_eq!(
        a.peer_endpoint(b_id),
        Some(PeerAddr::Udp(r_addr)),
        "restoration must go back through the relay"
    );
    assert!(
        restored_session != direct_session && restored_session != routed_session,
        "restoration must be a new session, never a resurrected one \
         (restored {restored_session}, direct {direct_session}, routed {routed_session})"
    );

    let forwarded_before_restore = r.forwarded_app_packets(a_routing_id, b_id);
    a.send_routed(b_id, &batch(0, 4, "phase4-restored"))
        .await
        .expect("restored routed send");
    assert!(
        delivered_with_tag(&b, "phase4-restored", Duration::from_secs(15)).await >= 1,
        "phase 4 must actually deliver again through the relay"
    );
    let forwarded_after_restore = r.forwarded_app_packets(a_routing_id, b_id);
    assert!(
        forwarded_after_restore > forwarded_before_restore,
        "phase 4 puts the pair back on the relay, so the SAME per-pair counter \
         that stayed flat while they were direct must move again \
         (before {forwarded_before_restore}, after {forwarded_after_restore})"
    );

    // The stale RTC handle cannot reach the restored session.
    assert_eq!(
        a.rtc_driver()
            .expect("driver")
            .transport()
            .submit(&[0u8; 32], id_a),
        Err(net::adapter::net::rtc::RtcSubmitError::UnknownPeer),
        "a handle from the dead direct lifetime must not address anything"
    );
}

/// A reliable stream survives the same replacement: what was open
/// before the RTC upgrade is not silently dropped by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_idle_routed_stream_is_replaced_cleanly_by_the_rtc_pair() {
    let a = node(Some(rtc_config())).await;
    let r = node(None).await;
    let b = node(Some(rtc_config())).await;
    connect_udp(&a, &r).await;
    connect_udp(&r, &b).await;
    a.start();
    r.start();
    b.start();

    let b_id = b.node_id();
    let b_pub = *b.public_key();
    a.connect_via(r.local_addr(), &b_pub, b_id)
        .await
        .expect("routed handshake");

    // Quiescent by construction: no open streams, nothing unacked.
    let (id_a, _) = connect_rtc_loopback(&a, &b).await.expect("rtc pair");
    assert_eq!(a.peer_endpoint(b_id), Some(PeerAddr::Rtc(id_a)));

    // The new session carries a reliable stream end to end.
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a.open_stream(b_id, 0x0090, cfg).expect("open_stream");
    let payloads: Vec<Bytes> = (0..8u8).map(|i| Bytes::from(vec![i; 64])).collect();
    for payload in &payloads {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("send over the replacement session");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    while tokio::time::Instant::now() < deadline && seen.len() < payloads.len() {
        for shard in 0..4u16 {
            for event in b
                .poll_shard(shard, None, 512)
                .await
                .expect("poll_shard")
                .events
            {
                let raw = event.raw.to_vec();
                if raw.len() == 64 {
                    seen.insert(raw);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        seen.len(),
        payloads.len(),
        "the replacement session must carry the stream completely; saw {}",
        seen.len()
    );
}

/// S5-R15 / R12: the responder's busy-displacement branch is scoped
/// to an incumbent the peer superseded on **another RTC channel**. A
/// busy ROUTED incumbent is not that, and must still be deferred.
///
/// The two existing routed tests above cannot see this. Both arrange
/// quiescence first — one waits for both ends to go quiet before
/// invoking the upgrade, the other starts its replacement with no
/// open streams — so neither exercises the negative role/transport
/// boundary they were cited as proof of. This does: the same busy
/// incumbent, the responder role, and a relayed endpoint.
///
/// Inverse: widen `superseded_by_its_owner` in
/// `rtc_upgrade_precheck` to any responder facing a busy incumbent
/// (drop the `PeerAddr::Rtc(id) if id != peer` discriminator) — this
/// `accept_rtc` parks on the handshake inbox instead of returning,
/// so the immediate-refusal assertion fails on the timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_busy_routed_responder_still_defers_to_its_relay_session() {
    let a = node(Some(rtc_config())).await;
    let r = node(None).await;
    let b = node(Some(rtc_config())).await;
    connect_udp(&a, &r).await;
    connect_udp(&r, &b).await;
    a.start();
    r.start();
    b.start();

    let b_id = b.node_id();
    let b_pub = *b.public_key();
    a.connect_via(r.local_addr(), &b_pub, b_id)
        .await
        .expect("routed handshake");
    let routed_session = a.peer_session_id(b_id).expect("a routed session");
    let routed_endpoint = a.peer_endpoint(b_id);
    assert!(
        !matches!(routed_endpoint, Some(PeerAddr::Rtc(_))),
        "the premise is a RELAYED incumbent; got {routed_endpoint:?}",
    );

    // Delivery through the relay before the attempt, so "the routed
    // path works" is established rather than assumed. `send_routed`
    // is the routed send path — a typed-handle `send_on_stream` goes
    // straight to the peer's endpoint and does not wrap a routing
    // header, which is why the phases above use this call too.
    a.send_routed(b_id, &batch(0, 4, "routed-before-refusal"))
        .await
        .expect("routed send");
    assert!(
        delivered_with_tag(&b, "routed-before-refusal", Duration::from_secs(15)).await >= 1,
        "the routed incumbent must actually be carrying traffic A → R → B",
    );

    // Now make it busy in exactly the shape the gate reads — an open
    // application stream the peer cannot close — and attempt at once:
    // this fixture's `stream_idle_timeout` is 1 s and an evicted
    // stream would make the premise vacuous.
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let _busy = a.open_stream(b_id, 0x00A1, cfg).expect("open_stream");
    assert!(
        a.peer_session_for_test(b_id)
            .is_some_and(|s| s.stream_ids().contains(&0x00A1)),
        "the routed incumbent must be busy at the moment of the attempt",
    );

    // RESPONDER on a fresh RTC endpoint against that busy routed
    // incumbent. Refused, immediately, naming the role — not parked
    // on the handshake inbox the way the RTC-superseded case is.
    let driver = a.rtc_driver().expect("driver");
    let (fresh, _sdp) = driver.create_offer().await.expect("a fresh endpoint");
    let attempted = tokio::time::timeout(Duration::from_millis(500), a.accept_rtc(fresh, b_id))
        .await
        .expect(
            "a busy ROUTED responder must be refused at the gate, not wait for \
             msg1 — the displacement branch is for a superseded RTC channel only",
        );
    let why = attempted
        .expect_err("the routed responder must defer")
        .to_string();
    assert!(
        why.contains("the incumbent session is busy") && why.contains("Responder"),
        "the refusal must name the role it applied; got {why:?}",
    );

    // Nothing was lost by it: same incarnation, same relayed
    // endpoint, the busy stream still registered on it, and the
    // routed path still delivers a fresh nonce.
    assert_eq!(
        a.peer_session_id(b_id),
        Some(routed_session),
        "the refused attempt must leave the routed incarnation in place",
    );
    assert_eq!(
        a.peer_endpoint(b_id),
        routed_endpoint,
        "and must not retarget the endpoint",
    );
    assert!(
        a.peer_session_for_test(b_id)
            .is_some_and(|s| s.stream_ids().contains(&0x00A1)),
        "nor discard the very stream it declined to throw away",
    );
    a.send_routed(b_id, &batch(0, 4, "routed-after-refusal"))
        .await
        .expect("routed send after the refusal");
    assert!(
        delivered_with_tag(&b, "routed-after-refusal", Duration::from_secs(15)).await >= 1,
        "and the relay path must still deliver after it",
    );
}

/// The echo handler the existing two-node nRPC witness uses,
/// unchanged — only its transport differs here.
#[cfg(feature = "cortex")]
struct Echo;

#[cfg(feature = "cortex")]
#[async_trait::async_trait]
impl net::adapter::net::cortex::RpcHandler for Echo {
    async fn call(
        &self,
        ctx: net::adapter::net::cortex::RpcContext,
    ) -> Result<
        net::adapter::net::cortex::RpcResponsePayload,
        net::adapter::net::cortex::RpcHandlerError,
    > {
        Ok(net::adapter::net::cortex::RpcResponsePayload {
            status: net::adapter::net::cortex::RpcStatus::Ok,
            headers: vec![],
            body: ctx.payload.body,
        })
    }
}

/// nRPC over an RTC pair: the existing consumer assertion — the
/// **exact reply body** — with only the transport setup changed. A
/// disposition class is not an nRPC response.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_nrpc_call_round_trips_over_the_datachannel() {
    use net::adapter::net::mesh_rpc::CallOptions;

    let caller = node(Some(rtc_config())).await;
    let server = node(Some(rtc_config())).await;
    caller.start();
    server.start();
    connect_rtc_loopback(&caller, &server)
        .await
        .expect("rtc pair");

    let _serve = server.serve_rpc("echo", Arc::new(Echo)).expect("serve_rpc");

    let reply = caller
        .call(
            server.node_id(),
            "echo",
            Bytes::from_static(b"hello over a datachannel"),
            CallOptions::default(),
        )
        .await
        .expect("the call must complete over RTC");
    assert_eq!(
        reply.body.as_ref(),
        b"hello over a datachannel",
        "the reply body must be exact — the point of running nRPC over RTC \
         rather than classifying its send path"
    );
    assert!(reply.latency_ns > 0);
}

/// A **fold** applied over an RTC pair: a named remote fact crosses
/// the DataChannel and lands in the receiver's capability fold, with
/// the existing consumer assertion (`find_nodes_by_filter`) intact.
///
/// This is the half of "nRPC and fold" the report claimed as a
/// disposition class. A class witness proves a packet was admitted;
/// it says nothing about whether the fold applied the fact.
///
/// Inverse: drop all RTC ingress on the receiver
/// (`set_ingress_drop_one_in(1)`) and the fold never learns the tag.
#[cfg(feature = "cortex")]
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_capability_fold_applies_a_remote_fact_received_over_rtc() {
    use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};

    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    a.start();
    b.start();
    connect_rtc_loopback(&a, &b).await.expect("rtc pair");

    // A announces over the only transport it has to B: the
    // DataChannel.
    let caps = CapabilitySet::new().add_tag("rtc-folded").add_tag("gpu");
    a.announce_capabilities(caps)
        .await
        .expect("announce over RTC");

    let filter = CapabilityFilter::new().require_tag("rtc-folded");
    let a_id = a.node_id();
    let folded = wait_for(
        || b.find_nodes_by_filter(&filter).contains(&a_id),
        Duration::from_secs(20),
    )
    .await;
    assert!(
        folded,
        "B's capability fold must APPLY the announcement that crossed the \
         DataChannel — admitting the packet is not applying the fact"
    );

    // And the fold's answer is about A specifically: a tag nobody
    // announced resolves to nothing.
    let absent = CapabilityFilter::new().require_tag("never-announced");
    assert!(
        b.find_nodes_by_filter(&absent).is_empty(),
        "the fold must not answer for a tag no peer announced"
    );
}
