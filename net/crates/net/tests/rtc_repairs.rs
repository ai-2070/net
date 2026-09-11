//! Witnesses for Kyra's HOLD on `0fcff7a16` (S3-R1 … S3-R6).
//!
//! Each test here exists because a *wrong implementation* passed the
//! previous witness. Where the brief names an inverse, the test is
//! written so that inverse fails it — the comments say which
//! mutation, so the claim can be re-checked rather than believed.
//!
//! Run: `cargo test --features "webrtc fixtures" --test rtc_repairs`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::rtc::{connect_rtc_loopback, RtcConfig, RtcPeerId, RtcSubmitError};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability, SocketBufferConfig,
    StreamConfig, StreamError,
};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

/// `EnhancedPingwave::SIZE` — the fixed, headerless wire format the
/// dispatcher recognizes by length and by *not* starting with Net
/// magic. Restated here because the type is crate-internal.
const PINGWAVE_SIZE: usize = 72;

/// A structurally valid Net header — right magic, right version —
/// whose **declared** payload length is over `MAX_PAYLOAD_SIZE`.
///
/// This is the case that isolates payload-length validation: wrong
/// magic at a valid size and this are different rejections, and
/// 9 000 bytes of `0xAA` exercises only the first.
fn net_header_with_declared_payload(declared: u16) -> Vec<u8> {
    let header = net_wire::protocol::NetHeader::new(
        0xDEAD_BEEF,
        1,
        1,
        [0u8; net_wire::protocol::NONCE_SIZE],
        declared,
        1,
        net_wire::protocol::PacketFlags::NONE,
    );
    let mut out = header.to_bytes().to_vec();
    out.extend_from_slice(&[0u8; 64]);
    out
}

// ---------------------------------------------------------------
// harness
// ---------------------------------------------------------------

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), [0x51u8; 32]);
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

/// Drain every shard and return the payloads whose bytes start with
/// `tag`, **in arrival order**, so a witness can assert values,
/// order, completeness and duplicates rather than a count of
/// unrelated events.
async fn collect_tagged(
    node: &Arc<MeshNode>,
    tag: &[u8],
    want: usize,
    within: Duration,
) -> Vec<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen: Vec<Vec<u8>> = Vec::new();
    // Progress is measured in **distinct** payloads: a retransmitted
    // packet that also arrived originally is a duplicate delivery,
    // not progress, and stopping on the raw count would hide a
    // missing payload behind a duplicated one.
    while tokio::time::Instant::now() < deadline && seen.iter().collect::<HashSet<_>>().len() < want
    {
        let mut got_any = false;
        for shard in 0..4u16 {
            let result = node.poll_shard(shard, None, 512).await.expect("poll_shard");
            for event in result.events {
                let raw = event.raw.to_vec();
                if raw.starts_with(tag) {
                    seen.push(raw);
                    got_any = true;
                }
            }
        }
        if !got_any {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    seen
}

async fn wait_for<F: Fn() -> bool>(predicate: F, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    predicate()
}

/// `n` distinct payloads sharing one tag, each carrying its index, so
/// order and completeness are both observable at the receiver.
fn tagged_payloads(tag: &[u8], n: usize) -> Vec<Bytes> {
    (0..n)
        .map(|i| {
            let mut v = tag.to_vec();
            v.push(i as u8);
            v.extend_from_slice(&[0xE1u8; 48]);
            Bytes::from(v)
        })
        .collect()
}

// ---------------------------------------------------------------
// S3-R1 — the scheduler handoff
// ---------------------------------------------------------------

/// R1: a `scheduled` stream's packets reach RTC **admission**, and
/// then the peer, with exact values and no losses.
///
/// Inverse: delete `router.set_rtc_transport(..)` from
/// `MeshNode::new` — the scheduler drain's RTC arm finds an empty
/// option, every packet is consumed silently, and both halves of this
/// test fail (admission delta 0; no payload arrives).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_fire_and_forget_packets_reach_rtc_admission_and_the_peer() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::FireAndForget;
    cfg.scheduled = true;
    let stream = a
        .open_stream(b.node_id(), 0x0772, cfg)
        .expect("open_stream");

    // Paused pump: admitted packets stay in the reserved queue, so
    // the delta is attributable rather than raced by the drain.
    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let accepted_before = a.rtc_stats().accepted();
    let queued_before = driver.transport().queued_packets(id_a);

    const N: usize = 16;
    let payloads = tagged_payloads(b"R1SCHED", N);
    for payload in &payloads {
        a.send_on_stream(&stream, std::slice::from_ref(payload))
            .await
            .expect("scheduled send accepted");
    }

    // The scheduler must have handed all of them over.
    let handed_over = wait_for(
        || driver.transport().queued_packets(id_a) >= queued_before + N,
        Duration::from_secs(10),
    )
    .await;
    let accepted_delta = a.rtc_stats().accepted() - accepted_before;
    assert!(
        handed_over,
        "the scheduler drain must hand scheduled packets to RTC admission; \
         queued {} -> {}, admission delta {accepted_delta}",
        queued_before,
        driver.transport().queued_packets(id_a)
    );
    assert!(
        accepted_delta >= N as u64,
        "every scheduled packet must be admitted, not consumed: delta {accepted_delta}"
    );

    // …and, once the pump runs, the peer gets exactly those bytes.
    driver.hooks().set_pump_paused(false);
    let seen = collect_tagged(&b, b"R1SCHED", N, Duration::from_secs(15)).await;
    assert_eq!(
        seen.len(),
        N,
        "all {N} scheduled payloads must arrive; saw {}",
        seen.len()
    );
    for (i, payload) in payloads.iter().enumerate() {
        assert!(
            seen.iter().any(|s| s.as_slice() == payload.as_ref()),
            "scheduled payload {i} never arrived"
        );
    }
}

