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

/// A pair whose heartbeat is far outside any witness window, so
/// "the queue is empty" can actually hold with the pump paused.
async fn quiet_pair() -> (Arc<MeshNode>, Arc<MeshNode>, RtcPeerId) {
    let mut a_cfg = config(Some(rtc_config()));
    a_cfg.heartbeat_interval = Duration::from_secs(600);
    let mut b_cfg = config(Some(rtc_config()));
    b_cfg.heartbeat_interval = Duration::from_secs(600);
    let a = Arc::new(
        MeshNode::new(EntityKeypair::generate(), a_cfg)
            .await
            .expect("MeshNode::new"),
    );
    let b = Arc::new(
        MeshNode::new(EntityKeypair::generate(), b_cfg)
            .await
            .expect("MeshNode::new"),
    );
    a.start();
    b.start();
    let (id_a, _id_b) = connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    (a, b, id_a)
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

    // Big payloads: the MTU split turns one call into several
    // packets, which is what makes "prefix committed, suffix
    // refused" a shape that can exist at all. With small events the
    // whole slice is one packet and there is no prefix.
    let payloads: Vec<Bytes> = (0..12u8)
        .map(|i| {
            let mut v = b"R2PFX".to_vec();
            v.push(i);
            v.extend_from_slice(&[i; 3000]);
            Bytes::from(v)
        })
        .collect();
    let session = a
        .peer_session_for_test(b.node_id())
        .expect("an installed session");
    let seq_before = session
        .try_stream(0x0774)
        .map(|s| s.current_tx_seq())
        .unwrap_or(0);
    let sender = {
        let a = Arc::clone(&a);
        let payloads = payloads.clone();
        tokio::spawn(async move { a.send_on_stream(&stream, &payloads).await })
    };

    // Release the pressure while the call is still in flight: its
    // internal retry must carry the *remainder*, not the whole slice.
    tokio::time::sleep(Duration::from_millis(100)).await;
    driver.hooks().set_pump_paused(false);
    let outcome = tokio::time::timeout(Duration::from_secs(30), sender)
        .await
        .expect("the call must not hang once pressure clears")
        .expect("join");
    assert!(
        outcome.is_ok(),
        "a call that met pressure after a committed prefix must complete once \
         credit returns; got {outcome:?}"
    );

    let seen = collect_tagged(&b, b"R2PFX", payloads.len(), Duration::from_secs(30)).await;
    // H5: the exact expected vector after the consumer's reorder,
    // not a count of distinct values — a foreign payload plus a
    // missing one satisfied the cardinality check.
    let mut by_seq: std::collections::BTreeMap<u8, Vec<u8>> = std::collections::BTreeMap::new();
    for p in &seen {
        by_seq.insert(p[b"R2PFX".len()], p.clone());
    }
    let reordered: Vec<Vec<u8>> = by_seq.values().cloned().collect();
    let expected: Vec<Vec<u8>> = payloads.iter().map(|p| p.to_vec()).collect();
    assert_eq!(
        reordered, expected,
        "every payload must arrive, and reordered by seq they must be exactly \
         what was sent"
    );

    // The replay question is answered **sender-side**: a whole-call
    // replay of the committed prefix consumes a second sequence
    // number for every packet in it. Counting deliveries cannot
    // answer it — a paused pump makes the reliable RTO fire, the
    // original and the retransmission both land, and nothing dedups
    // events at the shard queue.
    let seq_after = session
        .try_stream(0x0774)
        .map(|s| s.current_tx_seq())
        .unwrap_or(0);
    let packets = seq_after - seq_before;
    assert!(
        packets >= 2,
        "precondition: the slice must span several packets, or there is no \
         prefix to replay (consumed {packets})"
    );
    let retransmits = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        packets <= payloads.len() as u64 + retransmits,
        "one sequence per packet sent, plus retransmissions: {packets} consumed, \
         {} payloads, {retransmits} retransmits — a replayed prefix consumes extras",
        payloads.len()
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
    //
    // H5: sample at a **settled ownership boundary**. These are
    // independent relaxed atomics, not a transactional ledger: a
    // pump between popping a packet (retained decremented) and
    // recording its outcome makes the arithmetic momentarily
    // untrue without anything being lost. Wait for the ledger to
    // hold and *stay* holding across consecutive reads, and only
    // then treat a violation as loss — a single mid-flight read
    // diagnoses scheduling, not conservation.
    let settled = wait_for(
        || {
            let sample = || {
                let accepted = a.rtc_stats().accepted();
                let written = a.rtc_stats().written();
                let queued = driver.transport().queued_packets(id_a) as u64;
                let retained = a.rtc_stats().retained();
                (accepted, written + queued + retained)
            };
            let first = sample();
            // Two consecutive agreeing reads with no accepted-count
            // movement between them: nothing was in flight across
            // the observation.
            first.0 == first.1 && sample() == first
        },
        Duration::from_secs(10),
    )
    .await;
    let accepted = a.rtc_stats().accepted();
    let written = a.rtc_stats().written();
    let queued = driver.transport().queued_packets(id_a) as u64;
    let retained = a.rtc_stats().retained();
    assert!(
        settled,
        "while writes refuse, the settled ledger must balance: \
         accepted({accepted}) == written({written}) + queued({queued}) + \
         retained({retained})"
    );
    assert!(
        a.rtc_stats().write_false() >= 1,
        "the injected refusal must reach the real write-result match"
    );

    // Clear the force: every retained and queued packet must now be
    // delivered, exactly once each.
    driver.hooks().set_force_write_false(false);
    let seen = collect_tagged(&b, b"R3CONS", N, Duration::from_secs(20)).await;
    // H5: the exact expected vector after the consumer's reorder,
    // not a set cardinality. The old assertion counted distinct
    // values, so a foreign payload plus a missing one would have
    // passed.
    let mut by_seq: std::collections::BTreeMap<u8, Vec<u8>> = std::collections::BTreeMap::new();
    for payload in &seen {
        by_seq.insert(payload[b"R3CONS".len()], payload.clone());
    }
    let reordered: Vec<Vec<u8>> = by_seq.values().cloned().collect();
    let expected: Vec<Vec<u8>> = payloads.iter().map(|p| p.to_vec()).collect();
    assert_eq!(
        reordered, expected,
        "every admitted packet must be delivered once the refusal clears, and \
         reordered by seq they must be exactly what was sent"
    );
    assert_eq!(
        a.rtc_stats().retained(),
        0,
        "no packet may stay retained once the channel accepts writes again"
    );
}

