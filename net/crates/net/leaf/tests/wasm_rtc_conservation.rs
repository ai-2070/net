//! **S6-06, the leaf half: a replaced `PeerLink` conserves the
//! ledger.**
//!
//! §2's conservation law is
//! `accepted == written + discarded_at_close + retained`: a packet
//! admitted into the retained queue belongs to the transport, and
//! the only place one is lost is a channel closing — where it is
//! counted.
//!
//! `close(peer)` performed that count. Replacement did not.
//! `create_offer`/`accept_offer` install a link by inserting over
//! the peer's key, and the incumbent's destructor closed the
//! `RTCPeerConnection` and its channel while skipping the
//! accounting — so a queue that still held admitted packets simply
//! vanished: the `retained` gauge dropped with it and nothing
//! recorded where those packets went. The ledger then read
//! `accepted > written + discarded_at_close + retained`, which is a
//! transport reporting fewer packets than it was given.
//!
//! # Why this test is in a browser and not on the host
//!
//! `net_leaf::rtc` is `#![cfg(target_arch = "wasm32")]`: the queue,
//! its flush and the destructor are all written against a real
//! `RTCDataChannel`, and the state the witness needs — an OPEN
//! channel whose `bufferedAmount` has crossed the advisory, so a
//! just-admitted packet stays retained instead of being written —
//! is a property of a real SCTP association. There is no host
//! stand-in for it, and a fake one would assert the fake.
//!
//! Two transports in one page, no anchor, no carrier, no signalling
//! layer: the offer, the answer and the candidates are handed
//! across directly, because what is under test is the transport's
//! own ledger.
//!
//! Run:
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test wasm_rtc_conservation
//! ```

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use bytes::Bytes;
use net_leaf::bootstrap::gloo_timer_sleep;
use net_leaf::control_plane::NodeId;
use net_leaf::counters::RtcLinkSnapshot;
use net_leaf::rtc::RtcLeafTransport;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// Two tabs on one origin pair on host candidates, so this is
/// generous rather than tuned.
const DEADLINE_MS: i32 = 15_000;

/// Poll interval.
const TICK_MS: i32 = 50;

const A: NodeId = 0x0A11;
const B: NodeId = 0x0B22;

/// One packet, deliberately larger than any engine's SCTP
/// `maxMessageSize` (Chromium caps at 256 KiB, Firefox around
/// 1 MiB) and comfortably inside the leaf's own 8 MiB reserved-bytes
/// bound — so admission ACCEPTS it and the write refuses it.
const PACKET_BYTES: usize = 4 * 1024 * 1024;

/// The law, restated as an assertion so a failure names it.
fn assert_conserved(label: &str, snapshot: &RtcLinkSnapshot) {
    assert_eq!(
        snapshot.accepted,
        snapshot.written + snapshot.discarded_at_close + snapshot.retained,
        "{label}: §2 conservation — every accepted packet is written, discarded \
         at close, or still retained: {snapshot:?}"
    );
}