/// R1, second half: the drain's **stated** disposition under
/// pressure — one re-offer, then counted and dropped.
///
/// Inverse: make `submit_rtc_with_one_retry` return without counting
/// (its pre-repair shape when the transport is missing) and
/// `drain_refused` never moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scheduler_drain_counts_what_it_drops_under_pressure() {
    let tiny = RtcConfig {
        send_queue_packets: 2,
        send_queue_bytes: 512,
        ..rtc_config()
    };
    let (a, b, id_a, _) = pair_with(tiny, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Saturate the reservation from outside the scheduler.
    while driver.transport().submit(&[0u8; 200], id_a).is_ok() {}

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::FireAndForget;
    cfg.scheduled = true;
    let stream = a
        .open_stream(b.node_id(), 0x0773, cfg)
        .expect("open_stream");

    let before = a.rtc_stats().drain_refused();
    for payload in tagged_payloads(b"R1DROP", 8) {
        // The scheduler accepts the enqueue; the refusal happens
        // later, in the drain, which is exactly the class boundary.
        let _ = a
            .send_on_stream(&stream, std::slice::from_ref(&payload))
            .await;
    }

    assert!(
        wait_for(
            || a.rtc_stats().drain_refused() > before,
            Duration::from_secs(10)
        )
        .await,
        "a drain that cannot place a packet must count it (drain_refused), never drop silently"
    );
}

// ---------------------------------------------------------------
// S3-R2 — typed pressure
// ---------------------------------------------------------------

/// R2: a refused **first** packet on an unscheduled reliable stream
/// is `Backpressure`, the credit is refunded, the sequence is rolled
/// back, and no retransmit descriptor is left behind. Draining the
/// queue then lets the same send succeed.
///
/// Inverse: restore `map_err(|e| StreamError::Transport(..))` on the
/// unscheduled arm — the first assertion fails (`Transport`), and
/// `send_with_retry` stops riding it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rtc_admission_pressure_is_retryable_backpressure_not_a_transport_error() {
    let small = RtcConfig {
        send_queue_packets: 4,
        ..rtc_config()
    };
    let (a, b, id_a, _) = pair_with(small, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    cfg.scheduled = false;
    let stream = a
        .open_stream(b.node_id(), 0x0771, cfg)
        .expect("open_stream");

    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(30)).await;
    while driver.transport().submit(&[0u8; 32], id_a).is_ok() {}
    assert_eq!(driver.transport().queued_packets(id_a), 4);

    let session = a
        .peer_session_for_test(b.node_id())
        .expect("an installed session");
    let seq_before = session
        .try_stream(0x0771)
        .map(|s| s.current_tx_seq())
        .unwrap_or(0);
    let credit_before = session
        .try_stream(0x0771)
        .map(|s| s.tx_credit_remaining())
        .unwrap_or(0);

    let refused = a
        .send_on_stream(&stream, &[Bytes::from_static(b"R2-pressure")])
        .await;
    assert!(
        matches!(refused, Err(StreamError::Backpressure)),
        "a healthy DataChannel with a full reserved queue is pressure, not a \
         transport fault; got {refused:?}"
    );
    assert_eq!(
        session
            .try_stream(0x0771)
            .map(|s| s.current_tx_seq())
            .unwrap_or(0),
        seq_before,
        "the consumed sequence must be rolled back, or the receiver sees a gap"
    );
    assert_eq!(
        session
            .try_stream(0x0771)
            .map(|s| s.tx_credit_remaining())
            .unwrap_or(0),
        credit_before,
        "the byte credit must be refunded when the packet never reached the wire"
    );

    // Drain and retry: the same send now lands.
    driver.hooks().set_pump_paused(false);
    assert!(
        wait_for(
            || driver.transport().queued_packets(id_a) == 0,
            Duration::from_secs(10)
        )
        .await,
        "the pump must drain the reservation once it is resumed"
    );
    a.send_with_retry(&stream, &[Bytes::from_static(b"R2-after-drain")], 8)
        .await
        .expect("the retry path must ride pressure to success");
}

