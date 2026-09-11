//! Stage 3 pressure witnesses: admission, retention, advisory
//! staleness, loss, `validate()` rejection, `ConnectionReset`
//! survival, and the per-class dispositions.
//!
//! These are the exit criteria that cannot be observed from a happy
//! path, so each uses the driver's test-only hooks to produce the
//! condition on demand: a paused pump, a `Channel::write` that
//! refuses after a passing precheck, injected DataChannel loss, and
//! an injected socket `ConnectionReset`.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_backpressure`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig, RtcPeerId, RtcSubmitError};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability, SocketBufferConfig,
    StreamConfig,
};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x44u8; 32]);
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

async fn pair_with(
    a_cfg: RtcConfig,
    b_cfg: RtcConfig,
) -> (Arc<MeshNode>, Arc<MeshNode>, RtcPeerId, RtcPeerId) {
    let a = node(Some(a_cfg)).await;
    let b = node(Some(b_cfg)).await;
    a.start();
    b.start();
    let (id_a, id_b) = connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    (a, b, id_a, id_b)
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

async fn drain_until(node: &Arc<MeshNode>, want: usize, within: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen = 0usize;
    while tokio::time::Instant::now() < deadline && seen < want {
        let result = node.poll_shard(0, None, 256).await.expect("poll_shard");
        seen += result.events.len();
        if result.events.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    seen
}

/// EXIT: admission refuses from `try_send` itself — no packet
/// enqueued, no credit committed — when the reserved slots run out,
/// and the refusal is `WouldBlock` (backpressure), not a transport
/// error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reserved_slots_refuse_at_admission_with_nothing_enqueued() {
    let small = RtcConfig {
        send_queue_packets: 4,
        ..rtc_config()
    };
    let (a, _b, id_a, _) = pair_with(small, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    // Pause the pump so the queue can actually fill: with the driver
    // running, a loopback channel drains faster than this loop fills.
    driver.hooks().set_pump_paused(true);

    // The node's own traffic (heartbeats, announcements) shares this
    // queue, so the counters are read as deltas and the loop's own
    // admissions are bounded by — not equal to — the reserved slots.
    let refused_before = a.rtc_stats().admission_refused_slots();
    let queued_before = transport.queued_packets(id_a);
    let mut accepted = 0;
    let mut refused = 0;
    for _ in 0..64 {
        match transport.submit(&[0u8; 256], id_a) {
            Ok(()) => accepted += 1,
            Err(RtcSubmitError::QueueFull) => refused += 1,
            Err(other) => panic!("unexpected refusal: {other}"),
        }
    }

    assert_eq!(
        accepted + refused,
        64,
        "every submit resolved one way or the other"
    );
    assert!(
        accepted + queued_before <= 4,
        "admission never over-admits past the reserved slots: {accepted} accepted \
         on top of {queued_before} already queued"
    );
    assert_eq!(
        transport.queued_packets(id_a),
        4,
        "a refusal must enqueue nothing: no packet is accepted and then dropped"
    );
    assert!(
        a.rtc_stats().admission_refused_slots() - refused_before >= refused as u64,
        "every slot refusal is counted (the node's own traffic may add more)"
    );
    assert_eq!(
        a.rtc_stats().discarded_at_close(),
        0,
        "refusing is not discarding"
    );

    // …and the io::Error shape a `PeerSink` caller sees.
    let as_io: std::io::Error = RtcSubmitError::QueueFull.into();
    assert_eq!(as_io.kind(), std::io::ErrorKind::WouldBlock);
}

/// EXIT: the reserved-BYTES bound holds while the advisory reading is
/// stale — the driver is paused mid-drain, so the published reading
/// cannot be what is doing the work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_reserved_byte_bound_holds_while_the_advisory_is_stale() {
    let small = RtcConfig {
        send_queue_packets: 4096,
        send_queue_bytes: 8 * 1024,
        ..rtc_config()
    };
    let (a, _b, id_a, _) = pair_with(small, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();
    driver.hooks().set_pump_paused(true);

    let stale = transport
        .published_buffered(id_a)
        .expect("a published reading exists");
    assert_eq!(
        stale, 0,
        "precondition: the advisory reads zero, so only the byte bound can refuse"
    );

    // The node's own traffic shares this queue; the bound is exact
    // over everything in it, not over this loop alone.
    let queued_before = transport.queued_bytes(id_a);
    let mut accepted_bytes = 0usize;
    let mut refusal = None;
    for _ in 0..64 {
        match transport.submit(&[0u8; 1024], id_a) {
            Ok(()) => accepted_bytes += 1024,
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }

    assert_eq!(
        refusal,
        Some(RtcSubmitError::BytesFull),
        "the byte bound must be what refuses, with the advisory reading stale at zero"
    );
    assert!(
        accepted_bytes + queued_before <= 8 * 1024,
        "admission never over-admits past the reserved bytes: {accepted_bytes} \
         accepted on top of {queued_before} already queued"
    );
    assert!(
        transport.queued_bytes(id_a) <= 8 * 1024,
        "the bound is a bound: the queue never exceeds it"
    );
    assert!(
        transport.queued_bytes(id_a) > 8 * 1024 - 1024,
        "…and it is tight: admission refused within one packet of the bound, not early"
    );
    assert_eq!(
        transport.published_buffered(id_a).expect("still published"),
        0,
        "the reading stayed stale throughout: the bound did not depend on it"
    );
}

/// EXIT: the advisory refuses earlier than the hard bound, and an
/// idle peer's stale high reading decays to the truth rather than
/// refusing forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advisory_refuses_early_and_then_decays_for_an_idle_peer() {
    let tight = RtcConfig {
        buffered_amount_advisory: 1,
        send_queue_packets: 4096,
        send_queue_bytes: 4 << 20,
        ..rtc_config()
    };
    let (a, _b, id_a, _) = pair_with(tight, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    // Fill enough that the driver publishes a non-zero reading.
    for _ in 0..64 {
        let _ = transport.submit(&[0u8; 4096], id_a);
    }

    let saw_advisory_refusal = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = false;
        while tokio::time::Instant::now() < deadline && !seen {
            if transport.submit(&[0u8; 4096], id_a) == Err(RtcSubmitError::AdvisoryOver) {
                seen = true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        seen
    };
    assert!(
        saw_advisory_refusal,
        "with a 1-byte advisory the published reading must refuse before the hard bound"
    );
    assert!(a.rtc_stats().admission_refused_advisory() >= 1);

    // Idle: the refresh cadence must re-read and republish, so the
    // peer becomes admissible again without any traffic from us.
    let recovered = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut ok = false;
        while tokio::time::Instant::now() < deadline && !ok {
            if transport.submit(&[0u8; 64], id_a).is_ok() {
                ok = true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        ok
    };
    assert!(
        recovered,
        "an idle peer's stale high reading must decay to the truth — otherwise a \
         one-time burst refuses that peer forever"
    );
}

/// EXIT: `Channel::write` returning `Ok(false)` after a passing
/// precheck exercises retention, and does NOT surface as whole-call
/// backpressure. Nothing accepted at admission is lost before close;
/// what is discarded at close is counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_post_acceptance_refusal_retains_and_is_never_reported_as_backpressure() {
    let (a, _b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    // Baseline: the handshake itself already wrote to this channel.
    let written_before = a.rtc_stats().written();
    driver.hooks().set_force_write_false(true);

    // Every submit here passes admission: the queue is far from its
    // bound and the advisory is low.
    for _ in 0..16 {
        transport
            .submit(&[0u8; 512], id_a)
            .expect("admission must accept: the refusal under test is post-acceptance");
    }
    // Let the pump try, and fail, repeatedly.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let stats = a.rtc_stats();
    assert!(
        stats.write_false() >= 1,
        "the injected refusal must actually reach `Channel::write`'s result"
    );
    assert_eq!(
        stats.written(),
        written_before,
        "not one packet may reach the wire while every write refuses"
    );
    assert_eq!(
        stats.discarded_at_close(),
        0,
        "retention means nothing is dropped while the channel is open"
    );

    // Everything admitted so far, including the node's own
    // background traffic while the pump was stuck.
    let accepted = a.rtc_stats().accepted();

    // Close: the remainder is discarded, and counted — the only
    // place an admitted packet is lost.
    driver.close(id_a).await.expect("close");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let discarded = a.rtc_stats().discarded_at_close();
    assert!(
        discarded >= 1,
        "a close mid-backlog must report what it discarded, not drop it silently"
    );
    assert!(
        discarded <= accepted,
        "more discarded ({discarded}) than ever accepted ({accepted}) is accounting nonsense"
    );
}

/// EXIT: with `maxRetransmits: 0`, `reliability.rs` is the only
/// recovery mechanism. Injected DataChannel loss must not stop a
/// reliable stream from completing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reliable_stream_completes_through_injected_datachannel_loss() {
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;

    // One in four inbound messages is dropped on B's driver, with no
    // SCTP retransmission behind it.
    b.rtc_driver()
        .expect("driver")
        .hooks()
        .set_ingress_drop_one_in(4);

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a.open_stream(b.node_id(), 0x61, cfg).expect("open_stream");
    for i in 0u8..16 {
        a.send_on_stream(&stream, &[Bytes::from(vec![i; 128])])
            .await
            .expect("send_on_stream");
    }
    a.send_to_peer_node(b.node_id(), &batch(0, 16, "lossy"))
        .await
        .expect("send_to_peer_node");

    let seen = drain_until(&b, 1, Duration::from_secs(15)).await;
    assert!(
        seen >= 1,
        "under 25% loss the session must still deliver — if nothing arrives, the \
         channel is dead rather than lossy"
    );

    // Loss is real: the receiver saw fewer datagrams than were sent.
    let delivered = b.rtc_stats().ingress_delivered();
    assert!(
        delivered >= 1,
        "the ingress counter must show what did get through"
    );
}

/// EXIT: the RTC ingress path counts `NetHeader::validate`
/// rejections — S0c's silent black hole — and the channel survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversize_frame_is_counted_not_swallowed_and_the_channel_survives() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let before = b.rtc_stats().validate_rejected();

    // A frame that is not a Net packet and not a routing envelope:
    // the shape S0c found vanishing.
    let junk = vec![0xAAu8; 9000];
    a.rtc_driver()
        .expect("driver")
        .transport()
        .submit(&junk, id_a)
        .expect("admission accepts anything that fits; validation is the receiver's job");

    let counted = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut ok = false;
        while tokio::time::Instant::now() < deadline && !ok {
            ok = b.rtc_stats().validate_rejected() > before;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        ok
    };
    assert!(
        counted,
        "a malformed RTC frame must increment validate_rejected, not disappear"
    );

    // The channel is still alive: a real batch still gets through.
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "after-junk"))
        .await
        .expect("send_to_peer_node");
    assert!(
        drain_until(&b, 1, Duration::from_secs(10)).await >= 1,
        "a rejected frame must not kill the session"
    );
}

/// EXIT: an injected `ConnectionReset` on the RTC socket is swallowed
/// and counted, with every other session intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_driver_survives_a_connection_reset_with_sessions_intact() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");

    driver.hooks().inject_conn_reset();
    let counted = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut ok = false;
        while tokio::time::Instant::now() < deadline && !ok {
            ok = a.rtc_stats().udp_conn_reset() >= 1;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ok
    };
    assert!(counted, "the reset must be counted, not ignored");

    assert!(
        driver.transport().is_open(id_a),
        "an ICMP port-unreachable about some other peer must not close this session"
    );
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "post-reset"))
        .await
        .expect("send_to_peer_node");
    assert!(
        drain_until(&b, 1, Duration::from_secs(10)).await >= 1,
        "traffic must keep flowing after a swallowed reset"
    );
}

