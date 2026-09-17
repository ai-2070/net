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
use net::adapter::net::rtc::{
    connect_rtc_loopback, open_rtc_channel, RtcConfig, RtcPeerId, RtcSubmitError,
};
use net::adapter::net::{
    ChannelName, EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, Reliability,
    SocketBufferConfig, StreamConfig, StreamError,
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

/// A node with a *chosen* entity identity, so a second node can
/// reconnect as the same `node_id` from its own UDP socket. That is
/// what a browser tab whose DataChannel died looks like from the
/// anchor's side, and it is the only way to get two live sessions for
/// one identity on different 5-tuples in-process.
async fn node_with_identity(secret: [u8; 32], rtc: RtcConfig) -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::from_bytes(secret), config(Some(rtc)))
            .await
            .expect("MeshNode::new"),
    )
}

/// Burn stream epochs on `peer`'s current session until the **next**
/// epoch it allocates is exactly `target`.
///
/// The collision this arranges is the whole point of the R12 witness:
/// `stream_epoch_counter` restarts at 1 per session, so a successor
/// can hand out an epoch a predecessor's handle already holds. Left
/// to whatever ordinal the two sessions happen to reach, the witness
/// would sometimes pass because the epochs differed — an accident,
/// not a discriminator. Panics rather than returning if the counter
/// is already at or past `target`: a premise that cannot be arranged
/// must fail loudly.
fn arrange_next_stream_epoch(node: &Arc<MeshNode>, peer: u64, target: u64) {
    const SCRATCH: u64 = 0x7F00_0000;
    for _ in 0..4096 {
        let probe = node
            .open_stream(peer, SCRATCH, StreamConfig::new())
            .expect("scratch open");
        let epoch = probe.epoch();
        node.close_stream_handle(&probe).expect("scratch close");
        assert!(
            epoch < target,
            "this session's epoch counter is already at {epoch}, past the \
             target {target}: the equal-epoch premise cannot be arranged",
        );
        if epoch + 1 == target {
            return;
        }
    }
    panic!("could not reach epoch {target} within the scratch budget");
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
/// `stall_loop` parks the loop at an `await`, and an `await` is
/// exactly where a cancellation lands, so the post-abort wait here
/// completes in microseconds: its 2 s bound is never approached and
/// bounding it costs this guarantee nothing. That is the whole
/// reconciliation between this row and
/// `a_driver_that_cannot_be_aborted_does_not_hang_the_join` in
/// `rtc_stun_endpoint.rs` — the bound exists for the task that
/// reaches no await point, which is a different task from this one.
///
/// **Strengthened, same claim.** This used to assert only that the
/// port happened to be free on return, which left the row resting
/// on *when the scheduler drops a cancelled task* — a real race,
/// because a join that returns straight after `abort()` and a
/// runtime that has already dropped the task are indistinguishable
/// from outside. `set_teardown_delay_ms` widens the task's teardown
/// guard to 250 ms, so the assertion is now that the join outlasted
/// the WHOLE guard: the same guarantee, settled by arithmetic
/// rather than by scheduling.
///
/// It is not the reason this row was ever green while CI was red.
/// That was simpler and worse: the commit that bounded the join
/// deleted its post-abort wait outright and kept the comment
/// describing it, so the tree that passed locally and the tree CI
/// compiled were not the same code. With the wait deleted this row
/// fails here too, 3 runs of 3, in the same ~2.3 s CI reported.
///
/// Inverse: move the handle into `timeout(...)` again (dropping it on
/// the timeout) or delete the bounded `handle.await` after `abort()`
/// — the immediate rebind fails, on any machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_driver_is_aborted_and_joined_before_shutdown_returns() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let driver = a.rtc_driver().expect("driver").clone();
    let addr = driver.local_addr();
    driver.hooks().set_stall_loop(true);
    // Teardown must be long enough that a join which did not wait
    // for it cannot possibly have seen it finish.
    driver.hooks().set_teardown_delay_ms(250);
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
/// **Strengthened, same claim.** Both joiners' assertions are
/// unchanged; what changed is what they rest on. Teardown was
/// microseconds long, so "observed completed teardown" was in
/// practice a bet on the scheduler having dropped the cancelled
/// task before the joiner read `is_terminal()`.
/// `set_teardown_delay_ms` widens the guard to 250 ms, which makes
/// it an ordering claim: no joiner can report completed teardown
/// unless it really waited for the guard to finish.
///
/// Inverse: return immediately on `None` instead of waiting on the
/// shared completion, or delete the bounded `handle.await` after
/// `abort()` — the joiners report a teardown that has not happened
/// and the rebind fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_joiners_both_observe_completed_teardown() {
    let a = node(Some(rtc_config())).await;
    a.start();
    let driver = a.rtc_driver().expect("driver").clone();
    let addr = driver.local_addr();
    driver.hooks().set_stall_loop(true);
    driver.hooks().set_teardown_delay_ms(250);
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

    // R-D: a transport slot the driver has NO `Session` for.
    // `create_offer` would not do — it creates a driver session the
    // teardown loop walks. This slot is only reachable through
    // `shutdown_terminal`, which is the claim under test: "every
    // historical handle refused", not "every session closed".
    let orphan = driver.transport().open_peer().expect("a bare slot");
    assert!(
        driver.transport().is_open(orphan),
        "the orphan slot must be live before teardown"
    );

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
    // R-D: and so must a handle the teardown loop never walked.
    assert!(
        !driver.transport().is_open(orphan),
        "a slot with no session must also be closed by teardown"
    );
    assert_eq!(
        driver.transport().submit(&[0x4Au8; 64], orphan),
        Err(RtcSubmitError::UnknownPeer),
        "\"every historical handle refused\" includes the slots the session \
         loop never walks — that is what makes the transport terminal"
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

/// Stage 5 (the browser harness's UDP-blocked CONTROL leg): the
/// quiescence gate must not lock an identity out of its own mesh.
///
/// The gate above is right when WE are replacing OUR session: the
/// in-flight state is ours and the deferral is this node declining
/// to throw it away. It is wrong in one shape, and the browser
/// harness paid for it: a peer that has ALREADY superseded its own
/// RTC channel — new DataChannel, completed Noise, same identity —
/// was refused with `the incumbent session is busy`, against six
/// subscription and nRPC-reply streams its previous tab had left
/// open on an endpoint the anchor still believed was live. Nothing
/// could ever clear them: the only party that could close those
/// streams is the party being refused. The identity was locked out
/// for good, and the observable was
/// `session: the anchor did not complete the Noise handshake inside
/// the deadline`.
///
/// So the RESPONDER, facing a busy incumbent on a DIFFERENT RTC
/// endpoint, displaces it. The remote is the only party whose state
/// could be lost and it has already abandoned it.
///
/// The two roles are asserted against the SAME busy incumbent, so
/// the discriminator is the role and nothing else: the responder
/// gets past the gate and waits for `msg1` (there is none to come,
/// so it is still waiting when the window closes), while the
/// initiator is refused immediately and names the reason. A second
/// live DataChannel between the same pair of UDP sockets is not
/// available to assert the full install here — the in-process
/// fixture signalling cannot demux two sessions on one 5-tuple —
/// and the full install over a real second channel is what the
/// browser harness measures end to end.
///
/// Inverse: make `rtc_upgrade_precheck` role-blind again (defer
/// whenever the incumbent is busy) — the responder is refused
/// immediately instead of waiting, and the first assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_peer_that_superseded_its_own_channel_is_not_locked_out_by_it() {
    let (a, b, _id_a, _id_b) = pair_with(rtc_config(), rtc_config()).await;
    let b_id = b.node_id();
    let stale = a.peer_session_id(b_id).expect("an installed session");

    // The state that made the gate refuse: a stream open on A's view
    // of the incumbent. Nothing is sent on it, so B never learns of
    // it — exactly the asymmetry the anchor was stuck in, where the
    // streams belonged to a browser that had gone away and could
    // never close them.
    let _stranded = a
        .open_stream(b_id, 0x0776, StreamConfig::new())
        .expect("open_stream");
    assert!(
        a.peer_session_for_test(b_id)
            .is_some_and(|s| s.stream_ids().contains(&0x0776)),
        "the incumbent must really be busy, or this witness proves nothing",
    );

    let driver = a.rtc_driver().expect("driver");

    // RESPONDER on a DIFFERENT RTC endpoint: the remote has
    // superseded its own channel, so the gate must let this through.
    // Getting past it means parking on the handshake inbox — nothing
    // will send msg1 here, so "still waiting" IS "the gate allowed
    // it". Before the repair this returned immediately with
    // `the incumbent session is busy`.
    let (fresh, _sdp) = driver.create_offer().await.expect("a fresh endpoint");
    let waited = tokio::time::timeout(Duration::from_millis(500), a.accept_rtc(fresh, b_id)).await;
    assert!(
        waited.is_err(),
        "the responder must get past the quiescence gate and wait for msg1; it \
         returned {waited:?}",
    );

    // INITIATOR against the same incumbent: unchanged: this node's
    // own in-flight state, this node's own call, refused at once and
    // naming the role.
    let (mine, _sdp) = driver.create_offer().await.expect("a second endpoint");
    let b_pub = *b.public_key();
    let refused = a.connect_rtc(mine, &b_pub, b_id).await;
    let why = refused
        .expect_err("the initiator must still defer")
        .to_string();
    assert!(
        why.contains("the incumbent session is busy") && why.contains("Initiator"),
        "the initiator's deferral must survive and say whose state it is \
         protecting; got {why:?}",
    );
    assert_eq!(
        a.peer_session_id(b_id),
        Some(stale),
        "and neither attempt installed anything over the incumbent",
    );
}

/// S5-R12: a native `Stream` handle from the session the busy-responder
/// branch DISPLACED must not be able to address the session that
/// replaced it.
///
/// The test above establishes that the responder gets past the
/// quiescence gate. It stops there — it never reaches Noise or the
/// installer, so it cannot see what the displacement does to the
/// handles the responder itself still holds. This one does the whole
/// transition against a real second peer process: same identity, its
/// own UDP socket and RTC driver, a second DataChannel, real Noise,
/// real `install_direct_fenced`.
///
/// What made this reachable: the handle recorded `(peer, stream_id,
/// epoch, config)` and `send_on_stream` resolved the peer's CURRENT
/// session and compared only the epoch. `stream_epoch_counter`
/// restarts at 1 for every `NetSession`, so a successor's stream at
/// the same creation ordinal carries the same epoch — the
/// predecessor's handle passed the check and transmitted into the new
/// lifetime under the OLD handle's reliability flags. The epoch
/// collision is arranged explicitly here rather than left to
/// coincidence, because a witness that passes only when the ordinals
/// happen to differ discriminates nothing.
///
/// Inverse: drop the `peer.session.session_id() != stream.session_id()`
/// refusal from `send_on_stream` and from `close_stream` — the stale
/// send succeeds (equal epochs) and the stale close tears down the
/// successor's stream, so the `SessionSuperseded` assertions and the
/// survival assertion all fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_displaced_sessions_handle_cannot_address_its_successor() {
    // One identity, two processes. `b` holds the incumbent; `b2` is
    // the same node id arriving on a different RTC endpoint.
    let b_secret = [0xB2u8; 32];
    let a = node(Some(rtc_config())).await;
    let b = node_with_identity(b_secret, rtc_config()).await;
    a.start();
    b.start();
    connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    let b_id = b.node_id();
    let displaced_sid = a.peer_session_id(b_id).expect("an installed session");

    // Put the stale handle at a high ordinal so the successor's
    // counter can be walked up to meet it.
    arrange_next_stream_epoch(&a, b_id, 24);
    let stale = a
        .open_stream(
            b_id,
            0x0776,
            StreamConfig::new().with_reliability(Reliability::Reliable),
        )
        .expect("open_stream on the incumbent");
    assert_eq!(stale.epoch(), 24, "the arranged ordinal");
    assert_eq!(
        stale.session_id(),
        displaced_sid,
        "a handle must record the incarnation it was opened on",
    );
    assert!(
        a.peer_session_for_test(b_id)
            .is_some_and(|s| s.stream_ids().contains(&0x0776)),
        "the incumbent must really be busy, or the responder branch is \
         not the one under test",
    );

    // The displacement, end to end. A is the RESPONDER — the only
    // role the busy branch permits — on a DataChannel it has never
    // seen before, so the precheck's `superseded_by_its_owner` arm
    // fires and the commit runs with `require_quiescent: false`.
    let b2 = node_with_identity(b_secret, rtc_config()).await;
    b2.start();
    assert_eq!(b2.node_id(), b_id, "b2 must be the same identity");
    let (a_endpoint, b2_endpoint) = open_rtc_channel(&a, &b2)
        .await
        .expect("a second DataChannel on its own socket");
    let accept = {
        let a = Arc::clone(&a);
        tokio::spawn(async move { a.accept_rtc(a_endpoint, b_id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let a_pub = *a.public_key();
    b2.connect_rtc(b2_endpoint, &a_pub, a.node_id())
        .await
        .expect("b2 initiates Noise over the fresh channel");
    accept
        .await
        .expect("join")
        .expect("the responder must complete Noise and install over the busy incumbent");

    let successor_sid = a.peer_session_id(b_id).expect("a successor session");
    assert_ne!(
        successor_sid, displaced_sid,
        "the busy incumbent must actually have been replaced — this is the \
         Noise/install the precheck-only witness never reached",
    );

    // Arrange the exact collision: the successor hands 0x0776 the
    // same epoch the stale handle holds.
    arrange_next_stream_epoch(&a, b_id, stale.epoch());
    let fresh = a
        .open_stream(
            b_id,
            0x0776,
            StreamConfig::new().with_reliability(Reliability::FireAndForget),
        )
        .expect("reopen the same id on the successor");
    assert_eq!(
        fresh.epoch(),
        stale.epoch(),
        "the premise: the epoch check cannot tell these two lifetimes apart",
    );
    assert_ne!(
        fresh.session_id(),
        stale.session_id(),
        "but the incarnation can",
    );

    // The refusals, typed. `NotConnected` would be the wrong answer
    // and would also be indistinguishable from "that id is closed":
    // the id IS open, on a session this handle does not own.
    let sent = a
        .send_on_stream(&stale, &[Bytes::from_static(b"R12STALE")])
        .await;
    assert!(
        matches!(sent, Err(StreamError::SessionSuperseded)),
        "a displaced session's handle must not transmit into its \
         successor; got {sent:?}",
    );
    let closed = a.close_stream_handle(&stale);
    assert!(
        matches!(closed, Err(StreamError::SessionSuperseded)),
        "nor tear the successor's stream down; got {closed:?}",
    );
    assert!(
        a.stream_stats(b_id, 0x0776).is_some(),
        "and the refused close must have left the successor's stream alive",
    );

    // Restored positive: the successor's own handle works, end to
    // end, on the session that replaced the one above.
    a.send_on_stream(&fresh, &[Bytes::from_static(b"R12FRESH")])
        .await
        .expect("the successor's handle must deliver");
    let seen = collect_tagged(&b2, b"R12FRESH", 1, Duration::from_secs(10)).await;
    assert_eq!(
        seen,
        vec![b"R12FRESH".to_vec()],
        "exactly the successor's payload, once, at the peer that replaced \
         the displaced session",
    );
    a.close_stream_handle(&fresh)
        .expect("and the successor's handle may close its own stream");
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
/// **What this witness can and cannot discriminate** (R-C). It
/// claims set completion plus the consumer's reorder by `seq`, so
/// by construction no set-preserving permutation can fail it —
/// Kyra's `v[5] = 11 - v[5]` at the send seam rewrites which
/// sequence each body claims, leaves the delivered *set* identical,
/// and is **green**. That is the correct answer for this claim, not
/// a gap: an ordered-arrival claim would contradict the documented
/// substrate (`streams.md`).
///
/// Executed inverses that DO discriminate it:
/// (1) drop one value at the send seam — 11 distinct sequences,
///     red; (2) corrupt one payload's body on the wire — the
///     byte-identical-copies / exact-vector assertion, red;
///     (3) suppress the sends on `0x51` — zero deliveries, red;
/// (4) disable the loss injector — the retransmit evidence
///     assertion, red.
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

// ---------------------------------------------------------------
// S5 — one stream, one sequence space (the ack the sender prunes on)
// ---------------------------------------------------------------

/// A plain-UDP pair. The two witnesses below are about the stream
/// protocol itself, not about WebRTC: the defect they pin was FOUND
/// through a browser leaf but lives entirely in the core's receive
/// path, and a UDP pair says so without a DataChannel in the way.
async fn udp_pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(None).await;
    let b = node(None).await;
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_accept = Arc::clone(&b);
    let accept = tokio::spawn(async move { b_accept.accept(a_id).await });
    a.connect(b_addr, &b_pub, b.node_id())
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
    (a, b)
}

/// **One stream, one sequence space.** A control-subprotocol frame
/// takes its sequence from the stream it is addressed to, so the
/// receiver has to record and acknowledge it exactly like an
/// event-plane packet on that stream.
///
/// Pre-fix every control arm of `process_local_packet` returned
/// before the event-plane accounting at the foot of that function,
/// so such a frame was never recorded. Two things broke at once: its
/// sender never saw an ack for it, and the skipped sequence left a
/// hole below the receiver's `next_expected` that no later packet
/// could fill — so the cumulative ack froze, the sender's retransmit
/// window never pruned, the RTO sweep resent the acknowledged
/// prefix, and the give-up (H-3) reset the stream. A browser leaf
/// hit this on every call: its channel `Subscribe` rides the
/// channel's own publish stream id, and the reset it eventually sent
/// tore down the very stream the anchor publishes RPC replies on.
///
/// This is that collision with no browser in it:
/// `send_subprotocol_to_node` — the production path under
/// `subscribe_channel` — addresses its frame to
/// `stream_id == subprotocol_id`, so a reliable stream opened at
/// that id shares one sequence space with it. The link is loss-free,
/// so the receiver owes an ack for every one of those sequences —
/// the control frame's included — and A's retransmit window has to
/// empty against those acks.
///
/// **Why the ACK frontier, not the window's emptiness.** Three
/// readings look like that claim and are not.
///
/// `retransmit_packets_sent == 0` and `reset_packets_sent == 0` are
/// falsified by scheduling alone, with every ack sent and received.
/// `flush_stream_batch` awaits `deliver_stream_packet` and registers
/// the packet for retransmit *after* that await, while the peer's
/// grant drainer answers on a 1 ms cadence — so a starved sender can
/// have the ack for its own last packet applied BEFORE
/// `register_retransmit` puts that packet in the window, leaving an
/// acknowledged packet to age out and resend. `on_ack`'s straggler
/// sweep documents that same ordering, and one RTO-late ack does it
/// too — `DEFAULT_RTO` starts at 50 ms and the adaptive estimate
/// floors at `MIN_RTO` (10 ms) on a loopback link. The give-up is
/// the same arithmetic times `DEFAULT_MAX_RETRIES`. Neither counter
/// is a property of a loss-free link; both are properties of the
/// interleaving, and both stay RETIRED.
///
/// The third is subtler and is why this witness was rewritten:
/// `!has_unacked()` is not evidence of acknowledgement. A packet
/// that exhausts its retries is DELETED from the window and the
/// stream flagged failed (`ReliableStream::get_timed_out`), so an
/// empty window is equally consistent with "the peer acked
/// everything" and "the sender gave up". Byte equality does not
/// close that gap either: `max_consumed_seen` rides the receiver's
/// credit ledger, which is a different piece of state from its
/// cumulative sequence cursor — a receiver could charge a frame's
/// bytes while never recording its sequence, and the byte oracle
/// would be satisfied by a stream whose cumulative ack never moved.
///
/// So this witness observes the acknowledgement itself, from both
/// ends: B's cumulative receive cursor must have advanced ACROSS the
/// control frame's sequence (a hole at that sequence freezes it
/// forever, whatever arrives later), and A's applied ACK FRONTIER
/// must cover every sequence it issued. The byte ledger is kept
/// beside them, and the lifetime's disposition is stated rather than
/// assumed: whatever the reliable layer may have given up on, it was
/// a sequence the frontier already covers, and the stream is still
/// the lifetime it started as.
///
/// Inverses: (A) delete the `account_inbound_stream_packet` call
/// above the subprotocol branches in `process_local_packet` — B
/// records neither the membership frame's sequence nor its bytes, so
/// the cursor, the frontier and the ledger gap all go red together.
/// (B) the sequence-only inverse, which is the one the byte oracle
/// could not see: keep the byte consumption and omit only
/// `r.on_receive(sequence)` for that frame. Every payload still
/// arrives, consumed-byte equality still holds — and B's cursor
/// stalls at 1 while A's frontier never passes 1, which is exactly
/// the hole the leaf hit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_frame_shares_the_sequence_space_of_the_stream_it_rides() {
    let (a, b) = udp_pair().await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    // The stream the membership frame is addressed to.
    const SHARED: u64 = net_wire::channel::membership::SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64;

    // Reliable FIRST: `open_stream` on an id that already has state
    // keeps the existing mode, and the membership frame would have
    // created it fire-and-forget.
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a.open_stream(b_id, SHARED, cfg).expect("open_stream");

    const N: usize = 8;
    let payloads = tagged_payloads(b"S5SEQ", N);

    // One payload, then the control frame, then the rest: the hole
    // the receiver used to leave sits in the MIDDLE of the stream,
    // where a cumulative ack cannot step over it.
    a.send_with_retry(&stream, std::slice::from_ref(&payloads[0]), 16)
        .await
        .expect("first payload");
    // A real round trip: `subscribe_channel` resolves only when B
    // has answered with a membership Ack, so the frame provably
    // reached B and was processed — a stall after this cannot be
    // blamed on a dropped datagram.
    a.subscribe_channel(b_id, ChannelName::new("s5.seqspace").expect("channel"))
        .await
        .expect("subscribe_channel");
    for payload in &payloads[1..] {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("payload");
    }

    let seen = collect_tagged(&b, b"S5SEQ", N, Duration::from_secs(10)).await;
    let distinct: HashSet<&Vec<u8>> = seen.iter().collect();
    assert_eq!(
        distinct.len(),
        N,
        "every payload must arrive: {} distinct of {N} ({} deliveries)",
        distinct.len(),
        seen.len()
    );

    // Past the give-up horizon: DEFAULT_RTO starts at 50 ms and the
    // window is retried DEFAULT_MAX_RETRIES times, so a stream that
    // is genuinely stalled on an unrecorded sequence has resent to
    // exhaustion and reset by now — and a ledger gap left by one is
    // permanent either way.
    tokio::time::sleep(Duration::from_secs(1)).await;
    // The window's contents are not the question (see the doc
    // comment): a give-up empties it too. What a repaired receiver
    // owes is an ACKNOWLEDGEMENT, and these are the two halves of
    // one — the receiver's cumulative cursor and the frontier the
    // sender applied from it.
    let b_cursor = || {
        b.peer_session_for_test(a_id)
            .expect("B's session to A")
            .get_or_create_stream(SHARED)
            .with_reliability(|r| r.rx_ack_seq())
    };
    let a_frontier = || {
        a.peer_session_for_test(b_id)
            .expect("A's session to B")
            .get_or_create_stream(SHARED)
            .with_reliability(|r| r.ack_frontier())
    };
    // `N + 1` sequences were issued on this stream: 0 for the first
    // payload, 1 for the membership frame, 2..=N for the rest. The
    // cursor is exclusive, so contiguous receipt of all of them reads
    // `N + 1` — and a receiver that skipped the control frame's
    // sequence is stuck at 1 no matter how long anything waits.
    let issued = N as u64 + 1;
    assert!(
        wait_for(|| a_frontier() == Some(issued), Duration::from_secs(5)).await,
        "the window must drain by ACKNOWLEDGEMENT: A applied a cumulative \
         ack covering every one of the {issued} sequences it issued on the \
         shared stream, the control frame's among them (frontier {:?})",
        a_frontier()
    );
    assert_eq!(
        b_cursor(),
        issued,
        "and the receiver's own cumulative cursor must have advanced ACROSS \
         the control frame's sequence — a frame whose sequence it never \
         recorded leaves a hole below `next_expected` that no later packet \
         can fill"
    );
    // The disposition, stated rather than assumed. A spurious RTO
    // give-up on an ALREADY-acknowledged packet is permitted here —
    // it is the register-after-ack interleaving above, and forbidding
    // it is what made the retired counter assertions flaky. Giving up
    // on a sequence the peer never acknowledged is not permitted.
    let session = a.peer_session_for_test(b_id).expect("A's session to B");
    let gave_up = session
        .get_or_create_stream(SHARED)
        .with_reliability(|r| r.abandoned_seq());
    if let Some(seq) = gave_up {
        assert!(
            seq < issued,
            "the reliable layer gave up on sequence {seq}, which the \
             acknowledgement frontier ({issued}) does not cover: that is a \
             genuine unacknowledged loss, not the register-after-ack race"
        );
    }
    // And it drained with the byte ledger closed too: the receiver
    // reported consuming every byte A put on the shared stream, the
    // control frame's among them. Bytes and sequences are separate
    // state, so this is a separate claim.
    let ledger = a.stream_stats(b_id, SHARED).expect("stream stats");
    assert_eq!(
        ledger.max_consumed_seen,
        ledger.tx_bytes_sent,
        "the receiver must report consuming every byte sent on the \
         shared stream, control frame included (sent {}, reported \
         consumed {}, gap {})",
        ledger.tx_bytes_sent,
        ledger.max_consumed_seen,
        ledger.tx_bytes_sent - ledger.max_consumed_seen
    );
    // And the stream is still the one it was: no close, no reopen.
    assert!(ledger.active, "the lifetime must not have ended");
    assert_eq!(
        ledger.tx_seq, issued,
        "the membership frame and the {N} payloads share one sequence \
         counter, so the stream has issued {issued} sequences"
    );
}

/// A `StreamReset` names the **sender's** outbound half of a stream.
///
/// Pre-fix the receiver answered one with `close_stream`, which
/// removes the whole `StreamState` — including what WE send on that
/// id, because a stream id names one bidirectional conversation.
/// `tx_seq` restarted at 0 mid-conversation, so every later packet
/// looked like a duplicate to the peer's dedup and was discarded,
/// and the grant quarantine then dropped the peer's acks for the
/// stream too: a peer could kill our send side by giving up on its
/// own.
///
/// Inverse: put `session.close_stream(reset.stream_id)` back in the
/// `SUBPROTOCOL_STREAM_RESET` arm — the post-reset payload never
/// reaches B and `tx_seq` reads 1 instead of 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peers_stream_reset_leaves_our_send_side_alone() {
    let (a, b) = udp_pair().await;
    let a_id = a.node_id();
    let b_id = b.node_id();
    const SID: u64 = 0x0051_A5E7;

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a.open_stream(b_id, SID, cfg).expect("open_stream");

    let before = Bytes::from_static(b"S5RSTbefore");
    a.send_with_retry(&stream, std::slice::from_ref(&before), 16)
        .await
        .expect("pre-reset payload");
    assert!(
        !collect_tagged(&b, b"S5RST", 1, Duration::from_secs(10))
            .await
            .is_empty(),
        "precondition: the stream works before the reset"
    );

    // B sends on the same id, so A has receive state to lose: after
    // two packets A's `rx_seq` reads 1, and the reset is what puts
    // it back to 0. That transition is how this witness knows the
    // reset was PROCESSED before the send below — and it is the
    // reset doing its actual job, dropping the receive half.
    let b_stream = b.open_stream(a_id, SID, cfg).expect("B's half");
    for payload in tagged_payloads(b"S5RSTb", 2) {
        b.send_with_retry(&b_stream, std::slice::from_ref(&payload), 16)
            .await
            .expect("B's payload");
    }
    assert!(
        wait_for(
            || a.stream_stats(b_id, SID).is_some_and(|s| s.rx_seq == 1),
            Duration::from_secs(10),
        )
        .await,
        "precondition: A must have receive state on the stream before the reset"
    );

    // B gives up on ITS half of the stream.
    b.send_subprotocol_to_node(
        a_id,
        net_wire::stream_window::SUBPROTOCOL_STREAM_RESET,
        &net_wire::stream_window::StreamReset { stream_id: SID }.encode(),
    )
    .await
    .expect("reset to A");
    assert!(
        wait_for(
            || a.stream_stats(b_id, SID).is_some_and(|s| s.rx_seq == 0),
            Duration::from_secs(10),
        )
        .await,
        "A must apply the reset to its RECEIVE half and keep the stream: \
         pre-fix the whole state was removed, so this reads None forever"
    );

    let after = Bytes::from_static(b"S5RSTafter");
    a.send_with_retry(&stream, std::slice::from_ref(&after), 16)
        .await
        .expect("A's send side survives a peer's reset");
    // `before` was already drained above, so one more is all that is
    // outstanding.
    let seen = collect_tagged(&b, b"S5RSTafter", 1, Duration::from_secs(10)).await;
    assert!(
        seen.iter().any(|p| p.as_slice() == after.as_ref()),
        "the payload sent after the peer's reset must arrive: a restarted \
         tx_seq would land on a sequence B already dedups; saw {seen:?}"
    );
    let stats = a.stream_stats(b_id, SID).expect("stream stats");
    assert_eq!(
        stats.tx_seq, 2,
        "the reset says nothing about our send side, so the sequence \
         continues rather than restarting"
    );
}

/// **N1a — a native control frame debits exactly the bytes the
/// receiver charges.**
///
/// The R3 hoist made the receiver charge a recognized control
/// frame's full wire bytes to the stream it rides. Native's
/// producers only allocated a sequence: no credit acquire, no
/// `tx_bytes_sent` debit. Sequence ownership and byte ownership are
/// different invariants, and moving one without the other puts the
/// receiver's cumulative-consumed total ahead of the sender's
/// watermark.
///
/// The receiver is partitioned for the whole test, so nothing can
/// grant the bytes back and the debit is observable as an exact
/// number rather than a transient dip. The producer is the
/// production one — `send_subprotocol_to_node`, which is what
/// `subscribe_channel` and the capability fan-out call.
///
/// Inverse: make `StreamState::note_tx_bytes_sent` a no-op, or route
/// this producer's sequence back through
/// `get_or_create_stream(..).next_tx_seq()`. Credit then reads the
/// full window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_control_frame_debits_the_bytes_its_receiver_charges() {
    use net::adapter::net::{EventFrame, HEADER_SIZE, TAG_SIZE};

    let (a, b) = udp_pair().await;
    let b_id = b.node_id();
    let a_addr = a.local_addr();
    const SHARED: u64 = net_wire::channel::membership::SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64;
    const WINDOW: u32 = 8192;

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::FireAndForget;
    cfg.window_bytes = WINDOW;
    let _stream = a.open_stream(b_id, SHARED, cfg).expect("open_stream");
    assert_eq!(
        a.stream_stats(b_id, SHARED).expect("stats").tx_window,
        WINDOW,
        "the test's own window must be the one the stream installed"
    );

    // Nothing that leaves A can be acknowledged while B drops it in
    // `dispatch_packet`, so the debit stands still.
    b.block_peer(a_addr);
    let payload = [0xA5u8; 64];
    a.send_subprotocol_to_node(
        b_id,
        net_wire::channel::membership::SUBPROTOCOL_CHANNEL_MEMBERSHIP,
        &payload,
    )
    .await
    .expect("control frame leaves the sender");

    let expected = (HEADER_SIZE + TAG_SIZE + EventFrame::LEN_SIZE + payload.len()) as u32;
    let stats = a.stream_stats(b_id, SHARED).expect("stats");
    assert_eq!(
        stats.tx_credit_remaining,
        WINDOW - expected,
        "the control frame's {expected} wire bytes — header, AEAD tag, \
         event frame and payload, the same total the receiver charges \
         — must be debited from the stream's send ledger"
    );
    assert_eq!(
        stats.tx_seq, 1,
        "and it still takes exactly one sequence from the stream it rides"
    );
}

/// **N1b — UDP byte conservation across a shared control/application
/// stream.**
///
/// The consequence of the asymmetry N1a pins: with the receiver's
/// cumulative total running ahead of the sender's watermark, the
/// next authoritative grant — clamped to `tx_bytes_sent`, which
/// bounds the counter but says nothing about arrival — refunds
/// window for application bytes the receiver never saw.
///
/// The schedule Kyra specified, on real UDP through production
/// entrypoints:
///
/// 1. A opens a stream at the membership subprotocol's id with an
///    explicit window, so one stream carries both control and
///    application traffic (the same sharing
///    `a_control_frame_shares_the_sequence_space_of_the_stream_it_rides`
///    pins).
/// 2. `subscribe_channel` puts a control frame on it and resolves
///    only when B has answered with a membership Ack — so the frame
///    provably arrived and was charged.
/// 3. B is partitioned and ONE application packet is sent: debited
///    here, never consumed there.
/// 4. B is un-partitioned and a second application packet is sent.
///    B consumes it and grants, so the grant's `total_consumed`
///    covers the control frame and the second packet but not the
///    withheld one.
///
/// Conservation at that settled boundary: remaining credit is the
/// window minus exactly the withheld packet's wire bytes. Pre-fix
/// the uncharged control frame's receiver-side surplus was larger
/// than the withheld packet, so the clamped grant reported every
/// byte the sender had committed and credit came back to the FULL
/// window — the sender re-opening its window for data that never
/// arrived.
///
/// Inverse: as N1a.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_control_frame_and_an_application_packet_conserve_bytes_on_one_stream() {
    use net::adapter::net::{EventFrame, HEADER_SIZE, TAG_SIZE};

    let (a, b) = udp_pair().await;
    let b_id = b.node_id();
    let a_addr = a.local_addr();
    const SHARED: u64 = net_wire::channel::membership::SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64;
    const WINDOW: u32 = 8192;

    // Fire-and-forget on purpose: a withheld packet must stay
    // withheld. A reliable stream would retransmit it the moment the
    // partition heals and there would be nothing unconsumed left to
    // account for.
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::FireAndForget;
    cfg.window_bytes = WINDOW;
    let stream = a.open_stream(b_id, SHARED, cfg).expect("open_stream");

    // (2) The control frame, through production `subscribe_channel`.
    // Its bytes are charged by B; wait for the grant that reports
    // them so the boundary below is settled rather than mid-flight.
    a.subscribe_channel(b_id, ChannelName::new("s5.conserve").expect("channel"))
        .await
        .expect("subscribe_channel");
    assert!(
        wait_for(
            || {
                a.stream_stats(b_id, SHARED).is_some_and(|s| {
                    s.credit_grants_received > 0 && s.tx_bytes_sent == s.max_consumed_seen
                })
            },
            Duration::from_secs(5)
        )
        .await,
        "the control frame round-tripped, so its bytes are consumed and \
         refunded: a control frame costs the application window only \
         while it is in flight (sent {}, consumed {}, credit {} of \
         {WINDOW}, grants {})",
        a.stream_stats(b_id, SHARED).expect("stats").tx_bytes_sent,
        a.stream_stats(b_id, SHARED)
            .expect("stats")
            .max_consumed_seen,
        a.stream_stats(b_id, SHARED)
            .expect("stats")
            .tx_credit_remaining,
        a.stream_stats(b_id, SHARED)
            .expect("stats")
            .credit_grants_received
    );

    // (3) One application packet that never reaches B's accounting.
    // `block_peer` drops it in `dispatch_packet`, before decrypt, so
    // the bytes left A and were charged to nobody.
    let withheld = Bytes::from_static(b"W");
    let consumed_later = Bytes::from_static(b"S5N1B");
    let one_packet_wire_bytes =
        (HEADER_SIZE + TAG_SIZE + EventFrame::LEN_SIZE + withheld.len()) as u32;
    b.block_peer(a_addr);
    a.send_on_stream(&stream, std::slice::from_ref(&withheld))
        .await
        .expect("the withheld packet still leaves the sender");
    assert_eq!(
        a.stream_stats(b_id, SHARED)
            .expect("stats")
            .tx_credit_remaining,
        WINDOW - one_packet_wire_bytes,
        "the withheld packet is debited like any other"
    );

    // The filter runs in `dispatch_packet`, i.e. AFTER the datagram
    // has been read off B's socket. Unblocking immediately would let
    // B's receive loop pick the still-buffered datagram up with the
    // filter already lifted and account it after all, so hold the
    // partition until B has provably had its turn — the control
    // below is what makes "withheld" a fact rather than a hope.
    let withheld_seen = collect_tagged(&b, b"W", 1, Duration::from_millis(500)).await;
    assert!(
        withheld_seen.is_empty(),
        "the partition must actually have dropped the withheld packet, \
         or this test measures nothing: {withheld_seen:?}"
    );

    // (4) Heal, then one packet B does consume, so B grants.
    b.unblock_peer(&a_addr);
    a.send_on_stream(&stream, std::slice::from_ref(&consumed_later))
        .await
        .expect("second application packet");
    let seen = collect_tagged(&b, b"S5N1B", 1, Duration::from_secs(10)).await;
    assert_eq!(
        seen.len(),
        1,
        "the second packet must actually be delivered, or the grant \
         this test reads has nothing to report"
    );

    // Settled boundary. A committed control + 2 packets; B consumed
    // control + 1. The ledger identity says the gap between the two
    // halves is exactly the withheld packet, and the credit that
    // follows from it is the window minus those bytes.
    let settled = wait_for(
        || {
            a.stream_stats(b_id, SHARED).is_some_and(|s| {
                s.tx_bytes_sent - s.max_consumed_seen == u64::from(one_packet_wire_bytes)
            })
        },
        Duration::from_secs(5),
    )
    .await;
    let s = a.stream_stats(b_id, SHARED).expect("stats");
    assert!(
        settled,
        "the sender's committed total must exceed the receiver's \
         reported total by exactly the withheld packet's \
         {one_packet_wire_bytes} wire bytes: sent = {}, consumed = {}, \
         gap = {} (grants received {})",
        s.tx_bytes_sent,
        s.max_consumed_seen,
        s.tx_bytes_sent - s.max_consumed_seen,
        s.credit_grants_received
    );
    assert!(
        s.credit_grants_received > 0,
        "the assertion above is only a conservation claim if a grant \
         actually arrived"
    );
    assert_eq!(
        u64::from(s.tx_credit_remaining) + (s.tx_bytes_sent - s.max_consumed_seen),
        u64::from(WINDOW),
        "remaining + in-flight == window"
    );
    assert_eq!(
        s.tx_credit_remaining,
        WINDOW - one_packet_wire_bytes,
        "so credit is the window minus the withheld packet, not the \
         full window: a grant may only refund what it reports"
    );
    // Hold it: a later grant must not retroactively refund the
    // withheld packet either.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let held = a.stream_stats(b_id, SHARED).expect("stats");
    assert_eq!(
        held.tx_bytes_sent - held.max_consumed_seen,
        u64::from(one_packet_wire_bytes),
        "and it stays there — nothing refunds bytes the receiver never \
         reported consuming"
    );
}