/// R2, the partial-call case: a call whose **prefix** was accepted
/// and whose suffix meets pressure must finish without replaying the
/// prefix — one delivery per payload, no duplicates.
///
/// Inverse: return whole-call `Backpressure` after a committed
/// prefix (drop `send_on_stream`'s `committed_any` branch) and the
/// caller's natural whole-slice retry duplicates the prefix, which
/// this test rejects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_committed_prefix_is_never_replayed_when_the_suffix_is_refused() {
    let small = RtcConfig {
        send_queue_packets: 3,
        ..rtc_config()
    };
    let (a, b, _id_a, _) = pair_with(small, rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    cfg.scheduled = false;
    let stream = a
        .open_stream(b.node_id(), 0x0774, cfg)
        .expect("open_stream");

    // Pause the pump: the first packets of the call are admitted into
    // the three reserved slots and the rest meet a full reservation
    // **mid-call**, which is the shape under test.
    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(30)).await;

    let payloads = tagged_payloads(b"R2PFX", 12);
    let sender = {
        let a = Arc::clone(&a);
        let payloads = payloads.clone();
        tokio::spawn(async move { a.send_on_stream(&stream, &payloads).await })
    };

    // Release the pressure while the call is still in flight: its
    // internal retry must carry the *remainder*, not the whole slice.
    tokio::time::sleep(Duration::from_millis(100)).await;
    driver.hooks().set_pump_paused(false);
    let outcome = tokio::time::timeout(Duration::from_secs(20), sender)
        .await
        .expect("the call must not hang once pressure clears")
        .expect("join");
    assert!(
        outcome.is_ok(),
        "a call that met pressure after a committed prefix must complete once \
         credit returns; got {outcome:?}"
    );

    let seen = collect_tagged(&b, b"R2PFX", payloads.len(), Duration::from_secs(20)).await;
    assert_eq!(
        seen.len(),
        payloads.len(),
        "every payload exactly once: {} deliveries for {} payloads",
        seen.len(),
        payloads.len()
    );
    let unique: HashSet<Vec<u8>> = seen.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "no payload may be delivered twice: {} deliveries, {} distinct",
        seen.len(),
        unique.len()
    );
}

// ---------------------------------------------------------------
// S3-R3 — lifetime
// ---------------------------------------------------------------