/// Drive both sides' ICE until each reports its channel open.
async fn connect(a: &RtcLeafTransport, b: &RtcLeafTransport) {
    let offer = a.create_offer(B, &[]).await.expect("A offers");
    let answer = b.accept_offer(A, &offer, &[]).await.expect("B answers");
    a.accept_answer(B, &answer)
        .await
        .expect("A takes the answer");

    let mut waited = 0;
    while waited < DEADLINE_MS {
        for (peer, candidate) in a.take_local_candidates() {
            debug_assert_eq!(peer, B);
            let _ = b.add_remote_candidate(A, &candidate).await;
        }
        for (peer, candidate) in b.take_local_candidates() {
            debug_assert_eq!(peer, A);
            let _ = a.add_remote_candidate(B, &candidate).await;
        }
        if a.is_open(B) && b.is_open(A) {
            return;
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    panic!(
        "both channels must open on host candidates (a_open={}, b_open={})",
        a.is_open(B),
        b.is_open(A)
    );
}

/// Leave exactly one admitted packet in the retained queue.
///
/// This is the state the whole witness turns on: with an empty
/// queue a replacement discards nothing and the defect is
/// unobservable.
///
/// The mechanism is the transport's own documented retain-and-retry
/// path, not a contrivance. Admission bounds reserved slots and
/// bytes and reads `bufferedAmount`; a packet over the channel's
/// `maxMessageSize` passes all three, and then
/// `RTCDataChannel.send` throws. §2's answer to that is NOT a loss:
/// the packet stays at the head of the queue and
/// `bufferedamountlow` retries it — `write_false`, the same term
/// the native driver records. Which leaves it exactly where this
/// witness needs it.
fn retain_one_unwritable_packet(transport: &RtcLeafTransport) -> u64 {
    let packet = Bytes::from(vec![0xA7u8; PACKET_BYTES]);
    transport.send(B, packet).expect(
        "admission bounds slots, bytes and bufferedAmount — none of which \
                 this packet exceeds",
    );
    transport.link_snapshot().retained
}

/// **The row.** A link replaced by `create_offer` accounts for the
/// packets its queue still held, exactly as an explicit
/// `close(peer)` does.
///
/// Inverse: delete the discard block from `PeerLink::drop` in
/// `leaf/src/rtc.rs` (the `if discarded > 0 { … }` arm) — the
/// replacement's retained packets leave the ledger unaccounted and
/// the `assert_conserved` after the replacement fails with
/// `accepted` exceeding the sum.
#[wasm_bindgen_test]
async fn a_replaced_link_conserves_the_packets_it_discards() {
    let a = RtcLeafTransport::new(Rc::new(|_, _| {}));
    let b = RtcLeafTransport::new(Rc::new(|_, _| {}));
    connect(&a, &b).await;

    let retained = retain_one_unwritable_packet(&a);
    assert!(
        retained > 0,
        "the witness needs a non-empty retained queue: with nothing queued a \
         replacement discards nothing and the defect cannot show. Snapshot: {:?}",
        a.link_snapshot()
    );
    let before = a.link_snapshot();
    assert_conserved("before replacement", &before);
    assert_eq!(
        before.discarded_at_close, 0,
        "nothing has closed yet: {before:?}"
    );

    // **The replacement.** A second attempt for the same peer
    // inserts over the incumbent's key; the incumbent's destructor
    // is the close.
    a.create_offer(B, &[]).await.expect("A re-offers");

    let after = a.link_snapshot();
    assert_conserved("after replacement", &after);
    assert_eq!(
        after.accepted, before.accepted,
        "a replacement admits nothing: {after:?}"
    );
    assert_eq!(
        after.discarded_at_close, retained,
        "the replaced link's queue is discarded AND counted — exactly what \
         explicit close does: {after:?}"
    );
    assert_eq!(
        after.retained, 0,
        "the fresh link starts with an empty queue: {after:?}"
    );

    // The explicit path, on the fresh link, still behaves as it
    // always did — so the destructor doing the accounting has not
    // double-charged it.
    let fresh_discarded = a.close(B);
    assert_eq!(
        fresh_discarded, 0,
        "an empty queue discards nothing, and close reports what it caused"
    );
    let closed = a.link_snapshot();
    assert_conserved("after close", &closed);
    assert_eq!(
        closed.discarded_at_close, retained,
        "closing an empty link adds no discard term: {closed:?}"
    );

    b.close_all();
}

/// The same law across `accept_offer`, which replaces links on the
/// answering path, plus the explicit close it composes with.
///
/// Inverse: the same deletion. This one fails at
/// `after.discarded_at_close`, which reads 0.
#[wasm_bindgen_test]
async fn the_answering_path_conserves_a_replaced_links_queue() {
    let a = RtcLeafTransport::new(Rc::new(|_, _| {}));
    let b = RtcLeafTransport::new(Rc::new(|_, _| {}));
    connect(&a, &b).await;

    let retained = retain_one_unwritable_packet(&a);
    assert!(retained > 0, "{:?}", a.link_snapshot());

    // A fresh offer from the peer, answered on top of the live
    // link: `accept_offer` inserts over the same key.
    let c = RtcLeafTransport::new(Rc::new(|_, _| {}));
    let offer = c.create_offer(A, &[]).await.expect("C offers");
    a.accept_offer(B, &offer, &[])
        .await
        .expect("A answers over the incumbent");

    let after = a.link_snapshot();
    assert_conserved("after answering replacement", &after);
    assert_eq!(
        after.discarded_at_close, retained,
        "the answering path owes the same discard term: {after:?}"
    );

    a.close_all();
    b.close_all();
    c.close_all();
}