/// R3-B: node shutdown releases the RTC socket — while the runtime
/// **and the node** are still alive, so neither runtime teardown nor
/// the destructor's abort can be what does it (H1: the original
/// dropped the node before rebinding, mixing the two paths).
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

    // Bind the exact address with the node still held: if the driver
    // is still alive, this is the failure a successor node would hit.
    let rebound = tokio::net::UdpSocket::bind(addr).await;
    assert!(
        rebound.is_ok(),
        "shutdown must release the dedicated RTC socket at {addr} \
         before it returns, with the node still alive: {rebound:?}"
    );
    drop(rebound);
    drop(a);
}

/// H1: `shutdown_and_join` takes its **timeout** arm and still
/// returns only after the task is gone.
///
/// The loop is parked in a 60 s sleep, so the cooperative exit
/// cannot happen: shutdown must abort — and then *await* the abort.
/// `abort()` requests cancellation, it does not perform it, so the
/// pre-repair code returned with the socket still bound.
///
/// Inverse: move the handle into `timeout(...)` again (dropping it on
/// the timeout) or delete the `handle.await` after `abort()` — the
/// immediate rebind fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_driver_is_aborted_and_joined_before_shutdown_returns() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let driver = a.rtc_driver().expect("driver").clone();
    let addr = driver.local_addr();
    driver.hooks().set_stall_loop(true);
    // Let the loop reach the stall.
    tokio::time::sleep(Duration::from_millis(300)).await;

    driver.shutdown_and_join().await;

    // No yield, no sleep, no wait_for: the method's contract is that
    // the socket is released when it returns.
    let rebound = std::net::UdpSocket::bind(addr);
    assert!(
        rebound.is_ok(),
        "shutdown_and_join returned before its aborted task released the socket: {rebound:?}"
    );
    assert!(
        driver.transport().is_terminal(),
        "an aborted teardown must still make the transport terminal"
    );
    drop(a);
}