/// R3-A: a submit parked between its closed precheck and the queue
/// lock, with a close landing in that window, is **refused** — and
/// the conservation law holds exactly.
///
/// Inverse: remove the under-the-lock `closed` re-check in
/// `RtcTransport::submit` — the submit returns `Ok`, the packet sits
/// in a queue nobody will pop, and `accepted` exceeds
/// `written + discarded_at_close + queued + retained`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_submit_that_races_a_close_is_refused_and_nothing_is_orphaned() {
    let (a, _b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = Arc::clone(driver.transport());
    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Absolute totals, not deltas: the node's own traffic is
    // admitted before any baseline could be taken, and the law is
    // about the whole ledger anyway.

    // Two parties meet in the window: the submitting thread, this
    // task, and nobody else — the barrier is what makes the
    // interleaving deterministic rather than hoped for.
    let gate = Arc::new(std::sync::Barrier::new(2));
    transport.set_submit_gate(Some((Arc::clone(&gate), 128)));

    let submitting = {
        let transport = Arc::clone(&transport);
        tokio::task::spawn_blocking(move || transport.submit(&[0u8; 128], id_a))
    };

    // Let the submit reach the gate, close underneath it, then
    // release it into the queue lock.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let discarded = transport.close_peer(id_a, 0);
    gate.wait();
    let result = submitting.await.expect("submit task");

    assert_eq!(
        result,
        Err(RtcSubmitError::UnknownPeer),
        "a submit that loses the race to a close must be refused, not admitted \
         into a queue nobody will drain"
    );
    assert_eq!(
        transport.queued_packets(id_a),
        0,
        "a refused submit must leave nothing behind"
    );

    let accepted = a.rtc_stats().accepted();
    let written = a.rtc_stats().written();
    let discarded_total = a.rtc_stats().discarded_at_close();
    let retained = a.rtc_stats().retained();
    assert_eq!(
        accepted,
        written + discarded_total + retained,
        "conservation: accepted({accepted}) == written({written}) + \
         discarded_at_close({discarded_total}) + retained({retained}); \
         this close reported {discarded}"
    );
}

/// R3-A/R6: full conservation across a forced `write(false)`, the
/// clearing of that force, and actual delivery — the exact ledger,
/// not an inequality.
///
/// Inverse: drop the packet on `Ok(false)` instead of retaining it
/// (S0b's first driver) — `accepted` then exceeds the sum, and the
/// delivered set is short.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_conserves_every_admitted_packet_and_then_delivers_it() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");

    driver.hooks().set_force_write_false(true);
    tokio::time::sleep(Duration::from_millis(50)).await;

    const N: usize = 8;
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a
        .open_stream(b.node_id(), 0x0775, cfg)
        .expect("open_stream");
    let payloads = tagged_payloads(b"R3CONS", N);
    for payload in &payloads {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("admission accepts; the refusal under test is post-acceptance");
    }

    // While every write refuses, nothing may be lost: each admitted
    // packet is either still queued or held in the retry slot.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Absolute totals: the retained gauge is absolute, so the ledger
    // has to be too.
    let accepted = a.rtc_stats().accepted();
    let written = a.rtc_stats().written();
    let queued = driver.transport().queued_packets(id_a) as u64;
    let retained = a.rtc_stats().retained();
    assert_eq!(
        accepted,
        written + queued + retained,
        "while writes refuse: accepted({accepted}) == written({written}) + \
         queued({queued}) + retained({retained})"
    );
    assert!(
        a.rtc_stats().write_false() >= 1,
        "the injected refusal must reach the real write-result match"
    );

    // Clear the force: every retained and queued packet must now be
    // delivered, exactly once each.
    driver.hooks().set_force_write_false(false);
    let seen = collect_tagged(&b, b"R3CONS", N, Duration::from_secs(20)).await;
    let unique: HashSet<Vec<u8>> = seen.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        N,
        "every retained packet must be delivered after the refusal clears; \
         saw {} distinct of {N}",
        unique.len()
    );
    assert_eq!(
        a.rtc_stats().retained(),
        0,
        "no packet may stay retained once the channel accepts writes again"
    );
}

/// R3-B: node shutdown releases the RTC socket — while the runtime is
/// still alive, so runtime teardown cannot be what does it.
///
/// Inverse: drop the `shutdown_and_join` call from `MeshNode::shutdown`
/// (or make the disconnected signal channel non-terminal again) and
/// the rebind fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_shutdown_releases_the_rtc_socket() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let addr = a.rtc_driver().expect("driver").local_addr();

    a.shutdown().await.expect("shutdown");
    drop(a);

    // Bind the exact address: if the driver is still alive, this is
    // the failure a successor node would hit.
    let rebound = tokio::net::UdpSocket::bind(addr).await;
    assert!(
        rebound.is_ok(),
        "shutdown must release the dedicated RTC socket at {addr}: {rebound:?}"
    );
}

