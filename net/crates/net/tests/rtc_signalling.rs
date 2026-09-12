//! Stage 4a signalling witnesses: `0x0D02` over a routed session,
//! the §9 sequence end to end, and the §10 three-part direct-path
//! witness.
//!
//! Run: `cargo test --features "webrtc fixtures cortex" --test rtc_signalling`
#![cfg(all(feature = "webrtc", feature = "fixtures"))]

use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::rtc::{
    connect_rtc_loopback, RtcConfig, RtcRejectReason, RtcSignalMsg, MAX_DIALOGS_PER_PEER,
    MAX_FRAMES_PER_WINDOW,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, PeerAddr, SocketBufferConfig};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};

const PSK: [u8; 32] = [0x4Au8; 32];

fn config(rtc: Option<RtcConfig>) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new("127.0.0.1:0".parse().expect("addr"), PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5));
    cfg.socket_buffers = SocketBufferConfig::for_testing();
    cfg.stream_idle_timeout = Duration::from_secs(1);
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
    RtcConfig {
        // Production's 10 s is not a test's patience, and the §9
        // witness needs the *expiry* path (step 6) to be reached
        // deterministically rather than raced.
        ice_deadline: Duration::from_secs(2),
        ..RtcConfig::new().with_bind_addr("127.0.0.1:0".parse().expect("addr"))
    }
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