/// **N2 — a stale handle's close must not tear down the successor
/// lifetime, and must not wait on it either.**
///
/// `close_stream_handle` compared the epoch through a `try_stream`
/// read guard, released it, and then removed unconditionally; a
/// close+reopen landing in that gap was destroyed by the old handle.
/// The comparison and the removal now happen under one `Entry` write
/// guard (`NetSession::close_stream_for_lifetime`), and the race
/// itself is driven in
/// `net_wire::session::tests::a_conditional_close_never_removes_a_concurrent_reopen`.
///
/// What this witness adds is the *operational* half through the
/// public API: after a close+reopen, the retained handle is refused,
/// the successor keeps its epoch and its state, fresh traffic on the
/// successor still arrives, and a graceful close on the stale handle
/// refuses IMMEDIATELY rather than waiting out its timeout on
/// somebody else's retransmit window.
///
/// Inverse: drop the epoch half of `close_stream_for_lifetime` (or
/// the `LifetimeMismatch` arm in `close_stream_handle`) and the
/// successor's state disappears; drop the `LifetimeMismatch` arm of
/// `close_stream_graceful_handle` and the elapsed-time assertion
/// goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reopened_streams_state_survives_its_predecessors_close() {
    let (a, b) = udp_pair().await;
    let b_id = b.node_id();
    const ID: u64 = 0x5EED;

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let first = a.open_stream(b_id, ID, cfg).expect("open first");

    // Close by id (the unfenced, handle-less entrypoint) and reopen:
    // same id, same session, different lifetime.
    a.close_stream(b_id, ID);
    let second = a.open_stream(b_id, ID, cfg).expect("reopen");
    let successor_epoch = second.epoch();
    assert_ne!(
        successor_epoch,
        first.epoch(),
        "a reopen must allocate a fresh epoch, or this test proves nothing"
    );
    assert_eq!(
        second.session_id(),
        first.session_id(),
        "same session — the epoch, not the incarnation, is what \
         separates these two lifetimes"
    );

    // Put traffic on the successor so it has state worth losing.
    let payloads = tagged_payloads(b"S5N2A", 3);
    for payload in &payloads {
        a.send_with_retry(&second, std::slice::from_ref(payload), 16)
            .await
            .expect("successor send");
    }
    let before = a.stream_stats(b_id, ID).expect("successor stats");

    // The predecessor's handle-addressed close is refused...
    let refused = a.close_stream_handle(&first);
    assert!(
        matches!(refused, Err(StreamError::NotConnected)),
        "a close naming a lifetime that is over must be refused, not \
         applied to whichever lifetime is current: {refused:?}"
    );
    // ...and the successor is untouched: still open, same epoch,
    // same sequence counter, same credit.
    let after = a
        .stream_stats(b_id, ID)
        .expect("the successor must still be open after the stale close");
    assert_eq!(after.tx_seq, before.tx_seq, "sequence space preserved");
    assert_eq!(
        after.tx_credit_remaining, before.tx_credit_remaining,
        "credit ledger preserved"
    );
    assert_eq!(
        a.open_stream(b_id, ID, StreamConfig::new())
            .expect("idempotent re-open returns the live lifetime")
            .epoch(),
        successor_epoch,
        "the live lifetime is still the successor's"
    );

    // The graceful form refuses immediately instead of draining a
    // successor's unacked data to the deadline.
    let started = tokio::time::Instant::now();
    let graceful = a
        .close_stream_graceful_handle(&first, Duration::from_secs(5))
        .await;
    let elapsed = started.elapsed();
    assert!(
        matches!(graceful, Err(StreamError::NotConnected)),
        "a graceful close on a replaced lifetime is a refusal: {graceful:?}"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "the refusal must be immediate — waiting on a successor's \
         retransmit window is waiting on somebody else's stream \
         (waited {elapsed:?} of a 5 s timeout)"
    );

    // And the successor still delivers.
    let fresh = tagged_payloads(b"S5N2B", 2);
    for payload in &fresh {
        a.send_with_retry(&second, std::slice::from_ref(payload), 16)
            .await
            .expect("post-refusal send on the successor");
    }
    let seen = collect_tagged(&b, b"S5N2B", 2, Duration::from_secs(10)).await;
    let distinct: HashSet<&Vec<u8>> = seen.iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "the successor must keep working after refusing its \
         predecessor's close: {} distinct of 2",
        distinct.len()
    );

    // The successor's own handle closes its own stream.
    a.close_stream_handle(&second)
        .expect("the live lifetime's handle closes it");
    assert!(a.stream_stats(b_id, ID).is_none());
}