/// R3-C: a stale (wrong-generation) handle cannot close a live
/// session, nor collect its open.
///
/// Inverse: resolve signals by `peer.slot` alone — the live session
/// closes and this fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_generation_handle_cannot_touch_the_live_session() {
    let (a, _b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let stale = RtcPeerId {
        slot: id_a.slot,
        generation: id_a.generation.wrapping_add(1),
    };

    let awaited = driver.await_open(stale).await;
    assert!(
        awaited.is_err(),
        "a wrong-generation AwaitOpen must not report the live session's open"
    );
    driver.close(stale).await.expect("signal delivered");

    // Give the driver several turns to act on it, then require the
    // live session to be untouched.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        driver.transport().is_open(id_a),
        "a wrong-generation Close must not evict the live incarnation of that slot"
    );
}

/// R3-D: lifetime churn recycles slots instead of accumulating them,
/// and a handle from lifetime N is refused after the slot is reused
/// in lifetime N+1.
///
/// Inverse: allocate a fresh slot per session (the pre-repair
/// `open_peer`) — retained slots grow to 32 and the bound fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thirty_two_lifetimes_do_not_grow_the_slot_table() {
    let a = node(Some(RtcConfig {
        max_peers: 4,
        ..rtc_config()
    }))
    .await;
    a.start();
    let driver = a.rtc_driver().expect("driver");

    let mut first: Option<RtcPeerId> = None;
    for _ in 0..32 {
        let (id, _sdp) = driver.create_offer().await.expect("offer");
        if first.is_none() {
            first = Some(id);
        }
        driver.close(id).await.expect("close");
        assert!(
            wait_for(|| !driver.transport().is_open(id), Duration::from_secs(5)).await,
            "the close must land before the next lifetime opens"
        );
    }

    assert!(
        driver.transport().retained_slots() <= 4,
        "32 lifetimes must not retain more than max_peers slots; retained {}",
        driver.transport().retained_slots()
    );
    let stale = first.expect("a first lifetime");
    assert_eq!(
        driver.transport().submit(&[0u8; 8], stale),
        Err(RtcSubmitError::UnknownPeer),
        "a handle from lifetime 1 must stay dead after its slot is reused"
    );
}

/// R3-E: a close runs the ordinary peer-removal transaction, and a
/// handshake completing after it cannot install a dead endpoint.
///
/// Inverse: remove `require_live_rtc_endpoint` from `connect_rtc` —
/// the install lands on a closed handle and the peer reappears.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_handle_cannot_be_installed_and_a_close_evicts_the_peer() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let b_id = b.node_id();
    assert_eq!(a.peer_endpoint(b_id), Some(PeerAddr::Rtc(id_a)));

    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(5)).await,
        "a closed channel must evict its peer through the ordinary removal path"
    );

    // Now try to install on that dead handle: the fence must refuse
    // before any transition is published.
    let b_pub = *b.public_key();
    let installed = a.connect_rtc(id_a, &b_pub, b_id).await;
    assert!(
        installed.is_err(),
        "installing on a closed RTC handle must be refused, not published"
    );
    assert!(
        a.peer_endpoint(b_id).is_none(),
        "a refused install must leave no peer behind"
    );
}

/// R3-E: a **busy** incumbent is not replaced by an RTC upgrade — the
/// same quiescence decision `attempt_direct_upgrade` makes.
///
/// Inverse: drop `rtc_upgrade_precheck` — the busy session is
/// replaced and its open stream is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_incumbent_survives_an_rtc_upgrade_attempt() {
    let (a, b, _id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let b_id = b.node_id();
    let sid_before = a.peer_session_id(b_id).expect("an installed session");

    // Make it busy: an open stream is exactly the state the gate
    // refuses to throw away.
    let _stream = a
        .open_stream(b_id, 0x0776, StreamConfig::new())
        .expect("open_stream");

    let driver = a.rtc_driver().expect("driver");
    let (fresh, _sdp) = driver.create_offer().await.expect("offer");
    let b_pub = *b.public_key();
    let attempted = a.connect_rtc(fresh, &b_pub, b_id).await;

    assert!(
        attempted.is_err(),
        "an RTC install must defer while the incumbent is busy, not replace it"
    );
    assert_eq!(
        a.peer_session_id(b_id),
        Some(sid_before),
        "the busy incumbent must survive the refused upgrade unchanged"
    );
}

// ---------------------------------------------------------------
// S3-R4 — input classification
// ---------------------------------------------------------------