async fn delivered_with_tag(node: &Arc<MeshNode>, tag: &str, within: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + within;
    let needle = format!("\"tag\":\"{tag}\"");
    let mut count = 0usize;
    while tokio::time::Instant::now() < deadline && count == 0 {
        for shard in 0..4u16 {
            for event in node
                .poll_shard(shard, None, 512)
                .await
                .expect("poll_shard")
                .events
            {
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

/// Two started RTC nodes with a direct UDP session, for the
/// signalling-lifecycle witnesses.
async fn signalling_pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    let a_id = a.node_id();
    let b_clone = Arc::clone(&b);
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b.local_addr(), b.public_key(), b.node_id())
        .await
        .expect("udp handshake");
    accept.await.expect("accept task").expect("accept");
    a.start_arc();
    b.start_arc();
    (a, b)
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

/// Three native nodes, A ↔ R ↔ B, with A and B holding a routed
/// session through R. Build it and return `(a, r, b)`.
async fn routed_trio() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = node(Some(rtc_config())).await;
    let r = node(Some(rtc_config())).await;
    let b = node(Some(rtc_config())).await;
    connect_udp(&a, &r).await;
    connect_udp(&r, &b).await;
    a.start_arc();
    r.start_arc();
    b.start_arc();
    let b_pub = *b.public_key();
    a.connect_via(r.local_addr(), &b_pub, b.node_id())
        .await
        .expect("routed handshake through the anchor");
    (a, r, b)
}

/// EXIT 3: signalling between two nodes that share no direct
/// session is delivered **through** the anchor, and the anchor
/// never sees the frame — it classifies by `subprotocol_id`, which
/// is cleartext header, and holds no key for the payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn signalling_crosses_an_anchor_that_cannot_read_it() {
    let (a, r, b) = routed_trio().await;
    let b_id = b.node_id();

    let frame = RtcSignalMsg::Offer {
        dialog: 0xD1A1,
        sdp: "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n".to_string(),
    };
    a.send_rtc_signal(b_id, &frame).await.expect("send 0x0D02");

    let arrived = wait_for(
        || {
            b.drain_rtc_signals_for_test()
                .iter()
                .any(|(from, msg)| *from == a.node_id() && msg.dialog() == 0xD1A1)
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(
        arrived,
        "the offer must reach B over the routed session the anchor forwards"
    );

    // The anchor forwarded it and read nothing: it admitted no
    // signalling frame of its own, and it counted the forward on
    // the signalling counter rather than the application one.
    assert!(
        r.drain_rtc_signals_for_test().is_empty(),
        "the anchor must never decode a signalling frame it forwards — it has \
         no key for the session that carries it"
    );
    assert_eq!(
        r.rtc_stats().signal_delivered(),
        0,
        "…and it must not count one as delivered to itself"
    );
    assert!(
        r.rtc_stats().signal_forwarded() >= 1,
        "it may classify by subprotocol_id, which is cleartext AAD-authenticated \
         header — that is the one thing an anchor is allowed to know"
    );
}

/// EXIT 5a: a sender past its frame budget is dropped **with the
/// counter**.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn over_budget_signalling_is_dropped_and_counted() {
    let (a, _r, b) = routed_trio().await;
    let b_id = b.node_id();
    let before = b.rtc_stats().signal_over_budget();

    // One dialog, many frames: the frame window is what refuses,
    // not the dialog bound.
    for i in 0..(MAX_FRAMES_PER_WINDOW as usize + 16) {
        let frame = RtcSignalMsg::Candidate {
            dialog: 7,
            candidate: format!("candidate:{i} 1 udp 1 127.0.0.1 4444 typ host"),
            mid: "0".to_string(),
        };
        let _ = a.send_rtc_signal(b_id, &frame).await;
    }

    assert!(
        wait_for(
            || b.rtc_stats().signal_over_budget() > before,
            Duration::from_secs(10)
        )
        .await,
        "frames past the window must be refused and COUNTED — a silent drop is \
         indistinguishable from a peer that never signalled"
    );
}

/// EXIT 5b: the dialog bound refuses the fifth concurrent dialog
/// from one sender, and a `Reject` frees a slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_dialog_bound_holds_and_a_reject_ends_the_dialog() {
    let (a, _r, b) = routed_trio().await;
    let b_id = b.node_id();

    // R5: the offers must be REAL. A malformed SDP is a failed
    // allocation, and a failed allocation now holds no reservation
    // at all — so filling the bound with malformed offers would
    // measure nothing. Each of these allocates an actual agent on
    // the receiver, which is what the bound protects.
    async fn real_offer(node: &Arc<MeshNode>) -> String {
        node.rtc_driver()
            .expect("driver")
            .create_offer()
            .await
            .expect("offer")
            .1
    }

    for dialog in 0..MAX_DIALOGS_PER_PEER as u64 {
        let sdp = real_offer(&a).await;
        a.send_rtc_signal(b_id, &RtcSignalMsg::Offer { dialog, sdp })
            .await
            .expect("send offer");
    }
    assert!(
        wait_for(
            || b.open_signal_dialogs(a.node_id()) == MAX_DIALOGS_PER_PEER,
            Duration::from_secs(10)
        )
        .await,
        "precondition: the bound is full (saw {})",
        b.open_signal_dialogs(a.node_id())
    );

    let before = b.rtc_stats().signal_over_budget();
    let sdp = real_offer(&a).await;
    a.send_rtc_signal(b_id, &RtcSignalMsg::Offer { dialog: 999, sdp })
        .await
        .expect("send offer");
    assert!(
        wait_for(
            || b.rtc_stats().signal_over_budget() > before,
            Duration::from_secs(10)
        )
        .await,
        "concurrency is what costs the receiver an ICE agent, so that is what \
         the dialog bound protects"
    );

    // A Reject for an open dialog frees its slot: the next offer is
    // admitted rather than refused.
    let refused_before = b.rtc_stats().signal_over_budget();
    a.send_rtc_signal(
        b_id,
        &RtcSignalMsg::Reject {
            dialog: 0,
            reason: RtcRejectReason::Declined,
        },
    )
    .await
    .expect("send reject");
    assert!(
        wait_for(
            || b.open_signal_dialogs(a.node_id()) < MAX_DIALOGS_PER_PEER,
            Duration::from_secs(10)
        )
        .await,
        "the Reject must free its slot"
    );
    let sdp = real_offer(&a).await;
    a.send_rtc_signal(b_id, &RtcSignalMsg::Offer { dialog: 1000, sdp })
        .await
        .expect("send offer");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        b.rtc_stats().signal_over_budget(),
        refused_before,
        "after a Reject frees a slot the next offer must be admitted, not refused"
    );
}

/// EXIT 2: a node **without** `rtc` emits an announcement with none
/// of the Stage 4 fields — the wire-compat claim, checked on a real
/// node rather than on a constructed struct.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_without_rtc_emits_no_stage4_fields() {
    let plain = node(None).await;
    plain.start_arc();
    plain
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("announce");

    let ann = wait_for(
        || plain.local_announcement_for_test().is_some(),
        Duration::from_secs(5),
    )
    .await;
    assert!(ann, "the node must have published an announcement");
    let ann = plain
        .local_announcement_for_test()
        .expect("local announcement");
    assert_eq!(ann.noise_pubkey, None);
    assert_eq!(ann.rtc_bootstrap, None);
    assert_eq!(ann.rtc_addr, None);
    let json = String::from_utf8(ann.to_bytes()).expect("UTF-8");
    for absent in ["noise_pubkey", "rtc_bootstrap", "rtc_addr", "transport:rtc"] {
        assert!(
            !json.contains(absent),
            "a node without rtc must be wire-invisible to Stage 4a; found {absent}"
        );
    }
}

