//! Independent fourth-repair composition controls.
use net_leaf::{LeafEvent, LeafIdentity, LeafNode, Reliability, StreamHandle};
fn pair() -> (LeafNode, LeafNode) {
    let mut a = LeafNode::new(LeafIdentity::generate().unwrap(), 100);
    let mut b = LeafNode::new(LeafIdentity::generate().unwrap(), 200);
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).unwrap();
    let one = a
        .begin_handshake(b.node_id(), &psk, b.identity().noise().public_key(), 1)
        .unwrap();
    let two = b.accept_handshake(a.node_id(), &psk, &one, 1).unwrap();
    a.complete_handshake(b.node_id(), &two).unwrap();
    a.drain_events();
    b.drain_events();
    (a, b)
}
fn opened(a: &mut LeafNode, peer: u64, reliability: Reliability) -> StreamHandle {
    a.open_stream(
        peer,
        "mixed",
        reliability,
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
fn lost_faf_prefix(reordered_boundary: bool, fragment_prefix: bool) {
    let (mut a, mut b) = pair();
    let faf = opened(&mut a, b.node_id(), Reliability::FireAndForget);
    a.stream_send(faf, &[0]).unwrap();
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    b.drain_events();
    if fragment_prefix {
        a.stream_send(faf, &vec![0x5a; 9000]).unwrap();
        let prefix = a.take_outbound();
        assert_eq!(prefix.len(), 2);
        b.on_datagram(
            a.node_id(),
            prefix[0].packet.clone(),
            net_leaf::clock::now(),
        );
    } else {
        a.stream_send(faf, &[1]).unwrap();
        assert_eq!(a.take_outbound().len(), 1);
    }
    // The lost packet was actually sent FAF BEFORE promotion, rather
    // than through an inherited FAF handle inside a reliable region.
    let reliable = opened(&mut a, b.node_id(), Reliability::Reliable);
    a.stream_send(reliable, &[2]).unwrap();
    let boundary = a.take_outbound();
    assert_eq!(boundary.len(), 1);
    a.stream_send(reliable, &[3]).unwrap();
    let suffix = a.take_outbound();
    assert_eq!(suffix.len(), 1);
    let packets = if reordered_boundary {
        [suffix[0].packet.clone(), boundary[0].packet.clone()]
    } else {
        [boundary[0].packet.clone(), suffix[0].packet.clone()]
    };
    for packet in packets {
        b.on_datagram(a.node_id(), packet, net_leaf::clock::now());
    }
    transfer(&mut b, &mut a);
    let later = net_leaf::clock::now()
        + std::time::Duration::from_millis(net_leaf::frame::REASSEMBLY_TTL_MS + 1);
    b.tick(later);
    a.stream_send(reliable, &[4]).unwrap();
    transfer(&mut a, &mut b);
    let events = b.drain_events();
    let delivered: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            LeafEvent::StreamData { payload, .. } => Some(payload.to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(
        delivered,
        vec![vec![2], vec![3], vec![4]],
        "reliable suffix must survive genuine lost FAF prefix: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, LeafEvent::StreamFailed { .. })),
        "FAF prefix must not terminal reliable successor: {events:?}"
    );
}
#[test]
fn kyra_real_faf_loss_before_promotion_ordered() {
    lost_faf_prefix(false, false);
}
#[test]
fn kyra_real_faf_loss_before_promotion_reordered() {
    lost_faf_prefix(true, false);
}
#[test]
fn kyra_partial_faf_expiry_after_promotion_ordered() {
    lost_faf_prefix(false, true);
}
#[test]
fn kyra_partial_faf_expiry_after_promotion_reordered() {
    lost_faf_prefix(true, true);
}

fn wire_session() -> net_wire::session::NetSession {
    use net_wire::crypto::SessionKeys;
    let random_key = || {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).unwrap();
        key
    };
    net_wire::session::NetSession::new(
        SessionKeys {
            tx_key: random_key(),
            rx_key: random_key(),
            session_id: 99,
            remote_static_pub: [0u8; 32],
            route_hop_tx_key: random_key(),
            route_hop_rx_key: random_key(),
        },
        net_leaf::session::rtc_addr(1, 1),
        1,
        false,
    )
}
#[test]
fn kyra_reset_replaces_receive_boundary_not_send_half() {
    let session = wire_session();
    let id = 73;
    {
        let old = session.get_or_create_stream_for_packet(id, true, Some(0));
        assert!(old.with_reliability(|r| r.on_receive(0)));
        old.update_rx_seq(0);
        assert_eq!(old.next_tx_seq(), 0);
    }
    session.reset_rx_stream(id);
    let new = session.get_or_create_stream_for_packet(id, true, Some(1));
    assert!(new.with_reliability(|r| r.on_receive(1)));
    assert_eq!(new.current_tx_seq(), 1, "reset must preserve TX half");
    assert_eq!(
        new.with_reliability(|r| r.rx_ack_seq()),
        2,
        "new signalled boundary concedes lost FAF sequence 0 after reset"
    );
}
#[test]
fn kyra_native_flags_cannot_false_ack_lost_reliable_head() {
    let session = wire_session();
    let id = 73;
    for (seq, reliable) in [(0, false), (2, false), (3, true)] {
        // Mechanism probe: flags are those emitted by native
        // send_on_stream's independently configured FAF/reliable handles.
        let state = session.get_or_create_stream_for_packet(id, reliable, None);
        assert!(state.with_reliability(|r| r.on_receive(seq)));
        state.update_rx_seq(seq);
    }
    let ack = session
        .try_stream(id)
        .unwrap()
        .with_reliability(|r| r.rx_ack_seq());
    assert!(
        ack <= 1,
        "native mixed-producer sequence 1 was reliably sent but absent: ACK {ack}"
    );
}
fn completed_duplicate(duplicate_head: bool) {
    let (mut a, mut b) = pair();
    let stream = opened(&mut a, b.node_id(), Reliability::Reliable);
    let body = vec![0x5a; 9000];
    a.stream_send(stream, &body).unwrap();
    transfer(&mut a, &mut b);
    let first = b.drain_events();
    assert_eq!(first.iter().filter(|e| matches!(e, LeafEvent::StreamData {payload,..} if payload.as_ref()==body.as_slice())).count(), 1);
    if duplicate_head {
        // Withhold feedback; the production sender rebuilds fresh-counter packets.
        b.take_outbound();
        std::thread::sleep(std::time::Duration::from_millis(80));
        a.tick(net_leaf::clock::now());
        let resent = a.take_outbound();
        assert_eq!(resent.len(), 2);
        b.on_datagram(
            a.node_id(),
            resent[0].packet.clone(),
            net_leaf::clock::now(),
        );
    }
    transfer(&mut b, &mut a);
    b.tick(
        net_leaf::clock::now()
            + std::time::Duration::from_millis(net_leaf::frame::REASSEMBLY_TTL_MS + 1),
    );
    a.stream_send(stream, b"after-complete").unwrap();
    transfer(&mut a, &mut b);
    let events = b.drain_events();
    assert!(events.iter().any(|e| matches!(e, LeafEvent::StreamData {payload,..} if payload.as_ref()==b"after-complete")), "already-delivered group's duplicate must not kill stream: {events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, LeafEvent::StreamFailed { .. })),
        "no live obligation was abandoned: {events:?}"
    );
}
#[test]
fn kyra_completed_fragment_duplicate_does_not_terminal() {
    completed_duplicate(true);
}
#[test]
fn kyra_completed_fragment_no_duplicate_control() {
    completed_duplicate(false);
}
fn rpc_carrier_collision(registered_rpc: bool) {
    let (mut a, mut b) = pair();
    let service = "kyra.reply.ownership";
    if registered_rpc {
        let _pending = b.call(a.node_id(), service, b"q", Some(1)).unwrap();
        assert_eq!(
            b.tick(net_leaf::clock::now() + std::time::Duration::from_millis(2)),
            1
        );
        b.take_outbound();
        b.drain_events();
    }
    let name = b.reply_channel_for(service).unwrap();
    let route = net_leaf::Channel::new(&name).unwrap().canonical();
    let carrier = net_leaf::channel::publish_stream_id(route);
    let admitted = b.open_stream(
        a.node_id(),
        "application",
        Reliability::Reliable,
        Some(carrier),
        Some(route as u16),
    );
    if registered_rpc && admitted.is_err() {
        return;
    }
    admitted.unwrap();
    let send = a
        .open_stream(
            b.node_id(),
            "application",
            Reliability::Reliable,
            Some(carrier),
            Some(route as u16),
        )
        .unwrap();
    a.stream_send(send, b"opaque").unwrap();
    transfer(&mut a, &mut b);
    let events = b.drain_events();
    assert!(
        events.iter().any(
            |e| matches!(e, LeafEvent::StreamData {payload,..} if payload.as_ref()==b"opaque")
        ),
        "successful application registration must own delivery or refuse the conflict: {events:?}"
    );
}
#[test]
fn kyra_rpc_carrier_cannot_silently_shadow_admitted_stream() {
    rpc_carrier_collision(true);
}
#[test]
fn kyra_unowned_carrier_application_control() {
    rpc_carrier_collision(false);
}