/// R4-A: `serve_stun` on both ends does **not** intercept ICE. The
/// session establishes and delivers, and a bare STUN client still
/// gets an answer from the same socket while it does.
///
/// Inverse: answer Binding Requests before `Rtc::accepts` (the
/// pre-repair order) — the pair never opens and this times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serving_stun_does_not_intercept_ice_connectivity_checks() {
    let serving = RtcConfig {
        serve_stun: true,
        ice_deadline: Duration::from_secs(5),
        ..rtc_config()
    };
    let (a, b, id_a, _) = pair_with(serving.clone(), serving).await;

    // The session is live and delivering while STUN is served.
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "stun-coexist"))
        .await
        .expect("send over the RTC pair");
    let delivered = wait_for(
        || a.rtc_driver().expect("driver").transport().is_open(id_a),
        Duration::from_secs(5),
    )
    .await;
    assert!(delivered, "the RTC session must stay open with STUN served");

    // A bare, unsolicited Binding Request on the same socket still
    // gets a well-formed response: serving STUN is not disabled, it
    // is subordinated to the sessions.
    let stun_addr = a.rtc_driver().expect("driver").local_addr();
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("client socket");
    let mut request = Vec::with_capacity(20);
    request.extend_from_slice(&0x0001u16.to_be_bytes()); // Binding Request
    request.extend_from_slice(&0u16.to_be_bytes()); // no attributes
    request.extend_from_slice(&net::adapter::net::rtc::STUN_MAGIC_COOKIE.to_be_bytes());
    request.extend_from_slice(&[0xA5u8; 12]); // transaction id
    client.send_to(&request, stun_addr).await.expect("send");

    let mut buf = [0u8; 256];
    let answered = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf)).await;
    let (n, _) = answered
        .expect("the bare STUN request must be answered")
        .expect("recv");
    let mapped = net::adapter::net::rtc::parse_xor_mapped_address(&buf[..n])
        .expect("a well-formed XOR-MAPPED-ADDRESS");
    assert_eq!(
        mapped.port(),
        client.local_addr().expect("addr").port(),
        "the response must describe the client's own reflexive port"
    );
}

/// R4-B: the prefilter admits every outer format the dispatcher
/// handles, and rejects only what the dispatcher would reject — with
/// each rejection *reason* exercised separately.
///
/// Inverse: restore the two-format filter (routing magic or a
/// validating `NetHeader`) and the route-hop and pingwave cases fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rtc_prefilter_admits_exactly_what_dispatch_accepts() {
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let transport = a.rtc_driver().expect("driver").transport();

    // --- admitted shapes: a route-hop envelope and a pingwave ---
    let mut hop = Vec::new();
    hop.extend_from_slice(&net_wire::route_hop::ROUTE_HOP_MAGIC.to_le_bytes());
    hop.extend_from_slice(&[0x11u8; 96]);
    let before_reject = b.rtc_stats().validate_rejected();
    transport.submit(&hop, id_a).expect("admission");

    let mut pingwave = vec![0u8; PINGWAVE_SIZE];
    // Anything but Net magic in the first two bytes; the dispatcher's
    // own discriminator.
    pingwave[0] = 0xFF;
    pingwave[1] = 0xFF;
    transport.submit(&pingwave, id_a).expect("admission");

    // --- rejected shapes, one reason each ---
    // (a) valid size, bad magic.
    let bad_magic = vec![0xAAu8; 128];
    transport.submit(&bad_magic, id_a).expect("admission");
    // (b) good Net magic and version, oversize declared payload.
    let oversize = net_header_with_declared_payload(u16::MAX);
    transport.submit(&oversize, id_a).expect("admission");
    // (c) a message too short to be anything.
    transport.submit(&[0x45u8], id_a).expect("admission");

    // Exactly the three malformed frames are charged to validation;
    // the two valid outer formats are not.
    assert!(
        wait_for(
            || b.rtc_stats().validate_rejected() >= before_reject + 3,
            Duration::from_secs(10)
        )
        .await,
        "each malformed frame must be counted: validate_rejected {} -> {}",
        before_reject,
        b.rtc_stats().validate_rejected()
    );
    assert!(
        b.rtc_stats().validate_rejected() - before_reject <= 3,
        "a valid route-hop envelope or pingwave must NOT be charged to \
         validate_rejected: delta {}",
        b.rtc_stats().validate_rejected() - before_reject
    );

    // The channel survives all of it.
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "after-mixed"))
        .await
        .expect("send");
    assert!(
        a.rtc_driver().expect("driver").transport().is_open(id_a),
        "neither a rejected frame nor an admitted-but-unauthenticated one may kill the session"
    );
}