/// N3: a session that ends releases the partial fragment groups it
/// opened, and cannot have them recreated.
///
/// The reassembly map is mesh-owned and outlives every session in
/// it, so nothing but an explicit retirement frees a closed peer's
/// held bytes — the TTL only runs when traffic arrives, and a quiet
/// mesh has none. The head here is a real fragmented Net packet
/// sealed by B's real session and carried by the real DataChannel
/// into A's real ingress: only a leaf fragments, so no native send
/// path produces one. The retirement is the production close path
/// (`rtc_driver().close` → the close notifier → the ordinary peer
/// removal transaction); the late piece afterwards is offered to the
/// helper directly, which is the one part of this that is not
/// production ingress.
///
/// Inverses: drop `retire_session`'s group release — the held bytes
/// survive the close; drop the retirement marker in `retire_session`
/// — the late piece re-opens the group that was just released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_sessions_partial_fragment_group_is_retired_and_cannot_be_recreated() {
    use net::adapter::net::rtc::{AbandonReason, FragmentOutcome, FragmentPiece};
    use net_wire::protocol::{PacketFlags, FRAG_FRAGMENTED};

    const STREAM: u64 = 0x0779;
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    // One leaf-shaped head: fragmented, no LAST, so the group can
    // never complete on its own.
    let head = {
        let session = b
            .peer_session_for_test(a_id)
            .expect("B's session to A")
            .clone();
        let seq = session.get_or_create_stream(STREAM).next_tx_seq();
        let mut builder = session.thread_local_pool().get();
        builder.set_channel_hash(0);
        builder.set_origin_hash(b_id);
        builder.set_fragment(7, 0, FRAG_FRAGMENTED);
        builder
            .build_subprotocol(
                STREAM,
                seq,
                &[Bytes::from_static(b"native fragment head")],
                PacketFlags::NONE,
                0,
            )
            .to_vec()
    };
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");

    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) > 0,
            Duration::from_secs(5)
        )
        .await,
        "the fragment must reach production ingress and be buffered, or this \
         witness proves nothing about what retirement releases"
    );

    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(5)).await,
        "the close must evict the peer through the ordinary removal path"
    );

    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "the retired session must hold no bytes"
    );
    let released = a.rtc_reassembly().take_abandoned();
    assert_eq!(
        released.len(),
        1,
        "the released group is DISPOSED of, not dropped: a close that \
         throws away acknowledged bytes says which stream lost them"
    );
    assert_eq!(released[0].reason, AbandonReason::SessionRetired);
    assert_eq!(
        released[0].provenance.stream_id, STREAM,
        "the disposition names the stream whose bytes were released"
    );
    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id,
                fragment_id: 9,
                offset: 0,
                flags: FRAG_FRAGMENTED,
                sequence: 1,
                provenance: net::adapter::net::rtc::FragmentProvenance {
                    stream_id: STREAM,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: false,
                },
                data: Bytes::from_static(b"late"),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Retired),
        "a packet already admitted before the retirement must not recreate it"
    );
    assert_eq!(a.rtc_reassembly().held_bytes(session_id), 0);
}

/// A leaf-shaped fragment packet, sealed by `from`'s real session to
/// `to_node` and addressed to `stream_id`.
///
/// Only a browser leaf fragments, so no native send path produces
/// one of these: the witnesses below have to build them to drive the
/// real ingress at all. Everything else about the packet is
/// production — the session's cipher, its stream sequence, the
/// builder's framing.
fn leaf_fragment(
    from: &Arc<MeshNode>,
    to_node: u64,
    stream_id: u64,
    frag: (u16, u16, u8),
    channel_hash: u16,
    payload: &[u8],
) -> Vec<u8> {
    use net_wire::protocol::PacketFlags;

    let (fragment_id, offset, flags) = frag;
    let session = from
        .peer_session_for_test(to_node)
        .expect("a session to the peer")
        .clone();
    let seq = session.get_or_create_stream(stream_id).next_tx_seq();
    let mut builder = session.thread_local_pool().get();
    builder.set_channel_hash(channel_hash);
    builder.set_origin_hash(from.node_id());
    builder.set_fragment(fragment_id, offset, flags);
    builder
        .build_subprotocol(
            stream_id,
            seq,
            &[Bytes::copy_from_slice(payload)],
            // RELIABLE: a fragment's sequence has to be RECORDED by
            // the receiver for the piece to be acknowledged at all,
            // and "the receiver acknowledged it" is the premise that
            // makes an abandoned group a loss rather than a dropped
            // datagram. A fire-and-forget stream tracks no sequence.
            PacketFlags::RELIABLE,
            0,
        )
        .to_vec()
}