/// H1: two concurrent joiners both observe **completed** teardown.
///
/// One of them finds the handle; the other finds `None`. Before the
/// repair, `None` was read as "already done" and that caller
/// returned while the driver was still running.
///
/// Inverse: return immediately on `None` instead of waiting on the
/// shared completion — the second joiner's rebind fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_joiners_both_observe_completed_teardown() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let driver = a.rtc_driver().expect("driver").clone();
    let addr = driver.local_addr();
    driver.hooks().set_stall_loop(true);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Each joiner reports the completion marker the task's own
    // teardown guard publishes — `terminal` is set after every slot
    // is closed and before the socket is released, so observing it
    // is observing completed teardown. The socket is bound once,
    // after both return: two joiners racing the same `bind` would
    // fail each other on `AddrInUse` and say nothing about the
    // driver.
    let first = {
        let d = driver.clone();
        tokio::spawn(async move {
            d.shutdown_and_join().await;
            d.transport().is_terminal()
        })
    };
    let second = {
        let d = driver.clone();
        tokio::spawn(async move {
            d.shutdown_and_join().await;
            d.transport().is_terminal()
        })
    };
    let (first, second) = tokio::join!(first, second);
    assert!(
        first.expect("first joiner"),
        "the joiner that owned the handle must see completed teardown"
    );
    assert!(
        second.expect("second joiner"),
        "the joiner that found no handle must wait for the SAME completion, \
         not return on the assumption that someone else finished"
    );
    let rebound = std::net::UdpSocket::bind(addr);
    assert!(
        rebound.is_ok(),
        "both joins returned, so the socket must be free: {rebound:?}"
    );
    drop(a);
}

/// H1: `Drop` — which aborts rather than exiting cooperatively —
/// still runs the whole transport teardown.
///
/// Kyra's schedule: a connected pair with the pump paused and three
/// packets admitted, then drop the node while holding a driver
/// clone. The abort used to release the socket and stop there:
/// `slot_open = true`, `queued = 3`, and a fresh `submit` returned
/// `Ok(())` into a driver that no longer existed.
///
/// Inverse: perform teardown in the loop tail again instead of in
/// `SessionTable::drop` (or skip `shutdown_terminal`) — the slot
/// stays open, the queue keeps its packets and the fresh submit is
/// admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_a_node_tears_down_the_transport_not_just_the_socket() {
    let (a, b, id, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver").clone();
    let addr = driver.local_addr();
    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(150)).await;

    let discarded_before = driver.stats().discarded_at_close();
    for _ in 0..3 {
        driver
            .transport()
            .submit(&[0x48u8; 256], id)
            .expect("admitted before teardown");
    }
    // The node's own maintenance traffic also queues behind the
    // paused pump, so the exact figure teardown must account for is
    // whatever is settled in the queue at the moment of the drop —
    // not the three this test admitted.
    let queued_at_drop = driver.transport().queued_packets(id);
    assert!(
        queued_at_drop >= 3,
        "the three admitted packets must still be queued behind the paused pump"
    );

    drop(a);

    assert!(
        wait_for(
            || std::net::UdpSocket::bind(addr).is_ok(),
            Duration::from_secs(5)
        )
        .await,
        "the destructor must release the RTC socket"
    );
    assert!(
        !driver.transport().is_open(id),
        "the aborted teardown must close the slot, not leave it addressable"
    );
    assert_eq!(
        driver.transport().queued_packets(id),
        0,
        "queued packets must be drained by teardown, not left in a dead queue"
    );
    assert_eq!(
        driver.stats().discarded_at_close() - discarded_before,
        queued_at_drop as u64,
        "every admitted packet teardown throws away must be counted, not \
         silently dropped with the aborted task"
    );
    assert_eq!(
        driver.transport().submit(&[0x49u8; 64], id),
        Err(RtcSubmitError::UnknownPeer),
        "a historical handle must be refused once the driver is gone"
    );
    drop(b);
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

/// R4-B: the RTC ingress prefilter admits exactly the five outer
/// **formats** dispatch accepts, and counts what it rejects.
///
/// Scope, labelled (H5): this is **format acceptance only**. It
/// says nothing about authenticated route-hop forwarding or native
/// pingwave route learning — a shape being admitted by the
/// prefilter is not the same as the packet being authenticated,
/// forwarded or learned from, and those are separate gates several
/// modules away (`relay_protected_hop`, the pingwave admission
/// path). Do not read this witness as evidence about either.
///
/// Inverse: restore the narrow prefilter (Net magic only) — the
/// valid route-hop and pingwave shapes are rejected and the
/// rejection delta assertion fails.
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

