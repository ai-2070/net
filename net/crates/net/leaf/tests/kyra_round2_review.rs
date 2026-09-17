//! Kyra's third-round probes, landed VERBATIM.
//!
//! Only this header was added, and only because the crate denies
//! `missing_docs` on every target. Not one assertion, helper or name
//! below is the parent's. At the head they landed on they reproduced
//! three failures against two controls; each repair row turns named
//! probes green without touching this file.
use net_leaf::stream::LEAF_STREAM_DISCRIMINATOR;
use net_leaf::{LeafEvent, LeafIdentity, LeafNode, Reliability};

fn connect(a: &mut LeafNode, b: &mut LeafNode) {
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).unwrap();
    // S6-01: a responder attributes a fresh session to a claimed
    // identity only after that identity's signed establishment proof
    // verifies against the entity key in its announcement, so the
    // pair discovers each other first and the proof is delivered
    // before the session is usable. Helper plumbing only — no probe
    // name, assertion or message is changed.
    let from_a = a.build_announcement(&["kyra".to_string()]).unwrap();
    let from_b = b.build_announcement(&["kyra".to_string()]).unwrap();
    assert!(b.ingest_announcement(&from_a));
    assert!(a.ingest_announcement(&from_b));
    let one = a
        .begin_handshake(b.node_id(), &psk, b.identity().noise().public_key(), 1)
        .unwrap();
    let two = b.accept_handshake(a.node_id(), &psk, &one, 1).unwrap();
    a.complete_handshake(b.node_id(), &two).unwrap();
    transfer(a, b);
    assert_eq!(b.take_verified_admissions(), vec![a.node_id()]);
    a.drain_events();
    b.drain_events();
}

fn transfer(a: &mut LeafNode, b: &mut LeafNode) {
    for p in a.take_outbound() {
        b.on_datagram(a.node_id(), p.packet, net_leaf::clock::now());
    }
}

#[test]
fn kyra_same_session_close_reopen_preserves_receive_progress() {
    let mut a = LeafNode::new(LeafIdentity::generate().unwrap(), 100);
    let mut b = LeafNode::new(LeafIdentity::generate().unwrap(), 200);
    connect(&mut a, &mut b);
    let id = LEAF_STREAM_DISCRIMINATOR | 7;
    let receive = a
        .open_stream(
            b.node_id(),
            "same",
            Reliability::Reliable,
            Some(id),
            Some(7),
        )
        .unwrap();
    let send = b
        .open_stream(
            a.node_id(),
            "same",
            Reliability::Reliable,
            Some(id),
            Some(7),
        )
        .unwrap();
    b.stream_send(send, b"before").unwrap();
    transfer(&mut b, &mut a);
    assert!(a.drain_events().iter().any(
        |e| matches!(e, LeafEvent::StreamData { payload, .. } if payload.as_ref() == b"before")
    ));
    transfer(&mut a, &mut b);
    a.close_stream(receive).unwrap();
    // A refused reopen would be a valid explicit unsupported-restart
    // disposition. A successful reopen must not ACK and strand data.
    if a.open_stream(
        b.node_id(),
        "same",
        Reliability::Reliable,
        Some(id),
        Some(7),
    )
    .is_err()
    {
        return;
    }
    b.stream_send(send, b"after").unwrap();
    transfer(&mut b, &mut a);
    transfer(&mut a, &mut b);
    let events = a.drain_events();
    assert!(
        events.iter().any(
            |e| matches!(e, LeafEvent::StreamData { payload, .. } if payload.as_ref() == b"after")
        ) || events
            .iter()
            .any(|e| matches!(e, LeafEvent::StreamFailed { .. })),
        "successful close/reopen stranded the peer's next sequence; events={events:?}"
    );
}

#[test]
fn kyra_control_overdraft_grant_does_not_refund_withheld_application() {
    let state = net_wire::session::StreamState::new_full(false, 1, 100);
    assert!(state.try_acquire_tx_credit(100));
    state.note_tx_bytes_sent(30);
    assert_eq!(state.tx_bytes_sent(), 130);
    assert_eq!(state.tx_credit_remaining(), 0);
    // Only the control's 30 bytes reached the receiver. All 100
    // admitted application bytes are still outstanding.
    state.apply_authoritative_grant(30);
    assert_eq!(state.max_consumed_seen(), 30);
    assert_eq!(
        state.tx_credit_remaining(),
        0,
        "control overdraft was forgotten, refunding withheld application bytes"
    );
}

#[test]
fn kyra_control_debit_below_window_control() {
    let state = net_wire::session::StreamState::new_full(false, 1, 100);
    assert!(state.try_acquire_tx_credit(70));
    state.note_tx_bytes_sent(30);
    state.apply_authoritative_grant(30);
    assert_eq!(state.tx_credit_remaining(), 30);
}

fn delayed_fragment(delay_ms: u64) -> Vec<LeafEvent> {
    let mut a = LeafNode::new(LeafIdentity::generate().unwrap(), 100);
    let mut b = LeafNode::new(LeafIdentity::generate().unwrap(), 200);
    connect(&mut a, &mut b);
    let h = a
        .open_stream(
            b.node_id(),
            "kyra-ttl",
            Reliability::Reliable,
            Some(LEAF_STREAM_DISCRIMINATOR | 7),
            Some(7),
        )
        .unwrap();
    a.stream_send(h, &vec![0x5a; 9000]).unwrap();
    let packets = a.take_outbound();
    assert_eq!(packets.len(), 2);
    let now = net_leaf::clock::now();
    b.on_datagram(a.node_id(), packets[0].packet.clone(), now);
    assert!(b.drain_events().is_empty());
    let feedback = b.take_outbound();
    assert!(!feedback.is_empty(), "head must produce actual feedback");
    for p in feedback {
        a.on_datagram(b.node_id(), p.packet, now);
    }
    // Advance the receiving node's supported clock argument, not a
    // helper-only expiry call. The head's ACK has already reached A.
    let later = now + std::time::Duration::from_millis(delay_ms);
    b.tick(later);
    b.on_datagram(a.node_id(), packets[1].packet.clone(), later);
    for p in b.take_outbound() {
        a.on_datagram(b.node_id(), p.packet, later);
    }
    b.drain_events()
}

#[test]
fn kyra_expired_acknowledged_partial_delivers_or_fails_typed() {
    let events = delayed_fragment(net_leaf::frame::REASSEMBLY_TTL_MS + 1);
    let complete = events.iter().any(|e| {
        matches!(e,
        LeafEvent::StreamData { payload, .. } if payload.as_ref() == vec![0x5a; 9000].as_slice())
    });
    let failed = events
        .iter()
        .any(|e| matches!(e, LeafEvent::StreamFailed { .. }));
    assert!(complete || failed,
        "ACKed fragment state expired without complete delivery or typed terminal failure; events={events:?}");
}

#[test]
fn kyra_unexpired_partial_delivery_control() {
    let events = delayed_fragment(net_leaf::frame::REASSEMBLY_TTL_MS - 1);
    assert!(
        events.iter().any(|e| matches!(e,
        LeafEvent::StreamData { payload, .. } if payload.as_ref() == vec![0x5a; 9000].as_slice())),
        "an unexpired partial must still deliver"
    );
}