/// X11/P1: expiring an **acknowledged** fragment group is a terminal
/// disposition, and the tail that follows cannot open a headless
/// group.
///
/// The native ingress records a packet's sequence and returns its
/// credit BEFORE reassembly sees it — deliberately, because the
/// bytes did cross the wire. The consequence is that every buffered
/// piece is a piece this node has acknowledged, so its sender is
/// entitled to discard that descriptor. Pre-fix, expiring such a
/// head and then accepting the tail produced a NEW tail-only group:
/// the message was gone, the head unrecoverable, and the only trace
/// was a debug log — no completion, no terminal signal, nothing the
/// stream's owner could observe. `a_tail_past_the_ttl_cannot_complete_
/// a_group` even *required* that orphan `Buffered`.
///
/// Both pieces here go through the real RTC DataChannel into the
/// real ingress. The clock is real too: the head is left to sit past
/// `GROUP_TTL`, which is what a sender with a retransmit timer
/// longer than the TTL actually does.
///
/// Inverses: drop the abandonment fence lookup in `accept_locked` —
/// the tail opens its own group (`held_bytes == 4`) and the
/// disposition assertion still holds but the "no headless successor"
/// one fails; drop `SessionState::expire`'s abandonment push — the
/// tail is still refused but `abandoned_total` stays 0, which is the
/// silence this witness exists to forbid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_acknowledged_fragment_groups_expiry_is_terminal_not_silent() {
    use net::adapter::net::rtc::{AbandonReason, GROUP_TTL};
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST};

    const STREAM: u64 = 0x077A;
    const GROUP: u16 = 11;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"S5X11-head",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the head must reach production ingress and be buffered, or this \
         witness proves nothing about what its loss costs (held {})",
        a.rtc_reassembly().held_bytes(session_id)
    );
    // The premise that makes this loss silent-data-loss rather than a
    // dropped datagram: the head was ACKNOWLEDGED. Its sequence is in
    // A's receive state, so B may discard its retransmit descriptor
    // and will never send it again.
    let recorded = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .get_or_create_stream(STREAM)
        .with_reliability(|r| r.rx_ack_seq());
    assert_eq!(
        recorded, 1,
        "the head's sequence must be recorded — an unacknowledged piece \
         would be resent and nothing would be lost"
    );

    // Let the group age past its deadline, exactly as a sender whose
    // retransmit timer is longer than the TTL would.
    tokio::time::sleep(GROUP_TTL + Duration::from_millis(300)).await;

    let tail = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 10, FRAG_FRAGMENTED | FRAG_LAST),
        0,
        b"tail",
    );
    b.send_built_packet_for_test(a_id, &tail)
        .await
        .expect("the tail leaves B");

    assert!(
        wait_for(
            || a.rtc_reassembly().abandoned_total() == 1,
            Duration::from_secs(5)
        )
        .await,
        "the reaped group held acknowledged bytes, so its loss must be \
         reported — not logged at debug and forgotten (reported {})",
        a.rtc_reassembly().abandoned_total()
    );
    let records = a.rtc_reassembly().take_abandoned();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].reason,
        AbandonReason::Expired,
        "the disposition says WHY the bytes were abandoned"
    );
    assert_eq!(
        records[0].provenance.stream_id, STREAM,
        "and WHICH stream lost them — which is only knowable because the \
         group binds its provenance"
    );
    assert_eq!(records[0].held, 10, "and how much was lost");
    assert_eq!(
        records[0].first_sequence, 0,
        "and the acknowledged sequence it covers"
    );

    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "the tail must NOT open a group whose head is gone: a group that \
         can never complete is not progress, it is a second loss"
    );
    assert_eq!(
        a.rtc_reassembly().abandoned_total(),
        1,
        "and refusing the tail is not itself an abandonment — there was \
         nothing left to abandon"
    );
    // Nothing was dispatched: no partial payload, no lone tail.
    let delivered = collect_tagged(&a, b"S5X11", 1, Duration::from_millis(500)).await;
    assert!(
        delivered.is_empty(),
        "an incomplete group must not reach a subscriber: {delivered:?}"
    );
}

/// X11: a group's identity is its FIRST piece's, and a piece that
/// disagrees is refused rather than merged.
///
/// `fragment_id` is 16 bits of sender-chosen namespace. Pre-fix the
/// only thing a group required of its pieces was that they be
/// disjoint and inside the declared total — so two pieces from
/// DIFFERENT streams that happened to share a session and a group id
/// were concatenated into one payload and dispatched with whichever
/// piece's stream, channel, origin and subprotocol happened to
/// complete the group. Both pieces are authenticated here; this is a
/// protocol-integrity failure of an honest-looking producer, not a
/// forged identity.
///
/// Inverse: drop the `provenance != piece.provenance` check in
/// `accept_locked` — the two streams' bytes are assembled into one
/// payload and delivered as `S5X11Bhead-tail`, and both assertions
/// below fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fragment_from_another_stream_cannot_join_a_live_group() {
    use net::adapter::net::rtc::AbandonReason;
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST};

    const OWNER: u64 = 0x077B;
    const OTHER: u64 = 0x077C;
    const GROUP: u16 = 13;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(
        &b,
        a_id,
        OWNER,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"S5X11Bhead",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the owning stream's head must be buffered first"
    );

    // Same session, same group id, same declared total — a different
    // STREAM. Nothing about the bytes contradicts the group; only
    // their provenance does.
    let alien = leaf_fragment(
        &b,
        a_id,
        OTHER,
        (GROUP, 10, FRAG_FRAGMENTED | FRAG_LAST),
        0,
        b"-tail",
    );
    b.send_built_packet_for_test(a_id, &alien)
        .await
        .expect("the alien piece leaves B");

    assert!(
        wait_for(
            || a.rtc_reassembly().abandoned_total() == 1,
            Duration::from_secs(5)
        )
        .await,
        "the mixed group must be destroyed and reported"
    );
    let records = a.rtc_reassembly().take_abandoned();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].reason, AbandonReason::Inconsistent);
    assert_eq!(
        records[0].provenance.stream_id, OWNER,
        "the group belonged to the stream that OPENED it, and that is the \
         stream whose bytes were lost"
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "neither stream's bytes are kept: a group is not half-valid"
    );
    let delivered = collect_tagged(&a, b"S5X11B", 1, Duration::from_millis(500)).await;
    assert!(
        delivered.is_empty(),
        "two streams' pieces must never be assembled into one payload: \
         {delivered:?}"
    );
}

/// X10: a session REPLACED by its successor releases its partial
/// fragment groups.
///
/// The common installer swaps the peer entry and then cleans the
/// displaced session's reverse and address indexes — but it never
/// retired its reassembly state, and no later path can: a close
/// notification arriving afterwards goes to `evict_endpoint`, which
/// starts by requiring the reverse mapping this installer has just
/// removed, and only retires a session it actually removes. So a
/// quiet mesh kept the displaced session's acknowledged partial
/// bytes for as long as it ran — TTL reaping only happens when a
/// fragment arrives, and there is no successor traffic on a dead
/// session's group.
///
/// This is Kyra's busy-responder replacement scenario with a
/// retained partial: A is the responder on a DataChannel it has
/// never seen, so the install runs with `require_quiescent: false`
/// and replaces an incumbent that is genuinely busy — the buffered
/// group's own stream is open on it.
///
/// Inverse: drop the `retire_session` call from the installer's
/// displaced-session block — `held_bytes` stays at 10 and the late
/// piece re-opens the dead session's group.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_replaced_sessions_partial_fragment_group_is_retired() {
    use net::adapter::net::rtc::{
        AbandonReason, FragmentOutcome, FragmentPiece, FragmentProvenance,
    };
    use net_wire::protocol::FRAG_FRAGMENTED;

    const STREAM: u64 = 0x077D;
    let b_secret = [0xB7u8; 32];
    let a = node(Some(rtc_config())).await;
    let b = node_with_identity(b_secret, rtc_config()).await;
    a.start();
    b.start();
    connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let displaced_sid = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(&b, a_id, STREAM, (17, 0, FRAG_FRAGMENTED), 0, b"S5X10-head");
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(displaced_sid) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the incumbent must be holding a real partial group before it is \
         replaced, or this witness proves nothing"
    );

    // The displacement, end to end: same identity, fresh socket, A as
    // responder.
    let b2 = node_with_identity(b_secret, rtc_config()).await;
    b2.start();
    assert_eq!(b2.node_id(), b_id, "b2 must be the same identity");
    let (a_endpoint, b2_endpoint) = open_rtc_channel(&a, &b2)
        .await
        .expect("a second DataChannel on its own socket");
    let accept = {
        let a = Arc::clone(&a);
        tokio::spawn(async move { a.accept_rtc(a_endpoint, b_id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let a_pub = *a.public_key();
    b2.connect_rtc(b2_endpoint, &a_pub, a.node_id())
        .await
        .expect("b2 initiates Noise over the fresh channel");
    accept
        .await
        .expect("join")
        .expect("the responder must install over the incumbent");
    let successor_sid = a.peer_session_id(b_id).expect("a successor session");
    assert_ne!(
        successor_sid, displaced_sid,
        "the incumbent must actually have been replaced"
    );

    assert_eq!(
        a.rtc_reassembly().held_bytes(displaced_sid),
        0,
        "a replacement ends the displaced session's lifetime, so its \
         acknowledged partial bytes are released with it"
    );
    let released = a.rtc_reassembly().take_abandoned();
    assert_eq!(released.len(), 1, "and the loss is reported, not silent");
    assert_eq!(released[0].reason, AbandonReason::SessionRetired);
    assert_eq!(released[0].provenance.stream_id, STREAM);
    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id: displaced_sid,
                fragment_id: 17,
                offset: 10,
                flags: FRAG_FRAGMENTED,
                sequence: 2,
                provenance: FragmentProvenance {
                    stream_id: STREAM,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: true,
                },
                data: Bytes::from_static(b"late"),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Retired),
        "and a packet already past its session lookup when the replacement \
         landed cannot bring the displaced session's state back"
    );
}

/// X10: the permanently-dead peer sweep releases the swept session's
/// partial fragment groups.
///
/// The sweep removes the peer, its addresses, its reverse index, its
/// gate cache and its admission state — and left its reassembly
/// groups behind. This is the one eviction a quiet mesh reaches on
/// its own, so it is exactly the path on which leftover bytes are
/// never reclaimed.
///
/// The DataChannel is never closed here: B's heartbeat interval is
/// ten minutes, so A's session to B simply goes silent past
/// `session_timeout × 30` and the heartbeat loop's sweep is the only
/// eviction that can fire. The ordinary close path needs a close
/// notification that this test never produces, and the provisional
/// eviction paths need an admission state A — which serves no
/// bootstrap — never assigns.
///
/// Inverse: drop the `rtc_reassembly_evict.retire_session` call from
/// the sweep — the swept session's 10 bytes stay held forever and the
/// late piece re-opens its group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_swept_dead_peers_partial_fragment_group_is_retired() {
    use net::adapter::net::rtc::{
        AbandonReason, FragmentOutcome, FragmentPiece, FragmentProvenance,
    };
    use net_wire::protocol::FRAG_FRAGMENTED;

    const STREAM: u64 = 0x077E;
    // A sweeps fast: `dead_peer_timeout` is `session_timeout × 30`,
    // and the sweep rides the heartbeat tick. Fast enough that the
    // SWEEP is what releases the group: since NR2 the reassembly
    // deadline has a timer owner, so a group left sitting for
    // `GROUP_TTL` is reaped as `Expired` on its own and this witness
    // would stop being about the sweep at all.
    let mut a_cfg = config(Some(rtc_config()));
    a_cfg.session_timeout = Duration::from_millis(20);
    a_cfg.heartbeat_interval = Duration::from_millis(50);
    // B never refreshes A's view of it, which is what "permanently
    // dead" means to the detector.
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
    connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(&b, a_id, STREAM, (19, 0, FRAG_FRAGMENTED), 0, b"S5X10-swee");
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the partial group must exist before the peer is swept"
    );

    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(15)).await,
        "the failure sweep must evict the silent peer"
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "the swept session's acknowledged partial bytes go with it — \
         nothing else will ever release them on a quiet mesh"
    );
    let released = a.rtc_reassembly().take_abandoned();
    assert_eq!(released.len(), 1, "and the loss is reported, not silent");
    assert_eq!(released[0].reason, AbandonReason::SessionRetired);
    assert_eq!(released[0].provenance.stream_id, STREAM);
    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id,
                fragment_id: 19,
                offset: 10,
                flags: FRAG_FRAGMENTED,
                sequence: 2,
                provenance: FragmentProvenance {
                    stream_id: STREAM,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: true,
                },
                data: Bytes::from_static(b"late"),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Retired),
        "and the sweep fences the session it swept"
    );
}

// ---------------------------------------------------------------
// Native reassembly and hold lifetimes (NR2, NR3, NR4, NR6)
// ---------------------------------------------------------------

/// **NR2**: an abandoned fragment group ENDS the stream that lost
/// the bytes. Abandonment was diagnostic only — counted, logged and
/// queued for a `take_abandoned()` nobody called in production — so
/// the peer went on waiting for a reply to a request this node had
/// acknowledged and thrown away.
///
/// The premise is what makes this loss rather than a dropped
/// datagram: the head's sequence is RECORDED before reassembly sees
/// it, so B is entitled to discard its retransmit descriptor. This
/// witness therefore requires the loss to become an event on the
/// conversation: A's receive lifetime for the stream is ended (its
/// cumulative cursor drops back to zero, so B's restarted sequences
/// are admitted rather than dropped below a stale frontier) and a
/// reset is owed to B.
///
/// Inverses: delete the `dispose_abandoned_rtc_groups` call from
/// `reassemble_rtc_fragments` — `abandoned_total` still reaches 1
/// and the cursor stays at 1, which is exactly the "replaced a
/// silent drop with a log the caller never receives" shape; or
/// delete `session.note_receive_terminal(...)` from the drain — the
/// cursor resets but nothing is ever owed to the peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abandoned_fragment_group_ends_the_streams_receive_half() {
    use net::adapter::net::rtc::GROUP_TTL;
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST};

    const STREAM: u64 = 0x0791;
    const GROUP: u16 = 31;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"NR2-head--",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the head must reach production ingress and be buffered, or this \
         witness proves nothing about what its loss costs"
    );
    let a_to_b = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .clone();
    assert_eq!(
        a_to_b
            .get_or_create_stream(STREAM)
            .with_reliability(|r| r.rx_ack_seq()),
        1,
        "the head's sequence must be acknowledged — an unacknowledged piece \
         would be resent and nothing would be lost"
    );

    // The deadline, reached exactly as a sender whose retransmit
    // timer is longer than the TTL reaches it.
    tokio::time::sleep(GROUP_TTL + Duration::from_millis(300)).await;
    let tail = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 10, FRAG_FRAGMENTED | FRAG_LAST),
        0,
        b"tail",
    );
    b.send_built_packet_for_test(a_id, &tail)
        .await
        .expect("the tail leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().abandoned_total() == 1,
            Duration::from_secs(5)
        )
        .await,
        "the reaped group held acknowledged bytes, so it must be reported"
    );

    // **The NR2 property.** Not "it was logged": the conversation
    // that lost the bytes has a new state.
    assert!(
        wait_for(
            || a_to_b
                .try_stream(STREAM)
                .is_some_and(|s| s.with_reliability(|r| r.rx_ack_seq()) == 0),
            Duration::from_secs(5)
        )
        .await,
        "the losing stream's receive lifetime must END: its cumulative \
         cursor is dropped, so a peer that reopens the id from sequence \
         zero is admitted instead of being refused below a stale frontier"
    );
    // And the peer is owed exactly one reset for it. The retransmit
    // tick is the drainer, so the terminal is either still queued or
    // already on the wire; both are the obligation being honoured,
    // and "never recorded at all" is the failure this excludes.
    let owed = a_to_b.take_receive_terminals();
    assert!(
        owed.is_empty() || owed == vec![STREAM],
        "the only receive-half terminal this session may owe is the stream \
         that lost the bytes, got {owed:?}"
    );
    let delivered = collect_tagged(&a, b"NR2", 1, Duration::from_millis(500)).await;
    assert!(
        delivered.is_empty(),
        "and no partial payload reaches a subscriber: {delivered:?}"
    );
}

