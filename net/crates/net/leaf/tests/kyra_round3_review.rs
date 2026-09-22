//! Kyra's fourth-round probes, landed VERBATIM.
//!
//! Only this header was added, and only because the crate denies
//! `missing_docs` on every target. Not one assertion, helper or name
//! below is the parent's. At the head they landed on they reproduced
//! eight failures against four controls; each repair row turns named
//! probes green without touching this file.
//! Reviewer composition probes: payload and reliable ownership boundaries.

fn pair() -> (LeafNode, LeafNode) {
    let mut a = LeafNode::new(LeafIdentity::generate().unwrap(), 100);
    let mut b = LeafNode::new(LeafIdentity::generate().unwrap(), 200);
    connect(&mut a, &mut b);
    (a, b)
}
fn opened(a: &mut LeafNode, peer: u64, reliable: bool) -> net_leaf::StreamHandle {
    a.open_stream(
        peer,
        "mixed",
        if reliable {
            net_leaf::Reliability::Reliable
        } else {
            net_leaf::Reliability::FireAndForget
        },
        Some(net_leaf::stream::LEAF_STREAM_DISCRIMINATOR | 73),
        Some(73),
    )
    .unwrap()
}
fn transfer(a: &mut LeafNode, b: &mut LeafNode) {
    for p in a.take_outbound() {
        b.on_datagram(a.node_id(), p.packet, net_leaf::clock::now());
    }
}
fn promotion_boundary(missing_reliable: bool, drop_boundary: bool, deliver_late: bool) {
    let (mut a, mut b) = pair();
    let faf = opened(&mut a, b.node_id(), false);
    a.stream_send(faf, &[0]).unwrap();
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    assert!(b
        .drain_events()
        .iter()
        .any(|e| matches!(e,LeafEvent::StreamData{payload,..} if payload.as_ref()==[0])));
    let reliable = opened(&mut a, b.node_id(), true);
    a.stream_send(if missing_reliable { reliable } else { faf }, &[1])
        .unwrap();
    let boundary = a.take_outbound();
    assert_eq!(boundary.len(), 1);
    if !drop_boundary {
        b.on_datagram(
            a.node_id(),
            boundary[0].packet.clone(),
            net_leaf::clock::now(),
        );
    }
    for i in 2..=9u8 {
        a.stream_send(reliable, &[i]).unwrap();
    }
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    // A delayed reliable boundary really reaches the receiver; absence of
    // traffic is not the observer. The FAF-loss branch intentionally never
    // supplies a packet its sender has no obligation to rebuild.
    if missing_reliable && drop_boundary && deliver_late {
        b.on_datagram(
            a.node_id(),
            boundary[0].packet.clone(),
            net_leaf::clock::now(),
        );
        transfer(&mut b, &mut a);
    }
    let later = net_leaf::clock::now() + std::time::Duration::from_secs(20);
    a.tick(later);
    b.tick(later);
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    let mut events = b.drain_events();
    events.extend(a.drain_events());
    let delivered: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            LeafEvent::StreamData { payload, .. } if payload.len() == 1 => Some(payload[0]),
            _ => None,
        })
        .collect();
    let expected: Vec<u8> = (if missing_reliable || !drop_boundary {
        1
    } else {
        2
    }..=9u8)
        .collect();
    eprintln!("promotion case reliable={missing_reliable} drop={drop_boundary}: delivered={delivered:?}, events={events:?}");
    assert!(delivered==expected || events.iter().any(|e| matches!(e,LeafEvent::StreamFailed{..})),
        "promotion must deliver its reliable suffix or fail typed; delivered={delivered:?}, expected={expected:?}, events={events:?}");
}
#[test]
fn kyra_promotion_lossfree_control() {
    promotion_boundary(true, false, false);
}
#[test]
fn kyra_promotion_delayed_packet_still_delivers_control() {
    promotion_boundary(true, true, true);
}
#[test]
fn kyra_promotion_lost_reliable_boundary_delivers_or_fails() {
    promotion_boundary(true, true, false);
}
#[test]
fn kyra_promotion_wire_ack_does_not_claim_missing_sequence() {
    let state = net_wire::session::StreamState::new(false);
    assert!(state.with_reliability(|r| r.on_receive(0)));
    state.update_rx_seq(0);
    state.ensure_reliable();
    for seq in 2..=9 {
        state.with_reliability(|r| r.on_receive(seq));
    }
    let ack = state.with_reliability(|r| r.rx_ack_seq());
    assert!(
        ack <= 1,
        "cumulative ACK must not claim missing reliable sequence 1; ack={ack}"
    );
}
#[test]
fn kyra_promotion_past_lost_faf_boundary_settles_consumer() {
    promotion_boundary(false, true, false);
}