/// …and a node **with** `rtc` announces the key a peer needs, plus
/// the tag the classifier reads (EXIT 1's emission half).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_rtc_node_announces_its_noise_key_and_transport_tag() {
    let rtc_node = node(Some(RtcConfig {
        serve_bootstrap: true,
        public_addr: Some("198.51.100.4:4433".parse().expect("addr")),
        ..rtc_config()
    }))
    .await;
    rtc_node.start_arc();
    rtc_node
        .announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("announce");

    let ann = rtc_node
        .local_announcement_for_test()
        .expect("local announcement");
    assert_eq!(
        ann.noise_pubkey,
        Some(*rtc_node.public_key()),
        "the announced key must be THIS node's Noise static — it is what a peer \
         will handshake against"
    );
    assert_eq!(
        ann.rtc_addr,
        Some("198.51.100.4:4433".parse().expect("addr"))
    );
    assert!(ann.rtc_bootstrap.is_some(), "an anchor advertises its URL");
    let tags: Vec<String> = ann
        .capabilities
        .tags
        .iter()
        .map(|t| t.to_string())
        .collect();
    assert!(tags.iter().any(|t| t == "transport:rtc"));
    assert!(tags.iter().any(|t| t == "rtc-anchor"));
}

/// EXIT 4: §9 end to end, natively — announcement → `connect_via` →
/// `0x0D02` → ICE → direct install replacing routed → forced direct
/// loss → explicit interruption → routed reconnection, with the
/// §10 three-part direct-path witness around the direct phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn the_full_section_9_sequence_with_the_three_part_witness() {
    let (a, r, b) = routed_trio().await;
    let b_id = b.node_id();
    let a_routing_id = (a.node_id() & 0xFFFF_FFFF) as u32;

    // Step 1: A holds B's Noise key from B's signed announcement.
    b.announce_capabilities(net::adapter::net::behavior::capability::CapabilitySet::new())
        .await
        .expect("B announces");
    let learned = wait_for(
        || a.peer_announced_noise_pubkey(b_id) == Some(*b.public_key()),
        Duration::from_secs(15),
    )
    .await;
    assert!(
        learned,
        "§5 Layer 1: A must learn B's Noise key from the signed announcement, \
         not out of band"
    );

    // Phase 1 — routed delivery, and the anchor's per-pair
    // application-data counter moving while it carries the pair.
    a.send_routed(b_id, &batch(0, 2, "s9-routed"))
        .await
        .expect("routed send");
    assert!(
        delivered_with_tag(&b, "s9-routed", Duration::from_secs(15)).await >= 1,
        "phase 1 must actually deliver A → R → B"
    );
    let forwarded_after_routed = r.forwarded_app_packets(a_routing_id, b_id);
    assert!(
        forwarded_after_routed >= 1,
        "§10 part 3 (the inverse leg, taken first here): while the pair is on \
         the anchor, the anchor's per-pair counter MUST move — otherwise part 2 \
         cannot tell 'direct' from 'broken'"
    );

    // Phase 2 — the direct path. Stage 4a drives it with real
    // signalling: `offer_direct_path` sends the `Offer` over the
    // routed session and the engine does the rest. The in-process
    // ICE fixture stands in for a network that would let two
    // loopback nodes actually connect.
    // The §12/C3 quiescence gate is real: the routed session still
    // has phase 1's traffic unacked, and the installer refuses to
    // replace a busy incumbent. Wait for quiescence — that IS the
    // contract, not an inconvenience around it.
    let quiescent = wait_for(
        || {
            a.peer_session_for_test(b_id)
                .is_some_and(|s| !s.has_open_streams() && !s.has_unacked())
        },
        Duration::from_secs(20),
    )
    .await;
    assert!(
        quiescent,
        "the routed session must quiesce before replacement"
    );

    let attempts_before = a.rtc_stats().ice_attempted();
    let _dialog = a.offer_direct_path(b_id).await.expect("offer sent");
    assert_eq!(
        a.rtc_stats().ice_attempted(),
        attempts_before + 1,
        "the offer opens a real local endpoint and counts the ICE attempt"
    );

    // **R4: the SAME attempt completes.** No fixture substitution:
    // the production owner carries this dialog's DataChannel-open
    // event through Noise in the offerer's role and into the Stage 3
    // fenced install. Before R4 nothing did, and this witness waited
    // for the attempt to EXPIRE and then built a different
    // connection with `connect_rtc_loopback` — which is not a
    // continuation of the attempt under test.
    let a_old_session = a.peer_session_id(b_id).expect("routed session");
    let b_old_session = b.peer_session_id(a.node_id()).expect("routed session");
    assert!(
        wait_for(
            || matches!(a.peer_endpoint(b_id), Some(PeerAddr::Rtc(_))),
            Duration::from_secs(20)
        )
        .await,
        "§9 steps 3-6: the offered dialog must install the direct session itself"
    );
    let id_a = match a.peer_endpoint(b_id) {
        Some(PeerAddr::Rtc(id)) => id,
        other => panic!("expected a direct RTC endpoint, got {other:?}"),
    };
    assert_eq!(
        a.rtc_stats().ice_relayed(),
        0,
        "the attempt connected, so nothing may be counted as relayed"
    );
    // Both endpoints' new session identities: a replacement is a new
    // incarnation on both sides, not the old session at a new
    // address.
    assert_ne!(
        a.peer_session_id(b_id),
        Some(a_old_session),
        "A's session must be a new incarnation after the upgrade"
    );
    assert!(
        wait_for(
            || {
                matches!(b.peer_endpoint(a.node_id()), Some(PeerAddr::Rtc(_)))
                    && b.peer_session_id(a.node_id()) != Some(b_old_session)
            },
            Duration::from_secs(20)
        )
        .await,
        "B must install its own side of the SAME exchange, with its own new \
         session id (endpoint {:?}, session {:?})",
        b.peer_endpoint(a.node_id()),
        b.peer_session_id(a.node_id())
    );
    assert_eq!(
        a.peer_endpoint(b_id),
        Some(PeerAddr::Rtc(id_a)),
        "the direct endpoint replaces the relayed one — replacement, not a dual \
         session"
    );
    assert!(a.peer_is_direct(b_id));

    // §10 part 1 + part 2: the payload arrives over the direct
    // endpoint, and the anchor's per-pair counter stays flat while
    // it does.
    let flat_before = r.forwarded_app_packets(a_routing_id, b_id);
    a.send_to_peer_node(b_id, &batch(0, 4, "s9-direct"))
        .await
        .expect("direct send");
    assert!(
        delivered_with_tag(&b, "s9-direct", Duration::from_secs(15)).await >= 1,
        "§10 part 1: positive receipt over the selected direct connection"
    );
    assert_eq!(
        r.forwarded_app_packets(a_routing_id, b_id),
        flat_before,
        "§10 part 2: the anchor's per-pair application-data counter must stay \
         flat while the pair is direct"
    );

    // Phase 3 — forced direct loss and explicit interruption.
    a.rtc_driver()
        .expect("driver")
        .close(id_a)
        .await
        .expect("close");
    assert!(
        wait_for(|| a.peer_endpoint(b_id).is_none(), Duration::from_secs(10)).await,
        "direct loss must be an explicit interruption: the peer is removed, not \
         left half-working"
    );
    let b_a = a.node_id();
    assert!(
        wait_for(|| b.peer_endpoint(b_a).is_none(), Duration::from_secs(10)).await
            || b.peer_endpoint(b_a).is_some(),
        "B's own cleanup is timeout-driven; either state is acceptable here"
    );

    // Phase 4 — routed reconnection, and the counter moves again.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut restored = false;
    for _ in 0..3 {
        if a.connect_via(r.local_addr(), &b_pub_of(&b), b_id)
            .await
            .is_ok()
        {
            restored = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        restored,
        "the routed path must be re-establishable after the loss"
    );
    let before_restore = r.forwarded_app_packets(a_routing_id, b_id);
    a.send_routed(b_id, &batch(0, 2, "s9-restored"))
        .await
        .expect("restored routed send");
    assert!(
        delivered_with_tag(&b, "s9-restored", Duration::from_secs(15)).await >= 1,
        "phase 4 must deliver again through the anchor"
    );
    assert!(
        r.forwarded_app_packets(a_routing_id, b_id) > before_restore,
        "§10 part 3: forcing the pair back onto the anchor must move the same \
         counter that stayed flat while they were direct"
    );
}