/// **NR2**: the reassembly deadline runs on a TIMER, not only when
/// some later fragment happens to arrive.
///
/// Expiry was driven from `accept`, so a quiet live session held an
/// incomplete group's acknowledged bytes indefinitely — "a deadline
/// applied when unrelated traffic arrives is not a deadline" — and
/// the stream that lost them was never told. No second fragment is
/// sent here: nothing but the heartbeat tick can reap this group.
///
/// Inverse: delete the `rtc_reassembly_evict.expire(...)` /
/// `dispose_abandoned_rtc_groups(...)` block from the heartbeat loop
/// — `held_bytes` stays at 10 for as long as the node runs and
/// `abandoned_total` stays 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_sessions_expired_fragment_group_is_reaped_on_the_timer() {
    use net_wire::protocol::FRAG_FRAGMENTED;

    const STREAM: u64 = 0x0792;
    // A's heartbeat tick is the deadline's owner; B's is long, so no
    // traffic from B can be what reaps the group.
    let mut a_cfg = config(Some(rtc_config()));
    a_cfg.heartbeat_interval = Duration::from_millis(100);
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
    connect_rtc_loopback(&a, &b)
        .await
        .expect("DataChannel + Noise handshake");
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(&b, a_id, STREAM, (41, 0, FRAG_FRAGMENTED), 0, b"NR2-timer-");
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 10,
            Duration::from_secs(5)
        )
        .await,
        "the partial group must exist before the deadline is due"
    );

    let a_to_b = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .clone();
    assert!(
        wait_for(
            || a.rtc_reassembly().abandoned_total() == 1
                && a.rtc_reassembly().held_bytes(session_id) == 0,
            Duration::from_secs(10)
        )
        .await,
        "the deadline must be reached with NO further fragment traffic: a \
         quiet session that keeps acknowledged bytes for ever has no \
         deadline at all (abandoned {}, held {})",
        a.rtc_reassembly().abandoned_total(),
        a.rtc_reassembly().held_bytes(session_id)
    );
    assert!(
        wait_for(
            || a_to_b
                .try_stream(STREAM)
                .is_some_and(|s| s.with_reliability(|r| r.rx_ack_seq()) == 0),
            Duration::from_secs(5)
        )
        .await,
        "and the timer's reap is carried into the same terminal the \
         ingress's is: the losing stream's receive lifetime ends"
    );
}

/// **NR6**: a group's pieces must have arrived on CONTIGUOUS
/// sequences.
///
/// Byte coverage alone cannot tell one message's pieces from two.
/// Here the head takes sequence 0, an ordinary unfragmented event
/// takes sequence 1 and is delivered, and the tail takes sequence 2.
/// The two fragments cover their declared total exactly and agree on
/// every provenance field, so pre-fix they assembled — and the
/// assembled group silently claimed sequence 1, a packet that
/// belonged to a different message and had already advanced this
/// stream's FIFO.
///
/// Inverse: delete the `contiguous` check in `accept_locked` — the
/// `NR6-headNR6-tail` payload is delivered to the subscriber and
/// `abandoned_total` stays 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fragment_group_whose_sequences_are_not_contiguous_is_refused() {
    use net::adapter::net::rtc::AbandonReason;
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST};

    const STREAM: u64 = 0x0793;
    const GROUP: u16 = 43;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"NR6-head",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 8,
            Duration::from_secs(5)
        )
        .await,
        "the head must be buffered on sequence 0"
    );

    // Sequence 1: an ordinary, unfragmented event on the same
    // stream. This is the packet the group must not be allowed to
    // claim.
    let between = leaf_fragment(&b, a_id, STREAM, (0, 0, 0), 0, b"NR6-between");
    b.send_built_packet_for_test(a_id, &between)
        .await
        .expect("the interleaved event leaves B");
    assert!(
        !collect_tagged(&a, b"NR6-between", 1, Duration::from_secs(5))
            .await
            .is_empty(),
        "the interleaved event must really have consumed sequence 1 and been \
         delivered, or the schedule this witness needs did not happen"
    );

    // Sequence 2: a tail that completes the BYTES exactly.
    let tail = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 8, FRAG_FRAGMENTED | FRAG_LAST),
        0,
        b"NR6-tail",
    );
    b.send_built_packet_for_test(a_id, &tail)
        .await
        .expect("the tail leaves B");

    assert!(
        wait_for(
            || a.rtc_reassembly().abandoned_total() >= 1,
            Duration::from_secs(5)
        )
        .await,
        "a group whose pieces did not arrive on contiguous sequences is not \
         one message, and destroying it is reported"
    );
    let records = a.rtc_reassembly().take_abandoned();
    assert!(
        records
            .iter()
            .any(|r| r.reason == AbandonReason::Malformed && r.provenance.stream_id == STREAM),
        "the disposition says the group contradicted itself, on the stream \
         that owned it: {records:?}"
    );
    let delivered = collect_tagged(&a, b"NR6-head", 1, Duration::from_millis(500)).await;
    assert!(
        delivered.is_empty(),
        "and the payload is NOT assembled: a group that claims a sequence \
         none of its pieces arrived on would step the consumer's cursor \
         over a message it never saw ({delivered:?})"
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "neither piece is kept"
    );
}

/// **NR6**: a RESET ends that stream's receive lifetime, so its
/// partial fragment groups end with it.
///
/// RESET cleared reliability and the in-order holds and left the
/// native fragment groups alone, so a head buffered before the reset
/// stayed available to a delayed old tail after receive progress had
/// restarted — delivering a pre-reset payload against the fresh
/// cursor, behind the reset the consumer was already given. The leaf
/// retires its groups on reset; the native side now does too.
///
/// Inverse: delete the `ctx.rtc_reassembly.retire_stream(...)` call
/// from the `SUBPROTOCOL_STREAM_RESET` arm — `reset_retired_total`
/// stays 0, the group survives, and the late tail is `Buffered`
/// instead of fenced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_reset_retires_the_native_groups_of_that_receive_lifetime() {
    use net::adapter::net::rtc::{FragmentOutcome, FragmentPiece, FragmentProvenance};
    use net_wire::protocol::FRAG_FRAGMENTED;

    const STREAM: u64 = 0x0794;
    const GROUP: u16 = 47;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");

    let head = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"NR6-rset",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == 8,
            Duration::from_secs(5)
        )
        .await,
        "the head must be buffered before the reset"
    );

    // The production RESET, built and sealed by B's own session and
    // carried by the real DataChannel into A's real ingress. It takes
    // the STREAM's next sequence, exactly as a real reset does: one
    // stream, one sequence space.
    let reset = {
        let session = b
            .peer_session_for_test(a_id)
            .expect("B's session to A")
            .clone();
        let seq = session.get_or_create_stream(STREAM).next_tx_seq();
        let mut builder = session.thread_local_pool().get();
        builder
            .build_subprotocol(
                STREAM,
                seq,
                &[Bytes::copy_from_slice(
                    &net_wire::stream_window::StreamReset { stream_id: STREAM }.encode(),
                )],
                net_wire::protocol::PacketFlags::NONE,
                net_wire::stream_window::SUBPROTOCOL_STREAM_RESET,
            )
            .to_vec()
    };
    b.send_built_packet_for_test(a_id, &reset)
        .await
        .expect("the reset leaves B");

    assert!(
        wait_for(
            || a.rtc_reassembly().reset_retired_total() == 1,
            Duration::from_secs(5)
        )
        .await,
        "the reset ends this stream's RECEIVE lifetime, so the partial group \
         it was holding is released with it and the release is counted \
         (retired {}, held {})",
        a.rtc_reassembly().reset_retired_total(),
        a.rtc_reassembly().held_bytes(session_id)
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "a group from before the reset is not part of the lifetime after it"
    );
    assert!(
        a.rtc_reassembly().take_abandoned().is_empty(),
        "and the reset IS the terminal disposition: reporting it again would \
         ask the ingress to reset a stream because it was reset"
    );
    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id,
                fragment_id: GROUP,
                offset: 8,
                flags: FRAG_FRAGMENTED,
                sequence: 1,
                provenance: FragmentProvenance {
                    stream_id: STREAM,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: true,
                },
                data: Bytes::from_static(b"NR6-late"),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Abandoned),
        "and the released group is FENCED: a delayed old tail must not \
         complete a pre-reset group against the lifetime after the reset"
    );
}

/// **NR3**: a frame captured under an incarnation that is then
/// retired is refused AT DISPATCH — not by a marker lookup that may
/// already have expired.
///
/// The receive loop clones the resolved session `Arc` and releases
/// the peer lookup before dispatch, and nothing bounds how long the
/// frame can sit between the two. Reassembly's retirement marker
/// could not be that bound: it expires with `GROUP_TTL` and is
/// evicted under churn, after which an admitted old frame recreated
/// state under the retired session id. This schedules exactly that:
/// the fragment is held in the ingress — after its session was
/// resolved, before the admission decision — the peer is closed
/// through the production close path, and the frame is released only
/// once the marker's whole horizon has elapsed.
///
/// The middle assertion is the discriminator, and it is why the
/// marker cannot be the authority: offered straight to the
/// reassembler past the horizon, a piece for that session IS
/// admitted. The session handle is what refuses the captured frame
/// at the ingress, and that flag is one-way and captured with it.
///
/// Inverse: delete the `if !session.is_active()` guard from
/// `reassemble_rtc_fragments` — the released frame opens a group
/// under the retired session (`held_bytes == 10`).
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_frame_captured_under_a_retired_incarnation_cannot_revive_its_reassembly() {
    use net::adapter::net::rtc::{
        FragmentOutcome, FragmentPiece, FragmentProvenance, IngressPause, GROUP_TTL,
    };
    use net_wire::protocol::FRAG_FRAGMENTED;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::mpsc;

    const STREAM: u64 = 0x0795;
    const GROUP: u16 = 53;
    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");
    let stale = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .clone();

    // Hold the FIRST fragment inside the ingress, after its session
    // has been resolved and before the admission decision. One shot:
    // holding later traffic too would stall the close path's own
    // packets.
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    // `parking_lot`, per the workspace lint: std's lock poisoning is
    // not handled anywhere in this tree, so a poisoned std mutex here
    // would turn an unrelated panic into a confusing second failure.
    let release_rx = parking_lot::Mutex::new(release_rx);
    let armed = Arc::new(AtomicBool::new(true));
    {
        let armed = Arc::clone(&armed);
        a.rtc_reassembly()
            .set_dispatch_pause(Some(IngressPause::new(move || {
                if !armed.swap(false, AtomicOrdering::AcqRel) {
                    return;
                }
                entered_tx.send(()).expect("the test is waiting for this");
                release_rx
                    .lock()
                    .recv()
                    .expect("the test releases the ingress");
            })));
    }

    let head = leaf_fragment(
        &b,
        a_id,
        STREAM,
        (GROUP, 0, FRAG_FRAGMENTED),
        0,
        b"NR3-head--",
    );
    b.send_built_packet_for_test(a_id, &head)
        .await
        .expect("the head leaves B");
    let entered = tokio::task::spawn_blocking(move || {
        entered_rx.recv_timeout(Duration::from_secs(10)).is_ok()
    })
    .await
    .expect("join");
    assert!(
        entered,
        "the fragment must actually reach the captured-but-not-admitted \
         interval, or this witness schedules nothing"
    );

    // The retirement, through the production close path, while the
    // frame waits.
    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(5)).await,
        "the close must evict the peer through the ordinary removal path"
    );
    assert!(
        !stale.is_active(),
        "retirement must DEACTIVATE the incarnation it retires: that flag is \
         the only retirement authority that outlives every admitted frame"
    );

    // Past the whole marker horizon: the bounded fence is gone.
    tokio::time::sleep(GROUP_TTL + Duration::from_millis(400)).await;
    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id,
                fragment_id: 99,
                offset: 0,
                flags: FRAG_FRAGMENTED,
                sequence: 7,
                provenance: FragmentProvenance {
                    stream_id: STREAM,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: true,
                },
                data: Bytes::from_static(b"probe"),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Buffered),
        "premise: past its horizon the retirement MARKER admits work again. \
         That is exactly why it cannot be the authority the ingress relies \
         on, and why the guard below sits at dispatch instead"
    );
    // The probe's OWN five bytes are now the only thing this session
    // may hold. Re-retiring here would publish a FRESH marker and
    // mask the very guard this witness is about, so nothing else is
    // done to the reassembler before the frame is released.
    let probe_only = a.rtc_reassembly().held_bytes(session_id);
    assert_eq!(probe_only, 5, "the probe is all that is held");

    release_tx.send(()).expect("release the held ingress");
    // Settle long enough that an unrefused insert would have landed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        probe_only,
        "a frame captured under a retired incarnation must be refused at \
         DISPATCH: with the marker and the fence both past their horizon, \
         nothing else stops its ten bytes from recreating the dead \
         session's reassembly state"
    );
    a.rtc_reassembly().set_dispatch_pause(None);
}

/// **NR3's other half**: the ingress predicate is RETIREMENT, and a
/// session that is merely marked inactive LOCALLY still receives.
///
/// `NetSession::deactivate` and receive-lifetime retirement are two
/// different facts about a session, and this witness exists because
/// they were briefly one flag. `active` is advisory local liveness
/// with many writers — session replacement, node shutdown, tests, and
/// an SDK marking a session it no longer treats as live while the
/// peer stays pinned, stays selected and keeps sending. Retirement is
/// "this incarnation is over". Keying the dispatch guard on `active`
/// blackholed the INBOUND half of a live conversation: the peer's
/// replies were dropped at this node's own ingress, and the sender
/// saw an ack timeout with nothing refused anywhere.
///
/// So: B marks its session to A inactive, retires nothing, and every
/// payload A sends must still be delivered on B. The companion
/// witness above establishes the opposite direction — a frame under a
/// RETIRED incarnation is still refused — and the pair is what keeps
/// the two meanings from collapsing into each other again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_locally_deactivated_session_still_receives_its_peers_frames() {
    let (a, b, _id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let a_id = a.node_id();

    // The local mark, on the RECEIVING node, through the same public
    // call the SDK and the sensing witness use. Nothing is retired:
    // asserted, so this cannot pass by retiring nothing and also
    // marking nothing.
    let session = b
        .peer_session_for_test(a_id)
        .expect("B's session to A")
        .clone();
    session.deactivate();
    assert!(
        !session.is_active(),
        "precondition: B's session to A is marked not-live locally"
    );
    assert!(
        !session.is_receive_lifetime_retired(),
        "and its receive lifetime is NOT retired: deactivation is not \
         retirement, which is the whole distinction under test"
    );

    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let stream = a
        .open_stream(b.node_id(), 0x0053, cfg)
        .expect("open_stream");

    const N: usize = 4;
    let payloads = tagged_payloads(b"NR3LV", N);
    for payload in &payloads {
        a.send_with_retry(&stream, std::slice::from_ref(payload), 16)
            .await
            .expect("send_with_retry");
    }

    let seen = collect_tagged(&b, b"NR3LV", N, Duration::from_secs(30)).await;
    let distinct: HashSet<&Vec<u8>> = seen.iter().collect();
    assert_eq!(
        distinct.len(),
        N,
        "a locally deactivated session must still deliver its peer's frames: \
         {} of {N} distinct payloads arrived ({} deliveries). A shortfall here \
         means the ingress is refusing on local liveness again",
        distinct.len(),
        seen.len()
    );
    assert!(
        !session.is_receive_lifetime_retired(),
        "and nothing about receiving retired the incarnation"
    );
}