/// R6: a reliable stream delivers **every value**, and the
/// consumer's reorder by `seq` reproduces the sender's exact
/// sequence, each value exactly once — through real selected loss,
/// with the retransmit evidence that says recovery carried it.
///
/// The claim is deliberately the one the substrate makes. RTC and
/// raw dispatch permit out-of-order and repeated *observations*
/// (`streams.md`: "no loss, not in order"); ordering is the
/// consumer's, via the sequence each payload carries. Kyra
/// permuted the values at the send seam and the old assertions —
/// a `HashSet` of size N — passed, so the name promised an
/// ordering property the body never tested. This asserts the real
/// one: reorder by the embedded sequence, then require the exact
/// vector.
///
/// Inverses: (1) suppress the sends on this stream id — nothing
/// arrives and the value assertion fails; (2) permute the values at
/// the send seam — the reordered sequence no longer matches;
/// (3) disable the loss injector — the recovery evidence assertion
/// fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reliable_stream_delivers_every_value_and_reorders_by_seq() {
    let (a, b, _id_a, _) = pair_with(rtc_config(), rtc_config()).await;

    // One in four inbound DataChannel messages is dropped on B, with
    // `maxRetransmits: 0` behind it: only `reliability.rs` can repair
    // this. One in four rather than one in three because the repair
    // budget is finite — at a third, a saturated box can exhaust the
    // retransmit budget before the stream completes, which measures
    // the machine rather than the transport.
    b.rtc_driver()
        .expect("driver")
        .hooks()
        .set_ingress_drop_one_in(4);

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

    let seen = collect_tagged(&b, b"R6REL", N, Duration::from_secs(60)).await;

    // The consumer's reorder: each payload carries its sequence in
    // the byte after the tag (`tagged_payloads`), which is what a
    // real consumer reorders on.
    let mut by_seq: std::collections::BTreeMap<u8, Vec<Vec<u8>>> =
        std::collections::BTreeMap::new();
    for payload in &seen {
        let seq = payload[b"R6REL".len()];
        by_seq.entry(seq).or_default().push(payload.clone());
    }
    assert_eq!(
        by_seq.len(),
        N,
        "a reliable stream must deliver every value: {} distinct sequences of \
         {N} ({} deliveries)",
        by_seq.len(),
        seen.len()
    );
    // Reordered, the result is the sender's exact sequence — each
    // value once, in its own position, with no foreign or corrupted
    // value in between.
    let reordered: Vec<Vec<u8>> = by_seq
        .values()
        .map(|copies| {
            // Every copy of a sequence must be byte-identical: a
            // differing copy is corruption, not a retransmission.
            assert!(
                copies.windows(2).all(|w| w[0] == w[1]),
                "two deliveries claimed the same sequence with different bytes"
            );
            copies[0].clone()
        })
        .collect();
    let expected: Vec<Vec<u8>> = payloads.iter().map(|p| p.to_vec()).collect();
    assert_eq!(
        reordered, expected,
        "reordered by seq, the stream must be exactly what the sender sent"
    );

    // Duplicate *observations* are permitted — the substrate allows
    // them — but only as retransmissions, and the count says so.
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

    // Half the messages are dropped and nothing repairs them — and the
    // other half ARRIVE. `seen.len() < N` alone is satisfied by a
    // stream that never sent anything (Kyra's suppress-the-sends
    // inverse passed it); the loss claim needs both bounds, and the
    // survivors must be the sender's own values in the sender's order.
    let seen = collect_tagged(&b, b"R6FAF", N, Duration::from_secs(8)).await;
    assert!(
        seen.len() < N,
        "with every second message dropped and no recovery, a fire-and-forget \
         stream must actually lose packets; saw all {N}"
    );
    assert!(
        !seen.is_empty(),
        "a fire-and-forget stream under 1-in-2 loss must still deliver the \
         other half; nothing arrived, which is what a stream that never sent \
         looks like"
    );
    let mut cursor = 0usize;
    for got in &seen {
        let pos = payloads[cursor..]
            .iter()
            .position(|p| p == got)
            .unwrap_or_else(|| {
                panic!("received a value the sender never sent, or out of order: {got:?}")
            });
        cursor += pos + 1;
    }
    assert_eq!(
        a.control_plane_stats()
            .retransmit_packets_sent
            .load(std::sync::atomic::Ordering::Relaxed),
        before_retransmit,
        "an unreliable stream must retain nothing and retransmit nothing"
    );
}