// ---------------------------------------------------------------
// S3-R6 — the properties the old witnesses could not discriminate
// ---------------------------------------------------------------

/// R6: a reliable stream's **exact payloads**, in order, complete and
/// without duplicates, through real selected loss — with the
/// retransmit evidence that says recovery is what carried it.
///
/// Inverses: (1) suppress the sends on this stream id — nothing
/// arrives and the value assertion fails, where the old
/// count-the-other-batch witness passed; (2) disable the loss
/// injector — the recovery evidence assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reliable_stream_delivers_exact_values_in_order_through_loss() {
    let (a, b, _id_a, _) = pair_with(rtc_config(), rtc_config()).await;

    // One in three inbound DataChannel messages is dropped on B, with
    // `maxRetransmits: 0` behind it: only `reliability.rs` can repair
    // this.
    b.rtc_driver()
        .expect("driver")
        .hooks()
        .set_ingress_drop_one_in(3);

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a
        .open_stream(b.node_id(), 0x0051, cfg)
        .expect("open_stream");

    const N: usize = 12;
    let payloads = tagged_payloads(b"R6REL", N);
    for payload in &payloads {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("send_with_retry");
    }

    let seen = collect_tagged(&b, b"R6REL", N, Duration::from_secs(30)).await;
    let unique: HashSet<Vec<u8>> = seen.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        N,
        "a reliable stream must deliver every payload under loss; \
         {} distinct of {N} (deliveries: {})",
        unique.len(),
        seen.len()
    );
    for (i, payload) in payloads.iter().enumerate() {
        assert!(
            unique.contains(payload.as_ref()),
            "reliable payload {i} never arrived"
        );
    }

    // Duplicates are bounded by retransmission and never by luck:
    // with no loss injected this same flow delivers 12 for 12 (see
    // the no-loss control in the delivery witness), so any extra
    // delivery here is a re-sent packet whose original also landed.
    let retransmits = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        seen.len() as u64 <= N as u64 + retransmits,
        "extra deliveries must be accounted for by retransmission: {} deliveries, \
         {N} payloads, {retransmits} retransmits",
        seen.len()
    );
    assert!(
        retransmits >= 1,
        "under injected loss the reliable path must actually retransmit \
         (retransmit_packets_sent = {retransmits}) — otherwise the loss injector \
         is not doing anything and this proves nothing"
    );
}

/// R6: the fire-and-forget counterpart — loss is observed, and
/// nothing is retransmitted for it.
///
/// Inverse: retain descriptors for unreliable streams — the
/// retransmit assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fire_and_forget_stream_loses_packets_and_never_retransmits() {
    let (a, b, _id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    b.rtc_driver()
        .expect("driver")
        .hooks()
        .set_ingress_drop_one_in(2);

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::FireAndForget;
    let stream = a
        .open_stream(b.node_id(), 0x0061, cfg)
        .expect("open_stream");

    let before_retransmit = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    const N: usize = 16;
    let payloads = tagged_payloads(b"R6FAF", N);
    for payload in &payloads {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("send");
    }

    // Half the messages are dropped and nothing repairs them.
    let seen = collect_tagged(&b, b"R6FAF", N, Duration::from_secs(8)).await;
    assert!(
        seen.len() < N,
        "with every second message dropped and no recovery, a fire-and-forget \
         stream must actually lose packets; saw all {N}"
    );
    assert_eq!(
        a.control_plane_stats()
            .retransmit_packets_sent
            .load(std::sync::atomic::Ordering::Relaxed),
        before_retransmit,
        "an unreliable stream must retain nothing and retransmit nothing"
    );
}