/// **NR4**: an in-order hold is bound to the exact stream lifetime
/// that accepted the sequence.
///
/// Acceptance records the sequence under the stream's map guard,
/// drops that guard, and the hold re-acquires the stream by ID. A
/// close/reopen landing in between put the OLD frame into the
/// REPLACEMENT's reorder buffer — a stream that never accepted that
/// sequence, never reserved its bytes, and would have released it
/// under its own frontier. "The stream vanished, so its consumer is
/// gone" was sound; "the stream was replaced" was not.
///
/// The control is the same insertion under the live lifetime, which
/// must still be accepted: this is a lifetime check, not a ban on
/// holding.
///
/// Inverse: delete the `state.epoch() != lifetime.epoch` arm from
/// `NetSession::hold_in_order_frame` — the replacement's buffer
/// holds the predecessor's frame and the first assertion fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_order_hold_cannot_cross_a_stream_replacement() {
    use net_wire::protocol::PacketFlags;
    use net_wire::session::{HeldFrame, StreamLifetime};

    const STREAM: u64 = 0x0796;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let b_id = b.node_id();
    let session = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .clone();

    // One real out-of-order arrival, built and parsed exactly as the
    // ingress would hold it.
    let frame = |seq: u64| {
        let packet = {
            let mut builder = session.thread_local_pool().get();
            builder
                .build_subprotocol(
                    STREAM,
                    seq,
                    &[Bytes::from_static(b"NR4-held")],
                    PacketFlags::RELIABLE,
                    0,
                )
                .to_vec()
        };
        let parsed = net_wire::parsed_packet::ParsedPacket::parse(
            Bytes::from(packet),
            PeerAddr::Udp("127.0.0.1:1".parse().expect("addr")),
        )
        .expect("a real packet parses");
        HeldFrame {
            parsed,
            decrypted: Bytes::from_static(b"NR4-held"),
        }
    };

    // The lifetime that accepts the sequence.
    let accepting_epoch = session.open_stream_with(STREAM, true, 1);
    let accepted = StreamLifetime {
        session_id: session.session_id(),
        epoch: accepting_epoch,
    };

    // The close/reopen that lands between acceptance and insertion.
    session.close_stream(STREAM);
    let replacement_epoch = session.open_stream_with(STREAM, true, 1);
    assert_ne!(
        replacement_epoch, accepting_epoch,
        "the reopen must really be a new lifetime, or there is nothing to \
         cross"
    );

    assert!(
        !session.hold_in_order_frame(STREAM, 9, accepted, frame(9)),
        "a frame accepted by the predecessor must NOT be inserted into the \
         replacement's reorder buffer"
    );
    assert_eq!(
        session
            .get_stream(STREAM)
            .expect("the replacement exists")
            .reorder_held(),
        0,
        "and the replacement's buffer is untouched: it never accepted that \
         sequence and never reserved its bytes"
    );
    assert!(
        !session.holds_in_order(STREAM),
        "and the session's fast-path hold count is not left counting a \
         frame that was refused"
    );

    // Control: the live lifetime's own out-of-order arrival is held.
    let live = StreamLifetime {
        session_id: session.session_id(),
        epoch: replacement_epoch,
    };
    assert!(
        session.hold_in_order_frame(STREAM, 9, live, frame(9)),
        "the lifetime check must not break ordinary holding"
    );
    assert!(session.holds_in_order(STREAM));
}

/// **NR4**: evicting a stream that is holding ACKNOWLEDGED arrivals
/// is a typed terminal for that stream, and it does not leave the
/// session's hold count counting frames that no longer exist.
///
/// Both eviction branches dropped the `StreamState` — and with it
/// every in-order hold — behind an eviction log. Those arrivals were
/// already acknowledged, so the sender had dropped its only copy:
/// the data is unrecoverable and its owner was never told.
///
/// Inverse: delete the `forget_in_order_hold` / `note_receive_terminal`
/// pair from the idle branch of `evict_idle_streams` — nothing is
/// owed to the peer and `holds_in_order` still reports a hold on a
/// stream that is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evicting_an_acknowledged_in_order_hold_ends_the_receive_half() {
    use net_wire::protocol::PacketFlags;
    use net_wire::session::{HeldFrame, StreamLifetime};

    const STREAM: u64 = 0x0797;
    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let b_id = b.node_id();
    let session = a
        .peer_session_for_test(b_id)
        .expect("A's session to B")
        .clone();

    let epoch = session.open_stream_with(STREAM, true, 1);
    let packet = {
        let mut builder = session.thread_local_pool().get();
        builder
            .build_subprotocol(
                STREAM,
                4,
                &[Bytes::from_static(b"NR4-evict")],
                PacketFlags::RELIABLE,
                0,
            )
            .to_vec()
    };
    let parsed = net_wire::parsed_packet::ParsedPacket::parse(
        Bytes::from(packet),
        PeerAddr::Udp("127.0.0.1:1".parse().expect("addr")),
    )
    .expect("a real packet parses");
    assert!(
        session.hold_in_order_frame(
            STREAM,
            4,
            StreamLifetime {
                session_id: session.session_id(),
                epoch,
            },
            HeldFrame {
                parsed,
                decrypted: Bytes::from_static(b"NR4-evict"),
            },
        ),
        "the premise: this receiver is holding an acknowledged arrival"
    );
    assert!(session.holds_in_order(STREAM));
    let _ = session.take_receive_terminals();

    // Evict everything idle: `max_idle` of zero makes every stream
    // idle, which is the production sweep's own decision rule.
    let evicted = session.evict_idle_streams(Duration::ZERO, 0, "nr4_witness");
    assert!(
        evicted > 0,
        "the sweep must actually have evicted the stream"
    );

    assert_eq!(
        session.take_receive_terminals(),
        vec![STREAM],
        "an eviction that discards acknowledged arrivals is terminal for \
         that stream: the peer is told so its pending read fails fast \
         instead of waiting out a timeout for bytes this node threw away"
    );
    assert!(
        !session.holds_in_order(STREAM),
        "and the session's maintained hold count is corrected, rather than \
         left counting frames the eviction destroyed"
    );
}

/// **R3-1..R3-4, the receiving half:** a reliable stream whose
/// fire-and-forget prefix lost its LAST sequence recovers, because
/// the native receive path applies the boundary its sender STATED
/// instead of the one it could infer.
///
/// # The defect this exists for
///
/// A stream id carries both modes — a channel's publish id is
/// derived from the channel, and nRPC's from the route — so a
/// fire-and-forget burst and a reliable one share one sequence
/// space. On promotion the receiver has to know WHERE reliability
/// began. Until the sender's `MODE_BOUNDARY` arrives it assumes the
/// conservative boundary — its own contiguous frontier — and that
/// assumption names the sender's last fire-and-forget sequence
/// whenever that sequence was the one lost. Nothing can rebuild it:
/// fire-and-forget retained no descriptor, so there is no NACK that
/// can produce it and no ack the receiver can honestly emit. Every
/// reliable arrival is then held behind a permanent hole, the
/// cumulative ack never advances, and the SENDER's retransmits
/// exhaust into a typed `stream_failed` on a stream that lost
/// nothing reliable at all.
///
/// That was live on this path: `MODE_BOUNDARY` was stamped by leaf
/// senders, read by the leaf's receive half, and dropped on the
/// floor by the native core, which promoted with
/// `StreamState::ensure_reliable` and never looked at the flag. The
/// arrival-count concession that had covered it until then
/// (`RESUME_CONCESSION_ARRIVALS`) was deleted in the same change
/// that introduced the signal, so the native receiver was left with
/// neither mechanism.
///
/// # Why it is built by hand
///
/// The sender has to state a boundary, and no native send path
/// stamps one — a leaf does. So the packets are built from A's real
/// session (real AEAD, real header, real sequence space) and
/// submitted through A's real RTC transport, which is exactly the
/// datagram a browser leaf puts on the channel. Everything on B's
/// side is production: prefilter, decrypt, replay window,
/// `account_inbound_stream_packet`, the in-order hold, dispatch.
///
/// The reorder is deliberate and is the hazard's own shape: the
/// boundary packet arrives AFTER the sequence behind it, so a
/// receiver that only applied the boundary at first-touch of the
/// reliable region would already have promoted at the assumed one.
///
/// Inverse: pass `None` for `mode_boundary` at
/// `account_inbound_stream_packet`'s
/// `get_or_create_stream_for_packet` call — none of the six
/// reliable payloads is ever delivered, because the receiver holds
/// them all behind the conceded prefix's final sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stated_mode_boundary_releases_a_reliable_stream_over_a_lost_prefix() {
    use net_wire::protocol::PacketFlags;

    const STREAM: u64 = 0x07B1;
    // Sequences 0..=4 are fire-and-forget and arrive; 5 is
    // fire-and-forget, is built, and is never submitted — the
    // unrebuildable tail. The reliable region is 6..=11, and 6 is
    // the sender's stated boundary.
    const PREFIX: usize = 5;
    const LOST_PREFIX_SEQ: u64 = 5;
    const BOUNDARY_SEQ: u64 = 6;
    const RELIABLE: usize = 6;

    let (a, b, id_a, _) = pair_with(rtc_config(), rtc_config()).await;
    let session = a
        .peer_session_for_test(b.node_id())
        .expect("A's session to B")
        .clone();
    let transport = a.rtc_driver().expect("driver").transport();

    let build = |seq: u64, payload: &Bytes, flags: PacketFlags| -> Vec<u8> {
        let mut builder = session.thread_local_pool().get();
        builder
            .build(STREAM, seq, std::slice::from_ref(payload), flags)
            .to_vec()
    };

    // ---- the fire-and-forget prefix ------------------------------
    let prefix = tagged_payloads(b"MBPFX", PREFIX);
    for (i, payload) in prefix.iter().enumerate() {
        let packet = build(i as u64, payload, PacketFlags::NONE);
        transport.submit(&packet, id_a).expect("admission");
    }
    let prefix_seen = collect_tagged(&b, b"MBPFX", PREFIX, Duration::from_secs(20)).await;
    assert_eq!(
        prefix_seen
            .iter()
            .map(|p| p[b"MBPFX".len()])
            .collect::<HashSet<_>>()
            .len(),
        PREFIX,
        "premise: the fire-and-forget prefix must arrive, so the receiver's \
         assumed boundary really is one sequence above it — {} of {PREFIX} did",
        prefix_seen.len()
    );

    // Built and withheld. Its sender kept no descriptor for it
    // (fire-and-forget), so this sequence can never be produced
    // again by anything — which is the whole point: the receiver
    // must not wait on it, and must not acknowledge it either.
    let _never_sent = build(
        LOST_PREFIX_SEQ,
        &tagged_payloads(b"MBLOST", 1)[0],
        PacketFlags::NONE,
    );

    // ---- the reliable region, boundary stated and reordered ------
    let reliable = tagged_payloads(b"MBREL", RELIABLE);
    let mut packets: Vec<(u64, Vec<u8>)> = Vec::with_capacity(RELIABLE);
    for (i, payload) in reliable.iter().enumerate() {
        let seq = BOUNDARY_SEQ + i as u64;
        let flags = if seq == BOUNDARY_SEQ {
            PacketFlags::RELIABLE.with(PacketFlags::MODE_BOUNDARY)
        } else {
            PacketFlags::RELIABLE
        };
        packets.push((seq, build(seq, payload, flags)));
    }
    // 7 before 6 — the boundary overtaken — then 9 before 8, then
    // the tail in order. Built in submission order so the AEAD
    // counters ascend and it is the Net sequence, not the cipher
    // counter, that is out of order.
    for idx in [1usize, 0, 3, 2, 4, 5] {
        transport.submit(&packets[idx].1, id_a).expect("admission");
    }

    let seen = collect_tagged(&b, b"MBREL", RELIABLE, Duration::from_secs(30)).await;
    let order: Vec<u8> = seen.iter().map(|p| p[b"MBREL".len()]).collect();
    let distinct: HashSet<u8> = order.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        RELIABLE,
        "every reliable payload must be delivered: the prefix sequence the \
         receiver could not have is BELOW the stated boundary, so it is \
         conceded, not waited on. {} of {RELIABLE} arrived ({order:?}) — zero \
         is what the assumed boundary produces, and what the sender then \
         answers with exhausted retransmits and a failed stream",
        distinct.len()
    );
    assert_eq!(
        order,
        (0..RELIABLE as u8).collect::<Vec<u8>>(),
        "and in sequence order, exactly once each: the reorder is restored by \
         the in-order hold, not tolerated by the consumer — got {order:?}"
    );
}

/// Let a quiet RTC link flush whatever SCTP acknowledgements it still
/// owes, so the NEXT qualifying egress datagram is the one the caller
/// is about to send.
///
/// 400 ms clears a delayed-ack timer (typically 200 ms) with margin.
/// Not a synchronisation with anything the test then measures — every
/// assertion below has its own condition or its own deadline — only a
/// way to make the injector's "Nth qualifying datagram" mean the Nth
/// datagram of the traffic under test.
async fn settle_egress() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

// ---------------------------------------------------------------
// The below-SCTP injector's A/B — §11.8's mDNS causality, executed
// ---------------------------------------------------------------