/// R6/H5: the advisory refresh publishes a fresh reading for a
/// queued peer the pump never touches — from a **stale-high**
/// starting point.
///
/// The precondition is the whole test. `open_peer` initialises the
/// published reading to zero and recycling resets it, so the
/// original `Some(0)` predicate was already true before the action:
/// the witness passed with the entire refresh arm deleted. Here the
/// reading is poisoned to a value above the advisory bound, that is
/// asserted to be non-zero *and* admission-refusing, and only then
/// is decay required — with the pump paused throughout, so no write
/// can be what published it.
///
/// Inverse: delete the independent advisory-refresh phase from the
/// driver loop — the poisoned reading never decays and admission
/// stays refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advisory_refreshes_for_a_queued_peer_the_pump_never_touches() {
    let (a, _b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    driver.hooks().set_pump_paused(true);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Queue work first: this is the QUEUED arm of the refresh, and
    // it must be distinguishable from the empty-queue case.
    transport
        .submit(&[0u8; 256], id_a)
        .expect("queue something so the refresh arm considers this peer");

    // Poison the reading above the advisory bound.
    let poisoned = RtcConfig::new().buffered_amount_advisory + 1;
    transport.poison_published_buffered(id_a, poisoned);
    assert_eq!(
        transport.published_buffered(id_a),
        Some(poisoned),
        "precondition: the published reading is stale-HIGH, not an initialised zero"
    );
    assert_eq!(
        transport.submit(&[0u8; 64], id_a),
        Err(RtcSubmitError::AdvisoryOver),
        "precondition: that reading is refusing admission"
    );
    assert!(
        transport.queued_packets(id_a) >= 1,
        "precondition: the pump is paused, so no write can publish a reading"
    );

    let refreshed = wait_for(
        || {
            transport
                .published_buffered(id_a)
                .is_some_and(|v| v < poisoned)
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(
        refreshed,
        "the refresh arm must publish a fresh reading for a queued peer the pump \
         never touches; still {:?}",
        transport.published_buffered(id_a)
    );
    assert!(
        transport.queued_packets(id_a) >= 1,
        "and it must do so without draining the queue"
    );
}

/// The other half of the same arm: an **empty** queue with a
/// stale-high reading must also decay, or an idle peer stays
/// refused forever.
///
/// Inverse: restrict the refresh to peers with queued work (drop
/// the `stale_high` term) — the reading never decays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_advisory_decays_for_an_idle_peer_with_an_empty_queue() {
    let (a, _b, id_a) = quiet_pair().await;
    let driver = a.rtc_driver().expect("driver");
    let transport = driver.transport();

    // The pump is left RUNNING and the queue left EMPTY: `pump_peer`
    // returns before publishing anything when it finds nothing to
    // pop, so any reading published here can only come from the
    // independent stale-high refresh arm. (Pausing the pump instead
    // cannot hold "empty" — the node's own maintenance traffic
    // queues behind it.)
    assert!(
        wait_for(
            || transport.queued_packets(id_a) == 0,
            Duration::from_secs(10)
        )
        .await,
        "precondition: the queue must be empty for the idle arm to be the \
         only thing that can fire"
    );

    let poisoned = RtcConfig::new().buffered_amount_advisory + 1;
    transport.poison_published_buffered(id_a, poisoned);
    assert_eq!(
        transport.submit(&[0u8; 64], id_a),
        Err(RtcSubmitError::AdvisoryOver),
        "precondition: the stale reading refuses admission — and the refusal \
         leaves the queue empty, which is the arm under test"
    );

    assert!(
        wait_for(
            || transport
                .published_buffered(id_a)
                .is_some_and(|v| v < poisoned),
            Duration::from_secs(10)
        )
        .await,
        "an idle peer's stale-high reading must decay without any write"
    );
    assert!(
        transport.submit(&[0u8; 64], id_a).is_ok(),
        "and admission must recover once the reading is the truth"
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
    // Drain whatever the setup produced, so only post-reset traffic
    // can satisfy the assertions below (H5: the original collected
    // with an EMPTY tag and accepted `to_b || to_c`, so one broken
    // sibling — or an unrelated earlier event — passed it).
    let _ = collect_tagged(&b, b"", 64, Duration::from_millis(200)).await;
    let _ = collect_tagged(&c, b"", 64, Duration::from_millis(200)).await;

    let payload_b = Bytes::from_static(b"RESETB");
    let payload_c = Bytes::from_static(b"RESETC");
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let to_b_stream = a
        .open_stream(b.node_id(), 0x0B01, cfg)
        .expect("stream to b");
    let to_c_stream = a
        .open_stream(c.node_id(), 0x0C01, cfg)
        .expect("stream to c");
    a.send_with_retry(&to_b_stream, std::slice::from_ref(&payload_b), 16)
        .await
        .expect("send to b");
    a.send_with_retry(&to_c_stream, std::slice::from_ref(&payload_c), 16)
        .await
        .expect("send to c");

    let seen_b = collect_tagged(&b, b"RESETB", 1, Duration::from_secs(15)).await;
    let seen_c = collect_tagged(&c, b"RESETC", 1, Duration::from_secs(15)).await;
    assert!(
        seen_b.iter().any(|p| p == payload_b.as_ref()),
        "B must receive ITS post-reset payload; saw {seen_b:?}"
    );
    assert!(
        seen_c.iter().any(|p| p == payload_c.as_ref()),
        "C must receive ITS post-reset payload — one working sibling is not \
         'siblings intact'; saw {seen_c:?}"
    );
}

/// R6/H5: **one stress schedule**, with its preconditions measured
/// — not a proof of the service bound.
///
/// A producer refills B's queue to refusal for the whole window,
/// the backlog is sampled and required to stay deep, and C's own
/// tagged payload must arrive within the window. That is what this
/// test establishes: under a continuously deep backlog on one peer,
/// a sibling is still served.
///
/// It is deliberately **not** advertised as a proof of
/// `WRITE_QUANTUM_PER_TURN`. Executed inverse (H5g): removing the
/// quantum — restoring the drain-until-empty pump — leaves this
/// test green, because a fast unbounded pump drains each refill and
/// yields anyway. The bounded quanta are credited from source
/// (`driver.rs`: 8 writes per peer per turn, 16 signals per turn);
/// this witness is the stress schedule around them. A real
/// service-bound proof needs a driver-side scheduling observation
/// this fixture does not have.
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
            // Fill to refusal, not a fixed burst: the backlog has to
            // be *continuously* deep for the sibling's progress to
            // say anything about fairness (H5), and a fixed burst
            // drains between iterations.
            while transport.submit(&[0x7Au8; 1024], id_ab).is_ok() {}
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // While B is saturated, C must still get served promptly — with
    // an EXACT payload (H5: an empty tag accepted any pre-existing
    // event) and with B's backlog observed to stay deep for the
    // whole measurement, so "saturated" is a measured precondition
    // rather than an assumption about the producer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let backlog_probe = {
        let transport = Arc::clone(a.rtc_driver().expect("driver").transport());
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            let mut min_seen = usize::MAX;
            while tokio::time::Instant::now() < deadline {
                min_seen = min_seen.min(transport.queued_packets(id_ab));
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            min_seen
        })
    };
    let payload = Bytes::from_static(b"R6FAIRC");
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let sibling_stream = a
        .open_stream(c.node_id(), 0x0C77, cfg)
        .expect("stream to the sibling");
    let started = tokio::time::Instant::now();
    a.send_with_retry(&sibling_stream, std::slice::from_ref(&payload), 16)
        .await
        .expect("send to the sibling");
    let seen = collect_tagged(&c, b"R6FAIRC", 1, Duration::from_secs(10)).await;
    let elapsed = started.elapsed();
    let min_backlog = backlog_probe.await.expect("backlog probe");
    busy.abort();

    assert!(
        min_backlog >= 8,
        "precondition: B's queue must stay deeply backlogged for the whole \
         measurement, or the producer saturated nothing and the sibling's \
         progress says nothing about fairness (min {min_backlog})"
    );
    assert!(
        seen.iter().any(|p| p == payload.as_ref()),
        "the sibling must receive ITS payload while another peer is saturated; \
         saw {seen:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "under a bounded per-peer quantum the sibling is served while the busy \
         peer's backlog stands; took {elapsed:?}"
    );
}