#[test]
fn kyra_reliable_fragment_head_promotes_consumer_before_faf_tail_traffic() {
    let (mut a, mut b) = pair();
    let faf = opened(&mut a, b.node_id(), false);
    a.stream_send(faf, &[0]).unwrap();
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    b.drain_events();
    let reliable = opened(&mut a, b.node_id(), true);
    let body = vec![0x5a; 9000];
    a.stream_send(reliable, &body).unwrap();
    let pieces = a.take_outbound();
    assert_eq!(pieces.len(), 2);
    a.stream_send(faf, &[3]).unwrap();
    let following = a.take_outbound();
    assert_eq!(following.len(), 1);
    let now = net_leaf::clock::now();
    b.on_datagram(a.node_id(), pieces[0].packet.clone(), now);
    b.on_datagram(a.node_id(), following[0].packet.clone(), now);
    b.on_datagram(a.node_id(), pieces[1].packet.clone(), now);
    transfer(&mut b, &mut a);
    let events = b.drain_events();
    assert!(events.iter().any(|e|matches!(e,LeafEvent::StreamData{payload,..} if payload.as_ref()==body.as_slice())),
        "all consistent reliable fragments arrived but consumer lost the message; events={events:?}");
}
#[test]
fn kyra_faf_fragment_loss_does_not_kill_following_faf_messages() {
    let (mut a, mut b) = pair();
    let faf = opened(&mut a, b.node_id(), false);
    a.stream_send(faf, &vec![0x5a; 9000]).unwrap();
    let pieces = a.take_outbound();
    assert_eq!(pieces.len(), 2);
    let now = net_leaf::clock::now();
    b.on_datagram(a.node_id(), pieces[0].packet.clone(), now);
    let later = now + std::time::Duration::from_millis(net_leaf::frame::REASSEMBLY_TTL_MS + 1);
    b.tick(later);
    a.stream_send(faf, b"after-loss").unwrap();
    for p in a.take_outbound() {
        b.on_datagram(a.node_id(), p.packet, later);
    }
    let events = b.drain_events();
    assert!(
        events.iter().any(
            |e| matches!(e,LeafEvent::StreamData{payload,..} if payload.as_ref()==b"after-loss")
        ),
        "expected FAF loss must not permanently terminal the receive half; events={events:?}"
    );
}

fn debit_replacement(explicit: bool) {
    use net_wire::{crypto::SessionKeys, session::NetSession};
    let random_key = || {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).unwrap();
        bytes
    };
    let keys = SessionKeys {
        tx_key: random_key(),
        rx_key: random_key(),
        session_id: u64::from_le_bytes(random_key()[..8].try_into().unwrap()),
        remote_static_pub: [0u8; 32],
        route_hop_tx_key: random_key(),
        route_hop_rx_key: random_key(),
    };
    let session = std::sync::Arc::new(NetSession::new(
        keys,
        net_leaf::session::rtc_addr(1, 1),
        1,
        false,
    ));
    let id = 73;
    if explicit {
        session.open_stream_with(id, false, 1);
    }
    let old = session.next_tx_seq_charged(id, 30);
    assert_eq!(old.seq(), 0);
    session.close_stream(id);
    if explicit {
        session.open_stream_with(id, false, 1);
    }
    let successor = session.next_tx_seq_charged(id, 30);
    assert_eq!(successor.seq(), 0);
    successor.commit();
    let before = {
        let state = session.try_stream(id).unwrap();
        (
            state.tx_bytes_sent(),
            state.tx_credit_remaining(),
            state.current_tx_seq(),
        )
    };
    drop(old);
    let after = {
        let state = session.try_stream(id).unwrap();
        (
            state.tx_bytes_sent(),
            state.tx_credit_remaining(),
            state.current_tx_seq(),
        )
    };
    assert_eq!(
        after, before,
        "a predecessor control debit must not refund or rewind successor ownership"
    );
}
#[test]
fn kyra_explicit_epoch_debit_replacement_control() {
    debit_replacement(true);
}
#[test]
fn kyra_implicit_epoch_debit_cannot_rewind_successor() {
    debit_replacement(false);
}

#[test]
fn kyra_large_cumulative_grant_retires_all_control_debt() {
    let state = net_wire::session::StreamState::new_full(false, 1, 100);
    assert!(state.try_acquire_tx_credit(100));
    state.note_tx_bytes_sent(u32::MAX);
    state.note_tx_bytes_sent(31);
    let sent = state.tx_bytes_sent();
    state.apply_authoritative_grant(sent);
    assert_eq!(
        state.tx_credit_remaining(),
        100,
        "all charged bytes were consumed; a large cumulative grant must reopen the entire window"
    );
}

use net_leaf::{Channel, LeafEvent, LeafIdentity, LeafNode};

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

fn published_deadline_shaped_bytes(match_carrier: bool) {
    let mut sender = LeafNode::new(LeafIdentity::generate().unwrap(), 100);
    let mut receiver = LeafNode::new(LeafIdentity::generate().unwrap(), 200);
    connect(&mut sender, &mut receiver);
    let name = "kyra.application.payload";
    let channel = Channel::new(name).unwrap();
    receiver.subscribe(sender.node_id(), name).unwrap();
    for p in receiver.take_outbound() {
        sender.on_datagram(receiver.node_id(), p.packet, net_leaf::clock::now());
    }
    sender.take_outbound();
    sender.drain_events();
    receiver.drain_events();
    let mut payload = vec![0u8; 40];
    payload[0] = 0x13;
    let route = if match_carrier {
        channel.canonical()
    } else {
        channel.canonical() ^ 1
    };
    payload[24..32].copy_from_slice(&route.to_le_bytes());
    sender.publish(receiver.node_id(), name, &payload).unwrap();
    let packets = sender.take_outbound();
    assert!(!packets.is_empty());
    for p in packets {
        receiver.on_datagram(sender.node_id(), p.packet, net_leaf::clock::now());
    }
    let events = receiver.drain_events();
    assert!(events.iter().any(|e| matches!(e, LeafEvent::ChannelMessage { payload: bytes, .. } if bytes.as_ref() == payload.as_slice())),
        "public publish must deliver arbitrary application bytes unchanged, even when bytes name their own carrier; events={events:?}");
}

#[test]
fn kyra_deadline_shaped_application_bytes_other_route_control() {
    published_deadline_shaped_bytes(false);
}

#[test]
fn kyra_deadline_shaped_application_bytes_matching_route_are_data() {
    published_deadline_shaped_bytes(true);
}