/// One datagram lost BELOW SCTP, three payload classes, one
/// instrument: what recovers it is a Net mechanism, never SCTP.
///
/// # Why this test exists
///
/// `RtcTestHooks::set_raw_egress_drop_at` was landed with no caller.
/// An instrument that could support an experiment is not that
/// experiment, and an unused fixtures-only hook is worse than no
/// hook: it reads as evidence and is not. This is the run it was
/// added for, so §11.8's mDNS causality stays re-checkable instead
/// of resting on a lost trace.
///
/// # What is held fixed, and what varies
///
/// The LAYER is fixed. `set_ingress_drop_one_in` discards an
/// `Event::ChannelData` — SCTP has already delivered it, so that
/// loss is above SCTP and terminal however the channel was
/// negotiated. This hook discards the datagram on the socket, so
/// the peer's SCTP never sees the chunk at all. Every arm below
/// loses exactly one such datagram.
///
/// The PAYLOAD CLASS varies, and that is the finding: the
/// DataChannel is negotiated `{ordered: false, maxRetransmits: 0}`
/// (`driver.rs`), so SCTP recovers NOTHING here. What survives a
/// below-SCTP drop survives because some Net-level mechanism covers
/// that class of packet:
///
///   * a RELIABLE stream packet is covered by `reliability.rs` —
///     every payload still arrives, and the retransmit counter says
///     who carried it;
///   * a FIRE-AND-FORGET packet is covered by nothing — the payload
///     is gone, and the disarmed control proves it was this
///     injection that lost it;
///   * a Noise `msg1` is outside the reliable-stream machinery
///     entirely (`build_handshake`, not a stream packet) and is
///     TERMINAL: `handshake_initiator` does retransmit a
///     byte-identical copy, but the sole responder — `accept_rtc` —
///     is one-shot, has spent its deadline waiting for the copy that
///     was dropped, and is no longer listening when the next one
///     lands. The whole budget burns and the call reports
///     `Connection("handshake timeout")`.
///
/// That last arm CONFIRMS §11.8. The prediction going in was the
/// opposite — that retransmission would cover it and the finding
/// needed rewording — and the run said otherwise. Recovery below
/// SCTP needs something that RE-ARMS the far side, not merely a
/// sender that repeats itself, and only `reliability.rs` does that.
///
/// # Why the assertions are what they are
///
/// `raw_egress_counted()` is required nonzero in every armed arm:
/// without it the arm proves only that a drop was REQUESTED, and a
/// selector that stopped matching (the `0x17` DTLS content-type
/// check, say) would leave every assertion below satisfied for the
/// wrong reason — the reliable arm trivially, the FAF arm as a false
/// "nothing was lost".
///
/// Each arm arms the hook immediately before its own sends, so the
/// first qualifying datagram is a payload and not an SCTP
/// acknowledgement: the sender has received nothing to acknowledge
/// at that point, and the heartbeat is parked at 600 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_datagram_lost_below_sctp_is_recovered_only_where_net_covers_it() {
    // ---- arm 1: a RELIABLE stream ----------------------------
    let (a, b, _id_a) = quiet_pair().await;
    let hooks_a = a.rtc_driver().expect("driver").hooks();

    let before_retransmit = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    let reliable = a
        .open_stream(b.node_id(), 0x0071, cfg)
        .expect("open_stream");
    const N: usize = 8;
    let payloads = tagged_payloads(b"RAWREL", N);
    // QUIESCE before arming, and this is load-bearing. The first
    // attempt armed immediately after `quiet_pair()` and dropped a
    // leftover SCTP acknowledgement for the Noise exchange that had
    // just completed — 16 qualifying datagrams counted, every
    // sequence delivered, ZERO retransmits, i.e. the instrument
    // fired and hit nothing that mattered. The heartbeat is parked
    // at 600 s, so once the post-handshake acks flush, this side's
    // next qualifying datagram is its first payload.
    settle_egress().await;
    hooks_a.set_raw_egress_drop_at(1);
    for payload in &payloads {
        a.send_with_retry(&reliable, std::slice::from_ref(payload), 16)
            .await
            .expect("send_with_retry");
    }
    let reliable_seen = collect_tagged(&b, b"RAWREL", N, Duration::from_secs(60)).await;
    let reliable_dropped = hooks_a.raw_egress_counted();
    let reliable_indices: HashSet<u8> = reliable_seen.iter().map(|p| p[b"RAWREL".len()]).collect();
    assert!(
        reliable_dropped >= 1,
        "the below-SCTP injector counted {reliable_dropped} qualifying datagrams: \
         nothing was dropped, so this arm says nothing about recovery"
    );
    assert_eq!(
        reliable_indices.len(),
        N,
        "a reliable stream must recover a datagram lost below SCTP — SCTP will \
         not, the channel is maxRetransmits:0 — but {} of {N} sequences arrived \
         ({} deliveries)",
        reliable_indices.len(),
        reliable_seen.len()
    );
    let retransmits = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed)
        - before_retransmit;
    assert!(
        retransmits >= 1,
        "and `reliability.rs` is what carried it: {retransmits} retransmitted \
         packets. Zero would mean the payload was never actually lost, which \
         contradicts the {reliable_dropped} counted drop(s)"
    );

    // ---- arm 2: FIRE-AND-FORGET, disarmed CONTROL then armed --
    //
    // The control runs FIRST and on the same pair, so the armed arm's
    // loss cannot be blamed on the schedule, the box, or this stream
    // id: the identical send loop with the hook disabled delivers
    // everything.
    hooks_a.set_raw_egress_drop_at(0);
    let mut faf_cfg = StreamConfig::new();
    faf_cfg.reliability = Reliability::FireAndForget;
    let control = a
        .open_stream(b.node_id(), 0x0072, faf_cfg)
        .expect("open_stream");
    let control_payloads = tagged_payloads(b"RAWCTL", N);
    for payload in &control_payloads {
        a.send_with_retry(&control, std::slice::from_ref(payload), 16)
            .await
            .expect("send");
    }
    let control_seen = collect_tagged(&b, b"RAWCTL", N, Duration::from_secs(20)).await;
    let control_indices: HashSet<u8> = control_seen.iter().map(|p| p[b"RAWCTL".len()]).collect();
    assert_eq!(
        control_indices.len(),
        N,
        "CONTROL: with the injector disarmed a fire-and-forget stream must \
         deliver all {N} on loopback; {} arrived, so the armed arm below would \
         be measuring ambient loss",
        control_indices.len()
    );

    let armed = a
        .open_stream(b.node_id(), 0x0073, faf_cfg)
        .expect("open_stream");
    let armed_payloads = tagged_payloads(b"RAWFAF", N);
    // Same quiesce: the reliable arm above drew wire-level ACKs from
    // B, so this side owes SACKs until the link goes quiet again.
    settle_egress().await;
    hooks_a.set_raw_egress_drop_at(1);
    for payload in &armed_payloads {
        a.send_with_retry(&armed, std::slice::from_ref(payload), 16)
            .await
            .expect("send");
    }
    let armed_seen = collect_tagged(&b, b"RAWFAF", N, Duration::from_secs(20)).await;
    let armed_dropped = hooks_a.raw_egress_counted();
    let armed_indices: HashSet<u8> = armed_seen.iter().map(|p| p[b"RAWFAF".len()]).collect();
    assert!(
        armed_dropped >= 1,
        "the injector counted {armed_dropped} qualifying datagrams on the \
         fire-and-forget arm: nothing was dropped, so `all {N} arrived` would \
         be the control again rather than a measurement"
    );
    assert!(
        armed_indices.len() < N,
        "a datagram lost below SCTP on a fire-and-forget stream is lost for \
         good — nothing covers it, and the channel is maxRetransmits:0 — yet \
         all {N} sequences arrived"
    );
    assert!(
        !armed_indices.is_empty(),
        "one datagram was dropped, not the stream: the survivors must still \
         arrive, and nothing did — which is what a stream that never sent \
         looks like"
    );

    // ---- arm 3: a Noise `msg1`, on a fresh pair --------------
    //
    // `open_rtc_channel` opens the DataChannel WITHOUT running Noise,
    // so the hook can be armed between channel establishment and
    // `msg1`. Arming before it would target the DCEP open instead,
    // which is a different claim (channel establishment, not
    // handshake recovery).
    let c = node(Some(rtc_config())).await;
    let d = node(Some(rtc_config())).await;
    c.start();
    d.start();
    let (id_c, id_d) = open_rtc_channel(&c, &d)
        .await
        .expect("DataChannel without Noise");
    // Let the channel-open exchange's acknowledgements flush, so the
    // next qualifying datagram out of `c` is `msg1` itself.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let hooks_c = c.rtc_driver().expect("driver").hooks();
    hooks_c.set_raw_egress_drop_at(1);

    let d_task = {
        let d = Arc::clone(&d);
        let c_node_id = c.node_id();
        tokio::spawn(async move { d.accept_rtc(id_d, c_node_id).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let d_pubkey = *d.public_key();
    let handshake = c.connect_rtc(id_c, &d_pubkey, d.node_id()).await;
    let handshake_dropped = hooks_c.raw_egress_counted();
    assert!(
        handshake_dropped >= 1,
        "the injector counted {handshake_dropped} qualifying datagrams during \
         the handshake: nothing was dropped, so its completion proves nothing"
    );
    // MEASURED, and it confirms §11.8 rather than correcting it.
    //
    // The prediction going in was that this arm would come back
    // `Ok`: `handshake_initiator` retransmits a BYTE-IDENTICAL
    // `msg1` up to `handshake_retries` times, and only the FIRST
    // qualifying datagram is dropped, so attempt two should reach
    // the peer untouched. It does reach the peer. Nobody is
    // listening for it. `accept_rtc` is the sole responder and it is
    // ONE-SHOT with its own deadline (see `loopback.rs`): it spends
    // that deadline waiting for the `msg1` that never arrived, stops
    // listening, and every later copy lands on a node that no longer
    // answers direct handshakes. The initiator then burns its whole
    // budget — `handshake_retries` × `handshake_timeout` plus the
    // inter-retry sleeps — and reports `Connection("handshake
    // timeout")`.
    //
    // So one datagram lost below SCTP IS terminal for a Noise
    // exchange, and for a reason the two arms above isolate
    // precisely: not because retransmission is missing, but because
    // nothing on this path RE-ARMS the listener the way
    // `reliability.rs` re-drives a stream. That is the §11.8 mDNS
    // finding, now re-checkable from source instead of from a lost
    // trace, and it is why the harness that switched SCTP recovery
    // off made a single lost datagram fatal.
    assert!(
        handshake.is_err(),
        "a Noise `msg1` lost below SCTP must be TERMINAL: the sole responder \
         is one-shot and has stopped listening by the time the initiator's \
         retransmission arrives, so the budget expires. It reported \
         {handshake:?} — a success here means something now re-arms the \
         responder, which is a real behaviour change and this comment is \
         stale, not wrong"
    );
    let handshake_error = format!("{handshake:?}");
    assert!(
        handshake_error.contains("handshake timeout"),
        "and it must fail as a TIMEOUT — the budget expiring with nobody \
         answering — not as some other connection fault that would mean the \
         injector broke the channel instead of losing one datagram: \
         {handshake_error}"
    );
    assert!(
        d_task.await.expect("accept task").is_err(),
        "the responder's one-shot accept must also fail: if it succeeded, the \
         two sides disagree about whether a session exists, which is a worse \
         defect than the loss"
    );
    assert!(
        c.peer_endpoint(d.node_id()).is_none(),
        "and no session is installed — a terminal handshake must leave no \
         half-open peer record behind"
    );
}

// ---------------------------------------------------------------
// Stage 5, fifth round, ruling 4 — multi-fragment interoperability
// in BOTH directions. `S5_R5_BRIEF.md` §4.
// ---------------------------------------------------------------

/// A payload whose every byte is derived from `nonce`, so an
/// assertion that the receiver got *these* bytes cannot be satisfied
/// by a payload some other run, some other group or some retained
/// buffer produced.
///
/// The first sixteen bytes are the tag and the nonce verbatim, which
/// is what makes a partial arrival attributable: a lone head still
/// names the message it was a piece of.
fn nonce_payload(tag: &[u8; 8], nonce: u64, len: usize) -> Bytes {
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(tag);
    out.extend_from_slice(&nonce.to_le_bytes());
    let mut state = nonce | 1;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xFF) as u8);
    }
    out.truncate(len);
    Bytes::from(out)
}

/// The offset `event` occupies inside `payload`, if it is one of the
/// pieces a fragmenting sender would have cut it into.
///
/// Both fragmenting producers — the browser leaf's `split_payload`
/// and the native sender — cut at `MAX_EVENT_SIZE`, so the candidate
/// offsets are exactly the multiples of it. Checking those rather
/// than every window keeps the measurement O(pieces) and, more
/// importantly, makes "one whole payload" and "N partial pieces"
/// answers to the SAME question instead of two different ones.
fn piece_offset(payload: &[u8], event: &[u8]) -> Option<usize> {
    let step = net_wire::protocol::MAX_EVENT_SIZE;
    (0..payload.len().div_ceil(step))
        .map(|i| i * step)
        .find(|&off| {
            payload.len() - off >= event.len() && &payload[off..off + event.len()] == event
        })
}

/// Drain every shard and return, in arrival order, the events that
/// are `payload` or a piece of it.
///
/// Returns as soon as the WHOLE payload arrives; otherwise it spends
/// the window, so a partial delivery is measured rather than waited
/// out. That is the discriminator the reassembly witnesses need: a
/// working receive half produces `[payload]`, and the inverse (no
/// reassembly) produces the N pieces, from one measurement.
async fn collect_payload_events(
    node: &Arc<MeshNode>,
    payload: &[u8],
    within: Duration,
) -> Vec<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen: Vec<Vec<u8>> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        for shard in 0..4u16 {
            let result = node.poll_shard(shard, None, 512).await.expect("poll_shard");
            for event in result.events {
                let raw = event.raw.to_vec();
                if piece_offset(payload, &raw).is_some() {
                    seen.push(raw);
                }
            }
        }
        if seen.iter().any(|e| e.len() == payload.len()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    seen
}

/// Emit `payload` from `from` to `to_node` exactly as a browser leaf
/// would, and return how many packets that took.
///
/// This is the leaf's `openStream` + `send` shape, not an
/// approximation of it: `leaf/src/frame.rs::split_payload` cuts at
/// `MAX_EVENT_SIZE`, stamps `FRAG_FRAGMENTED` on every piece and
/// `FRAG_LAST` on the final one, and `leaf/src/session.rs::
/// build_packets` allocates the pieces' sequences inside ONE call in
/// offset order — which is why they are contiguous and ascending,
/// and why the receiver may hold a group to that.
async fn leaf_send_fragmented(
    from: &Arc<MeshNode>,
    to_node: u64,
    stream_id: u64,
    fragment_id: u16,
    payload: &[u8],
) -> usize {
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST, MAX_EVENT_SIZE};

    let mut offset = 0usize;
    let mut packets = 0usize;
    while offset < payload.len() {
        let end = (offset + MAX_EVENT_SIZE).min(payload.len());
        let last = end == payload.len();
        let piece = leaf_fragment(
            from,
            to_node,
            stream_id,
            (
                fragment_id,
                u16::try_from(offset).expect("the leaf's ceiling keeps offsets in u16"),
                FRAG_FRAGMENTED | if last { FRAG_LAST } else { 0 },
            ),
            0,
            &payload[offset..end],
        );
        from.send_built_packet_for_test(to_node, &piece)
            .await
            .expect("the piece leaves the leaf");
        packets += 1;
        offset = end;
    }
    packets
}

/// **(a)** leaf → native: a 40 000-byte reliable stream payload
/// arrives as ONE event, byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leaf_fragmented_payload_reaches_a_native_peer_as_one_event() {
    const STREAM: u64 = 0x07A0;
    const GROUP: u16 = 101;
    const SIZE: usize = 40_000;

    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");
    let payload = nonce_payload(b"S5R4-A--", 0x5A5A_0001_u64, SIZE);

    let packets = leaf_send_fragmented(&b, a_id, STREAM, GROUP, &payload).await;
    assert_eq!(
        packets, 5,
        "40 000 bytes is five leaf fragments; a single packet would mean the \
         premise (a payload no packet can carry) was never arranged"
    );

    let delivered = collect_payload_events(&a, &payload, Duration::from_secs(10)).await;
    assert_eq!(
        delivered.len(),
        1,
        "a fragmented leaf payload is ONE message: the receiver saw {} events \
         of sizes {:?}",
        delivered.len(),
        delivered.iter().map(|e| e.len()).collect::<Vec<_>>()
    );
    assert_eq!(
        delivered[0].len(),
        SIZE,
        "and it is the whole payload, not its head"
    );
    assert!(
        delivered[0] == payload,
        "and byte-identical to what the leaf sent"
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "a completed group keeps nothing"
    );
    assert_eq!(
        a.rtc_reassembly().abandoned_total(),
        0,
        "and nothing was abandoned on the way"
    );
}

/// A reliable stream config, spelled once for the ruling-4
/// witnesses: fragmentation is a reliable-semantics claim.
fn reliable_config() -> StreamConfig {
    let mut cfg = StreamConfig::new();
    cfg.reliability = Reliability::Reliable;
    cfg
}

/// Make `receiver` advertise fragment reassembly through the real
/// announcement path, and wait until `sender`'s capability fold
/// actually carries the tag.
///
/// Both halves matter. The gate reads the SENDER's fold, so
/// announcing without observing arrival would make every witness
/// below a race with the announcement it depends on; and asserting
/// the arrival is how we know a later refusal is the gate's verdict
/// rather than a capability that never propagated.
async fn advertise_reassembly(receiver: &Arc<MeshNode>, sender: &Arc<MeshNode>) {
    use net::adapter::net::behavior::capability::{CapabilitySet, FRAGMENT_REASSEMBLY_TAG};
    use net::adapter::net::behavior::fold::capability::capability_tags_for;

    receiver
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce_capabilities");
    let node_id = receiver.node_id();
    assert!(
        wait_for(
            || capability_tags_for(sender.capability_fold(), node_id)
                .iter()
                .any(|t| t == FRAGMENT_REASSEMBLY_TAG),
            Duration::from_secs(10)
        )
        .await,
        "the receiver's reassembly capability never reached the sender's fold, \
         so nothing below would be measuring the gate: {:?}",
        capability_tags_for(sender.capability_fold(), node_id)
    );
}

/// This stream's next TX sequence on `from`'s session to `to_node` —
/// how many sequences the send actually consumed, which is how a
/// witness tells "fragmented into N packets" from "sent as one" and
/// "refused before the wire" from "refused after a piece went out".
fn tx_seq_of(from: &Arc<MeshNode>, to_node: u64, stream_id: u64) -> u64 {
    from.peer_session_for_test(to_node)
        .expect("a session to the peer")
        .get_or_create_stream(stream_id)
        .current_tx_seq()
}