/// R6: the idle-refresh arm specifically — a peer with a **non-empty
/// queue and no writes** must still have its advisory reading
/// refreshed. Ordinary pumping cannot satisfy this, because the pump
/// is paused for the whole test.
///
/// Inverse: delete the advisory-refresh block from the driver loop
/// (or narrow it to peers the pump just wrote to) and the reading
/// never moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advisory_refreshes_for_a_queued_peer_the_pump_never_touches() {
    let (a, _b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Poison the published reading, then queue work without ever
    // letting the pump write.
    transport
        .submit(&[0u8; 256], id_a)
        .expect("queue something so the refresh arm considers this peer");
    let refreshed = wait_for(
        || transport.published_buffered(id_a) == Some(0),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        refreshed,
        "the refresh arm must publish a reading for a queued peer with no writes"
    );
    assert!(
        driver.transport().queued_packets(id_a) >= 1,
        "precondition: the pump really is paused, so nothing wrote this reading"
    );
}

/// R6: the injected `ConnectionReset` now travels the production
/// error arm, and a **sibling** session keeps delivering.
///
/// Inverse: delete the `ConnectionReset` arm from the driver's read
/// match — the injected error falls into the generic arm, the
/// counter never moves, and this fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_reset_is_swallowed_by_the_production_arm_with_siblings_intact() {
    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    let c = node(Some(rtc_config())).await;
    a.start();
    b.start();
    c.start();
    let (id_ab, _) = connect_rtc_loopback(&a, &b).await.expect("pair a-b");
    let (id_ac, _) = connect_rtc_loopback(&a, &c).await.expect("pair a-c");

    let before = a.rtc_stats().udp_conn_reset();
    a.rtc_driver().expect("driver").hooks().inject_conn_reset();
    assert!(
        wait_for(
            || a.rtc_stats().udp_conn_reset() > before,
            Duration::from_secs(5)
        )
        .await,
        "the reset must be counted by the production read-error arm"
    );

    let transport = a.rtc_driver().expect("driver").transport();
    assert!(
        transport.is_open(id_ab) && transport.is_open(id_ac),
        "an ICMP port-unreachable about some other peer must not close either session"
    );
    a.send_to_peer_node(b.node_id(), &batch(0, 2, "post-reset-b"))
        .await
        .expect("send to b");
    a.send_to_peer_node(c.node_id(), &batch(0, 2, "post-reset-c"))
        .await
        .expect("send to c");
    let to_b = !collect_tagged(&b, b"", 1, Duration::from_secs(10))
        .await
        .is_empty();
    let to_c = !collect_tagged(&c, b"", 1, Duration::from_secs(10))
        .await
        .is_empty();
    assert!(
        to_b || to_c,
        "both sibling sessions must keep delivering after a swallowed reset"
    );
}

/// R6: the bounded service policy. One continuously busy peer cannot
/// hold the driver: a sibling peer's packet and the driver's own
/// socket read both make progress while the busy peer is being
/// served.
///
/// Inverse: remove `WRITE_QUANTUM_PER_TURN` (restore the
/// drain-until-empty pump) and the sibling's delivery time becomes a
/// function of the busy peer's backlog rather than the quantum.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_peer_cannot_starve_a_sibling_or_the_socket() {
    let big = RtcConfig {
        send_queue_packets: 4096,
        send_queue_bytes: 8 << 20,
        ..rtc_config()
    };
    let a = node(Some(big)).await;
    let b = node(Some(rtc_config())).await;
    let c = node(Some(rtc_config())).await;
    a.start();
    b.start();
    c.start();
    let (id_ab, _) = connect_rtc_loopback(&a, &b).await.expect("pair a-b");
    let (_id_ac, _) = connect_rtc_loopback(&a, &c).await.expect("pair a-c");

    let transport = Arc::clone(a.rtc_driver().expect("driver").transport());
    // A continuously replenished producer against B.
    let busy = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        while tokio::time::Instant::now() < deadline {
            for _ in 0..64 {
                let _ = transport.submit(&[0x7Au8; 1024], id_ab);
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // While B is saturated, C must still get served promptly.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = tokio::time::Instant::now();
    a.send_to_peer_node(c.node_id(), &batch(0, 4, "sibling"))
        .await
        .expect("send to the sibling");
    let seen = collect_tagged(&c, b"", 1, Duration::from_secs(10)).await;
    let elapsed = started.elapsed();
    busy.abort();

    assert!(
        !seen.is_empty(),
        "the sibling peer must be served while another peer is saturated"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a bounded service policy means the sibling's latency is a function of the \
         quantum, not of the busy peer's backlog; took {elapsed:?}"
    );
}