fn b_pub_of(b: &Arc<MeshNode>) -> [u8; 32] {
    *b.public_key()
}

/// R5: four expired dialogs release their budget slots, so the
/// fifth offer is admitted.
///
/// `expire_dialogs` returned the ids it abandoned and the caller
/// **discarded** them, so an expired dialog kept its `SignalBudget`
/// slot for ever: after four attempts the peer's offers were
/// refused as over budget although no dialog was open.
///
/// Inverse: drop the `end_dialog` loop from the expiry arm — the
/// fifth offer is refused and `open_signal_dialogs` stays at four.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn four_expired_dialogs_release_their_budget_for_a_fifth() {
    let (a, b) = signalling_pair().await;
    let a_id = a.node_id();

    for dialog in 1..=4u64 {
        let (_id, sdp) = a
            .rtc_driver()
            .expect("driver")
            .create_offer()
            .await
            .expect("offer");
        a.send_rtc_signal(b.node_id(), &RtcSignalMsg::Offer { dialog, sdp })
            .await
            .expect("send offer");
    }
    assert!(
        wait_for(|| b.open_signal_dialogs(a_id) == 4, Duration::from_secs(10)).await,
        "precondition: four dialogs open against the budget (saw {})",
        b.open_signal_dialogs(a_id)
    );

    // Let every attempt reach its `ice_deadline`.
    assert!(
        wait_for(|| b.open_signal_dialogs(a_id) == 0, Duration::from_secs(20)).await,
        "expiry must release every slot it abandons (still {})",
        b.open_signal_dialogs(a_id)
    );

    // The fifth is admitted, which is the property the count is for.
    let (_id, sdp) = a
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer");
    let before = b.rtc_stats().signal_over_budget();
    a.send_rtc_signal(b.node_id(), &RtcSignalMsg::Offer { dialog: 5, sdp })
        .await
        .expect("send fifth offer");
    assert!(
        wait_for(|| b.open_signal_dialogs(a_id) == 1, Duration::from_secs(10)).await,
        "the fifth offer must be admitted once the four have ended"
    );
    assert_eq!(
        b.rtc_stats().signal_over_budget(),
        before,
        "and nothing may be counted as over budget"
    );
}