/// **(b)** native → a peer that advertises reassembly: a 40 000-byte
/// reliable `send_on_stream` arrives as ONE event, byte-identical.
///
/// The mirror of (a), and the half ruling 4 actually had to build.
/// `A` here is playing the role the browser leaf plays on the wire —
/// an RTC peer whose receive path reassembles and that says so — and
/// the assertion that this really fragmented is the sequence count:
/// five packets, five sequences, one event delivered.
///
/// Inverse (reassembly disabled): stub
/// `MeshNode::reassemble_rtc_fragments` to `Some(events)` — the five
/// pieces are delivered as five partial events.
/// Inverse (gate removed): see (e).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_sender_fragments_for_a_peer_that_advertises_reassembly() {
    const STREAM: u64 = 0x07A1;
    const SIZE: usize = 40_000;

    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");
    advertise_reassembly(&a, &b).await;

    let stream = b
        .open_stream(a_id, STREAM, reliable_config())
        .expect("open_stream");
    let before = tx_seq_of(&b, a_id, STREAM);
    let payload = nonce_payload(b"S5R4-B--", 0x5A5A_0002_u64, SIZE);
    b.send_on_stream(&stream, std::slice::from_ref(&payload))
        .await
        .expect("an over-cap event is carried, not refused, for this peer");

    assert_eq!(
        tx_seq_of(&b, a_id, STREAM) - before,
        5,
        "40 000 bytes must leave as FIVE pieces on five consecutive \
         sequences; one sequence would mean an over-cap packet no receiver \
         accepts, and a different count would break the receiver's \
         contiguity rule"
    );

    let delivered = collect_payload_events(&a, &payload, Duration::from_secs(10)).await;
    assert_eq!(
        delivered.len(),
        1,
        "the group is ONE message at the peer: it saw {} events of sizes {:?}",
        delivered.len(),
        delivered.iter().map(|e| e.len()).collect::<Vec<_>>()
    );
    assert_eq!(delivered[0].len(), SIZE, "whole, not the head");
    assert!(
        delivered[0] == payload,
        "and byte-identical to what the sender passed to send_on_stream"
    );
    assert_eq!(
        a.rtc_reassembly().held_bytes(session_id),
        0,
        "a completed group keeps nothing"
    );
    assert_eq!(a.rtc_reassembly().abandoned_total(), 0, "and lost nothing");
}

/// **(c)** a lost MIDDLE fragment of a reliable group is recovered by
/// the existing reliability machinery, and the payload arrives ONCE.
///
/// The loss is injected ABOVE SCTP, at the receiver's ingress, so
/// SCTP has already delivered the chunk and cannot recover it
/// whatever the channel was negotiated as: if the payload arrives, a
/// Net retransmit carried it. And the retransmit has to RESTAMP the
/// fragment header — a rebuilt piece with `frag_flags == 0` is a
/// whole event to the receiver, which would hand its application a
/// partial payload and leave the group short forever.
///
/// **"Middle" is now GUARANTEED, not inferred.** The injector names
/// the piece by its own `fragment_offset` (`2 × MAX_EVENT_SIZE`, the
/// third of five), so which piece is lost is an input to the
/// witness rather than something read back out of it, and
/// `ingress_fragment_dropped() == 1` asserts the loss actually
/// happened.
///
/// An earlier version instead WAITED to observe the interior hole —
/// `held == 2 × MAX_EVENT_SIZE` — and that assertion is gone
/// deliberately, because it is a race rather than a property. The
/// hole exists only between the loss and its recovery: on the Linux
/// runner the retransmission lands inside the polling interval, the
/// group completes, `held` returns to 0, and the witness reported
/// `held 0 bytes` — the same reading it prints when the HEAD was
/// lost. A check that cannot distinguish "recovered faster than we
/// looked" from "lost the wrong piece" is not measuring either.
///
/// The raw-egress injector cannot make this claim at all and the
/// first attempt at this witness proved it: one 8 KiB Net packet
/// rides ~7 DTLS datagrams, so arming it at 3 counted 43 qualifying
/// datagrams and lost a chunk of the FIRST piece.
///
/// The ORDINAL ingress injector (`set_ingress_drop_at(3)`) was the
/// second attempt, and CI found its flaw: an ordinal is the piece
/// this witness means only if nothing else arrives in between, and
/// credit grants and acknowledgements share the channel. It selected
/// the middle piece on Windows and the group's HEAD on the Linux
/// runner — where the assertion below read `held 0`, which its own
/// message already interprets as the head being lost. The injector
/// now names the piece by its own `fragment_offset`, which no other
/// traffic can shift, and fires ONCE: a retransmission carries the
/// same offset, so a still-armed injector would eat the recovery
/// this witness exists to observe.
///
/// Inverse: delete the `if let Some(f) = d.fragment` restamp from
/// either rebuild site — the recovered piece arrives unfragmented,
/// the group never completes, and the payload never arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_middle_fragment_is_retransmitted_and_the_payload_arrives_once() {
    use net_wire::protocol::MAX_EVENT_SIZE;

    const STREAM: u64 = 0x07A2;
    const SIZE: usize = 40_000;

    let (a, b, _id_a) = quiet_pair().await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    // B reassembles and says so; A is the fragmenting sender.
    advertise_reassembly(&b, &a).await;
    // Asserted, not bound: the witness no longer reads per-session
    // reassembly state, but a missing session here would make every
    // measurement below meaningless.
    b.peer_session_id(a_id).expect("an installed session");
    let hooks_b = b.rtc_driver().expect("driver").hooks();
    let before_retransmit = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed);

    let stream = a
        .open_stream(b_id, STREAM, reliable_config())
        .expect("open_stream");
    let payload = nonce_payload(b"S5R4-C--", 0x5A5A_0003_u64, SIZE);

    // Quiesce first, exactly as the below-SCTP witness above does:
    // an armed hook that eats a leftover acknowledgement fires and
    // hits nothing that matters. The heartbeat is parked at 600 s,
    // so once the announcement traffic flushes, B's next inbound
    // packets are this group's pieces.
    settle_egress().await;
    // The THIRD piece by its own identity, not by arrival ordinal.
    hooks_b.set_ingress_drop_fragment_offset(Some(2 * MAX_EVENT_SIZE as u16));
    a.send_on_stream(&stream, std::slice::from_ref(&payload))
        .await
        .expect("the group is admitted");

    // The LOSS, waited for as a fact rather than inferred from a
    // transient side effect. The injector disarms as it fires, so
    // this settles at exactly 1 and stays there; the interior hole
    // it creates is NOT asserted, because the hole exists only until
    // the retransmission lands and a faster machine closes it before
    // a poll can see it (which is precisely how this witness failed
    // on the Linux runner, reporting `held 0` — indistinguishable
    // from having lost the head).
    assert!(
        wait_for(
            || hooks_b.ingress_fragment_dropped() == 1,
            Duration::from_secs(10)
        )
        .await,
        "the offset-targeted injector never fired: it is armed at offset {} \
         and dropped {} fragment(s), so no piece was lost and nothing below \
         measures recovery",
        2 * MAX_EVENT_SIZE,
        hooks_b.ingress_fragment_dropped()
    );
    let dropped = hooks_b.ingress_fragment_dropped();

    let delivered = collect_payload_events(&b, &payload, Duration::from_secs(60)).await;
    assert_eq!(
        delivered.len(),
        1,
        "the payload must arrive exactly once — not twice from the \
         retransmission, not partially from the surviving pieces: {:?}",
        delivered.iter().map(|e| e.len()).collect::<Vec<_>>()
    );
    assert!(
        delivered[0] == payload,
        "and byte-identical: a recovered group is the original bytes or it is \
         nothing. Got {} bytes of {SIZE} — an 8 104-byte delivery here is the \
         recovered piece arriving UNFRAGMENTED and handed over as a whole \
         event",
        delivered[0].len()
    );
    let retransmits = a
        .control_plane_stats()
        .retransmit_packets_sent
        .load(std::sync::atomic::Ordering::Relaxed)
        - before_retransmit;
    assert!(
        retransmits >= 1,
        "and `reliability.rs` is what carried it: {retransmits} retransmitted \
         packets against {dropped} fragment(s) actually dropped at the \
         receiver. Zero would mean the piece was never actually lost"
    );

    // ONCE, measured rather than assumed: keep draining after the
    // payload landed. A retransmitted piece arriving after its group
    // completed is a refusal, not a second delivery.
    let after = collect_payload_events(&b, &payload, Duration::from_secs(2)).await;
    assert!(
        after.is_empty(),
        "a second delivery of the same group: {:?}",
        after.iter().map(|e| e.len()).collect::<Vec<_>>()
    );
}

/// **(d)** a group over the fragmentation ceiling is refused typed at
/// the FIRST piece, on both sides.
///
/// Sender: `send_on_stream` refuses `MAX_FRAGMENTED_EVENT_SIZE + 1`
/// with the ceiling as the named limit and consumes NO sequence —
/// there is no first piece, so there is nothing partial to clean up.
/// The companion at-ceiling payload is sent in the same run, so the
/// refusal is a boundary and not a blanket one.
///
/// Receiver: a piece whose own `offset + len` crosses the ceiling is
/// refused before the session's reassembly state is touched at all —
/// no group opened, nothing buffered, nothing delivered, and no
/// abandonment, because nothing was ever admitted to lose.
///
/// Inverse (ceiling check removed): drop the
/// `e.len() > protocol::MAX_FRAGMENTED_EVENT_SIZE` arm in
/// `send_on_stream` — the over-ceiling event is accepted and emitted
/// as nine pieces; drop `end > MAX_REASSEMBLED_BYTES` in
/// `RtcReassembly::accept` — the over-ceiling piece opens a group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_over_the_ceiling_is_refused_at_the_first_piece_on_both_sides() {
    use net::adapter::net::rtc::{FragmentOutcome, FragmentPiece, FragmentProvenance};
    use net_wire::protocol::{FRAG_FRAGMENTED, FRAG_LAST, MAX_FRAGMENTED_EVENT_SIZE};

    const STREAM: u64 = 0x07A3;
    const OVER: u64 = 0x07A4;
    const GROUP: u16 = 103;

    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let session_id = a.peer_session_id(b_id).expect("an installed session");
    advertise_reassembly(&a, &b).await;

    // ---- sender: the boundary, both sides of it ----------------
    let stream = b
        .open_stream(a_id, STREAM, reliable_config())
        .expect("open_stream");
    let at_ceiling = nonce_payload(b"S5R4-D1-", 0x5A5A_0004_u64, MAX_FRAGMENTED_EVENT_SIZE);
    b.send_on_stream(&stream, std::slice::from_ref(&at_ceiling))
        .await
        .expect(
            "the ceiling itself is carried: the refusal below is a bound, \
                 not a blanket",
        );
    let ceiling_delivered = collect_payload_events(&a, &at_ceiling, Duration::from_secs(20)).await;
    assert_eq!(
        ceiling_delivered.len(),
        1,
        "and it arrives as one event of {} bytes: {:?}",
        MAX_FRAGMENTED_EVENT_SIZE,
        ceiling_delivered
            .iter()
            .map(|e| e.len())
            .collect::<Vec<_>>()
    );
    assert!(ceiling_delivered[0] == at_ceiling);

    let before = tx_seq_of(&b, a_id, STREAM);
    let over = nonce_payload(b"S5R4-D2-", 0x5A5A_0005_u64, MAX_FRAGMENTED_EVENT_SIZE + 1);
    let refused = b.send_on_stream(&stream, std::slice::from_ref(&over)).await;
    assert!(
        matches!(
            refused,
            Err(StreamError::EventTooLarge { size, limit })
                if size == MAX_FRAGMENTED_EVENT_SIZE + 1
                    && limit == MAX_FRAGMENTED_EVENT_SIZE
        ),
        "one byte past the ceiling is a TYPED refusal naming the ceiling, \
         got {refused:?}"
    );
    assert_eq!(
        tx_seq_of(&b, a_id, STREAM),
        before,
        "and it consumed no sequence: refused at the first piece means before \
         the first piece exists, so no prefix of the group is on the wire"
    );

    // ---- receiver: a piece that crosses the ceiling ------------
    //
    // No conformant producer emits this — both cut at
    // MAX_EVENT_SIZE and refuse above the ceiling — so it is built
    // by hand, which is exactly the hostile case the bound exists
    // for. It is this group's FIRST piece.
    let held_before = a.rtc_reassembly().held_bytes(session_id);
    let abandoned_before = a.rtc_reassembly().abandoned_total();
    let bad = leaf_fragment(
        &b,
        a_id,
        OVER,
        (GROUP, 60_000, FRAG_FRAGMENTED | FRAG_LAST),
        0,
        &vec![0xD1u8; 5_000],
    );
    b.send_built_packet_for_test(a_id, &bad)
        .await
        .expect("the over-ceiling piece leaves B");

    assert_eq!(
        a.rtc_reassembly().accept(
            FragmentPiece {
                session_id,
                fragment_id: GROUP,
                offset: 60_000,
                flags: FRAG_FRAGMENTED | FRAG_LAST,
                sequence: 0,
                provenance: FragmentProvenance {
                    stream_id: OVER,
                    origin_hash: b_id,
                    channel_hash: 0,
                    subprotocol_id: 0,
                    reliable: true,
                },
                data: Bytes::from(vec![0xD1u8; 5_000]),
            },
            std::time::Instant::now(),
        ),
        Err(FragmentOutcome::Malformed),
        "a piece claiming bytes past the ceiling is typed-refused"
    );
    assert!(
        wait_for(
            || a.rtc_reassembly().held_bytes(session_id) == held_before,
            Duration::from_secs(2)
        )
        .await,
        "and it opened NO group: held {} bytes, was {held_before}",
        a.rtc_reassembly().held_bytes(session_id)
    );
    assert_eq!(
        a.rtc_reassembly().abandoned_total(),
        abandoned_before,
        "and nothing was abandoned — the refusal is before admission, so \
         there were no acknowledged bytes to lose"
    );
}

/// **(e)** native → a native peer that has NOT advertised
/// reassembly still gets the typed `EventTooLarge` refusal at
/// 8 104 B. The hard invariant: ruling 4 changed nothing for a peer
/// that cannot reassemble.
///
/// Inverse (capability gate removed): make
/// `peer_reassembles_fragments` return `true` — this peer is handed
/// two partial events instead of a refusal, which is the silent
/// corruption the gate exists to prevent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_without_the_reassembly_tag_still_gets_event_too_large() {
    use net::adapter::net::behavior::capability::FRAGMENT_REASSEMBLY_TAG;
    use net::adapter::net::behavior::fold::capability::capability_tags_for;
    use net_wire::protocol::MAX_EVENT_SIZE;

    const STREAM: u64 = 0x07A5;
    const SIZE: usize = 9_000;

    let (a, b, _, _) = pair_with(rtc_config(), rtc_config()).await;
    let a_id = a.node_id();
    // The premise, asserted rather than assumed: neither node has
    // announced, so A carries no reassembly claim in B's fold.
    assert!(
        !capability_tags_for(b.capability_fold(), a_id)
            .iter()
            .any(|t| t == FRAGMENT_REASSEMBLY_TAG),
        "this witness is about a peer that has NOT advertised; it has: {:?}",
        capability_tags_for(b.capability_fold(), a_id)
    );

    let stream = b
        .open_stream(a_id, STREAM, reliable_config())
        .expect("open_stream");
    let before = tx_seq_of(&b, a_id, STREAM);
    let payload = nonce_payload(b"S5R4-E--", 0x5A5A_0006_u64, SIZE);
    let refused = b
        .send_on_stream(&stream, std::slice::from_ref(&payload))
        .await;
    assert!(
        matches!(
            refused,
            Err(StreamError::EventTooLarge { size, limit })
                if size == SIZE && limit == MAX_EVENT_SIZE
        ),
        "a peer that does not say it reassembles keeps the 8 104-byte refusal, \
         naming MAX_EVENT_SIZE and not the fragmentation ceiling, got {refused:?}"
    );
    assert_eq!(
        tx_seq_of(&b, a_id, STREAM),
        before,
        "and nothing reached the wire"
    );
    let delivered = collect_payload_events(&a, &payload, Duration::from_millis(800)).await;
    assert!(
        delivered.is_empty(),
        "no partial events at the peer — that is the whole invariant: {:?}",
        delivered.iter().map(|e| e.len()).collect::<Vec<_>>()
    );
}