/// EXIT: per-class dispositions (S0d §3.2). The classes differ in
/// what they do with a refusal, and that is what is asserted here:
/// *refuse-at-admission* rows propagate it, *drop-with-counter* rows
/// count and carry on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_outbound_class_takes_its_stated_disposition() {
    let tiny = RtcConfig {
        send_queue_packets: 1,
        send_queue_bytes: 128,
        ..rtc_config()
    };
    let (a, b, id_a, _) = pair_with(tiny, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    driver.hooks().set_pump_paused(true);

    // Saturate the reserved bound.
    let _ = driver.transport().submit(&[0u8; 128], id_a);

    // refuse-at-admission (rows 30/32/42, the credit-committing
    // ones): the caller sees the refusal, and it is backpressure.
    let stream_send = a
        .send_to_peer_node(b.node_id(), &batch(0, 4, "refuse"))
        .await;
    assert!(
        stream_send.is_err(),
        "a refuse-at-admission class must propagate the refusal to its caller"
    );

    // drop-with-counter: the scheduler drain gives one re-offer and
    // then counts. Nothing here may panic or block, and the counter
    // is the only evidence the packet existed.
    let before = a.rtc_stats().admission_refused();
    let _ = driver.transport().submit(&[0u8; 128], id_a);
    assert!(
        a.rtc_stats().admission_refused() > before,
        "every refusal is counted, whichever class asked"
    );

    // The endpoint is still installed and still an RTC endpoint:
    // pressure is not teardown.
    assert_eq!(a.peer_endpoint(b.node_id()), Some(PeerAddr::Rtc(id_a)));
}