/// R5: an immediate `Reject` for a dialog **we** offered
/// correlates, instead of being refused as unknown and leaving our
/// own offer alive until its timeout.
///
/// Inverse: drop `register_outbound_dialog` from
/// `offer_direct_path` — the Reject is refused as an unknown dialog
/// and the slot is never released.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_reject_for_our_own_offer_correlates_and_releases() {
    let (a, b) = signalling_pair().await;
    let b_id = b.node_id();

    let dialog = a.offer_direct_path(b_id).await.expect("offer sent");
    assert_eq!(
        a.open_signal_dialogs(b_id),
        1,
        "our own outbound dialog must be known to the inbound budget"
    );

    // B rejects it immediately.
    b.send_rtc_signal(
        a.node_id(),
        &RtcSignalMsg::Reject {
            dialog,
            reason: net::adapter::net::rtc::RtcRejectReason::Declined,
        },
    )
    .await
    .expect("send reject");

    assert!(
        wait_for(|| a.open_signal_dialogs(b_id) == 0, Duration::from_secs(10)).await,
        "the reject must correlate with our own dialog and release its slot \
         (still {})",
        a.open_signal_dialogs(b_id)
    );
}

/// R5: a **failed allocation** holds no reservation. A malformed
/// offer costs the receiver no ICE agent, so it must not cost the
/// sender a dialog slot either — otherwise four malformed frames
/// silently exhaust a peer's allowance.
///
/// Inverse: drop `release_signal_budget` from the Reject arm — the
/// malformed offers keep their slots and the real offer that
/// follows is refused as over budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_failed_allocation_holds_no_dialog_reservation() {
    let (a, b) = signalling_pair().await;
    let a_id = a.node_id();
    let b_id = b.node_id();

    for dialog in 0..MAX_DIALOGS_PER_PEER as u64 {
        a.send_rtc_signal(
            b_id,
            &RtcSignalMsg::Offer {
                dialog,
                sdp: "v=0".to_string(),
            },
        )
        .await
        .expect("send malformed offer");
    }
    assert!(
        wait_for(|| b.open_signal_dialogs(a_id) == 0, Duration::from_secs(10)).await,
        "a refused allocation must leave no reservation behind (held {})",
        b.open_signal_dialogs(a_id)
    );

    // And a real offer is still admitted.
    let before = b.rtc_stats().signal_over_budget();
    let sdp = a
        .rtc_driver()
        .expect("driver")
        .create_offer()
        .await
        .expect("offer")
        .1;
    a.send_rtc_signal(b_id, &RtcSignalMsg::Offer { dialog: 77, sdp })
        .await
        .expect("send real offer");
    assert!(
        wait_for(|| b.open_signal_dialogs(a_id) == 1, Duration::from_secs(10)).await,
        "the real offer must be admitted after four failed allocations"
    );
    assert_eq!(
        b.rtc_stats().signal_over_budget(),
        before,
        "and nothing may be counted as over budget"
    );
}
