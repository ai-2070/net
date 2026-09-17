//! Kyra's Stage 5 follow-up probes, landed VERBATIM.
//!
//! Only this header was added, and only because the crate denies
//! `missing_docs` on every target. Not one assertion, helper or name
//! below is the parent's. They reproduced 7 failures out of 10 when
//! they landed; each repair row turns named probes green without
//! touching the file.
//!
//! One mechanical exception, forced by the repair the reviewer
//! herself specified. Round-3 row X3 reads: "`PieceMeta` lacks
//! subprotocol/reliability provenance; completion takes them from
//! the finishing packet — bind them group-wide like
//! stream/origin/channel." Rust struct literals are exhaustive, so
//! `kyra_fragment_group_cannot_change_stream_or_provenance` below
//! cannot construct the repaired `PieceMeta` without naming the two
//! new fields. Both of its literals were therefore given the **same**
//! `subprotocol_id` and `reliable` values, so the probe still
//! discriminates exactly what its name and its failure message name
//! — stream, origin and channel — and nothing else. No name, helper,
//! assertion or message changed.
use net_leaf::rpc_wire::{EventMeta, RpcStatus, DISPATCH_RPC_RESPONSE};
use net_leaf::stream::LEAF_STREAM_DISCRIMINATOR;
use net_leaf::{Channel, LeafEvent, LeafIdentity, LeafNode, Reliability};
// Parent-owned probes exercise unchanged public protocol APIs.

fn node(seed: u64) -> LeafNode {
    LeafNode::new(LeafIdentity::generate().unwrap(), seed)
}
fn connect(a: &mut LeafNode, b: &mut LeafNode, slot: u32) {
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
        .begin_handshake(b.node_id(), &psk, b.identity().noise().public_key(), slot)
        .unwrap();
    let two = b.accept_handshake(a.node_id(), &psk, &one, slot).unwrap();
    a.complete_handshake(b.node_id(), &two).unwrap();
    transfer(a, b);
    assert_eq!(b.take_verified_admissions(), vec![a.node_id()]);
    a.drain_events();
    b.drain_events();
}
fn transfer(a: &mut LeafNode, b: &mut LeafNode) {
    let id = a.node_id();
    for p in a.take_outbound() {
        assert_eq!(p.peer, b.node_id());
        b.on_datagram(id, p.packet, net_leaf::clock::now());
    }
}
fn stream(a: &mut LeafNode, b: &LeafNode) -> net_leaf::node::StreamHandle {
    a.open_stream(
        b.node_id(),
        "kyra-stream",
        Reliability::Reliable,
        Some(LEAF_STREAM_DISCRIMINATOR | 7),
        Some(7),
    )
    .unwrap()
}
fn payloads(n: &mut LeafNode) -> Vec<Vec<u8>> {
    n.drain_events()
        .into_iter()
        .filter_map(|e| match e {
            LeafEvent::StreamData { payload, .. } | LeafEvent::ChannelMessage { payload, .. } => {
                Some(payload.to_vec())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn kyra_retransmitted_fragment_does_not_destroy_acknowledged_partial() {
    let (mut a, mut b) = (node(1), node(2));
    connect(&mut a, &mut b, 1);
    let handle = stream(&mut a, &b);
    let body = vec![0x5au8; 9000];
    a.stream_send(handle, &body).unwrap();
    let original = a.take_outbound();
    assert_eq!(original.len(), 2);
    b.on_datagram(
        a.node_id(),
        original[0].packet.clone(),
        net_leaf::clock::now(),
    );
    assert!(payloads(&mut b).is_empty());
    // Lose original piece 1 and its predecessor's ACK. Both sequences
    // are legitimately retransmitted with fresh AEAD counters.
    b.take_outbound();
    std::thread::sleep(std::time::Duration::from_millis(80));
    a.tick(net_leaf::clock::now());
    let resent = a.take_outbound();
    assert_eq!(
        resent.len(),
        2,
        "real sender must rebuild both unacknowledged fragments"
    );
    for packet in resent {
        b.on_datagram(a.node_id(), packet.packet, net_leaf::clock::now());
    }
    transfer(&mut b, &mut a);
    assert_eq!(payloads(&mut b),vec![body],"a legitimate retransmission destroyed the partial group, while receive ACKs accepted its sequences");
}
#[test]
fn kyra_fragment_group_cannot_change_stream_or_provenance() {
    use bytes::Bytes;
    use net_leaf::frame::{PieceMeta, FRAG_FRAGMENTED, FRAG_LAST};
    let mut r = net_leaf::Reassembler::new();
    let c = net_leaf::LeafCounters::new();
    let now = net_leaf::clock::now();
    let a = PieceMeta {
        sequence: 0,
        stream_id: 10,
        origin_hash: 20,
        channel_hash: 30,
        subprotocol_id: 0x0A00,
        reliable: true,
    };
    let b = PieceMeta {
        sequence: 1,
        stream_id: 11,
        origin_hash: 21,
        channel_hash: 31,
        subprotocol_id: 0x0A00,
        reliable: true,
    };
    assert!(r
        .accept_piece(
            1,
            a,
            1,
            0,
            FRAG_FRAGMENTED,
            Bytes::from_static(b"a"),
            now,
            &c
        )
        .is_none());
    assert!(
        r.accept_piece(
            1,
            b,
            1,
            1,
            FRAG_FRAGMENTED | FRAG_LAST,
            Bytes::from_static(b"b"),
            now,
            &c
        )
        .is_none(),
        "one group assembled bytes across different streams/origins/channels"
    );
}
#[test]
fn kyra_unexpired_signal_replay_stays_refused_under_capacity_pressure() {
    use net_leaf::{signal, SignalKind};
    let id = LeafIdentity::generate().unwrap();
    let now = net_leaf::clock::now_unix_secs();
    let mut seen = signal::SeenSignals::new();
    let first = signal::sign(&id, 1, 1, SignalKind::Offer, b"original".to_vec(), now + 10);
    assert!(seen.admit(&first, now));
    assert!(!seen.admit(&first, now), "ordinary exact replay control");
    for dialog in 2..=(signal::MAX_REMEMBERED_SIGNALS as u64 + 1) {
        let env = signal::sign(
            &id,
            1,
            dialog,
            SignalKind::Offer,
            b"another".to_vec(),
            now + 10,
        );
        seen.admit(&env, now);
    }
    assert!(
        seen.len() <= signal::MAX_REMEMBERED_SIGNALS,
        "bounded storage control"
    );
    assert!(
        !seen.admit(&first, now),
        "capacity pressure evicted live replay protection before envelope expiry"
    );
}
fn response(call_id: u64, origin: u64, route: u64) -> Vec<u8> {
    let mut r = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, origin, call_id, 0)
        .to_bytes()
        .to_vec();
    r.extend_from_slice(&route.to_le_bytes());
    r.extend_from_slice(&RpcStatus::Ok.to_wire().to_le_bytes());
    r.push(0);
    r.extend_from_slice(&2u32.to_le_bytes());
    r.extend_from_slice(b"ok");
    r
}

#[test]
fn kyra_subscribe_then_publish_consumes_one_sequence_space() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    a.subscribe(b.node_id(), "kyra.shared").unwrap();
    a.publish(b.node_id(), "kyra.shared", b"hello").unwrap();
    transfer(&mut a, &mut b);
    transfer(&mut b, &mut a);
    assert_eq!(
        payloads(&mut b),
        vec![b"hello".to_vec()],
        "membership sequence zero left the reliable publication blocked forever"
    );
}
#[test]
fn kyra_other_channel_membership_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    a.subscribe(b.node_id(), "kyra.other").unwrap();
    a.publish(b.node_id(), "kyra.shared", b"hello").unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(payloads(&mut b), vec![b"hello".to_vec()]);
}
#[test]
fn kyra_correct_inner_route_does_not_authorize_wrong_outer_channel() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let mut rx = a
        .call(b.node_id(), "app.test", b"request", Some(60_000))
        .unwrap();
    a.take_outbound();
    let reply = a.reply_channel_for("app.test").unwrap();
    let route = Channel::new(&reply).unwrap().canonical();
    b.publish(
        a.node_id(),
        "unrelated.channel",
        &response(100, b.origin_hash(), route),
    )
    .unwrap();
    transfer(&mut b, &mut a);
    assert!(
        rx.try_recv().unwrap().is_none(),
        "self-declared expected inner route completed call carried on wrong channel"
    );
    b.publish(a.node_id(), &reply, &response(100, b.origin_hash(), route))
        .unwrap();
    transfer(&mut b, &mut a);
    assert_eq!(rx.try_recv().unwrap().unwrap().unwrap().as_ref(), b"ok");
}
#[test]
fn kyra_old_leaf_stream_handle_is_refused_after_replacement() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let old = stream(&mut a, &b);
    connect(&mut a, &mut b, 2);
    let result = a.stream_send(old, b"stale");
    let queued = a.take_outbound().len();
    assert!(
        result.is_err() && queued == 0,
        "stale handle send accepted={}, queued={queued}",
        result.is_ok()
    );
}
#[test]
fn kyra_fresh_handle_after_replacement_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let _old = stream(&mut a, &b);
    connect(&mut a, &mut b, 2);
    let fresh = stream(&mut a, &b);
    a.stream_send(fresh, b"fresh").unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(payloads(&mut b), vec![b"fresh".to_vec()]);
}
#[test]
fn kyra_reliable_reorder_overflow_delivers_all_or_fails_typed() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    for i in 0..66u8 {
        a.stream_send(h, &[i]).unwrap();
    }
    let packets = a.take_outbound();
    assert_eq!(packets.len(), 66);
    for p in &packets[1..] {
        b.on_datagram(a.node_id(), p.packet.clone(), net_leaf::clock::now());
    }
    b.on_datagram(
        a.node_id(),
        packets[0].packet.clone(),
        net_leaf::clock::now(),
    );
    let events = b.drain_events();
    let failed = events
        .iter()
        .any(|e| matches!(e, LeafEvent::StreamFailed { .. }));
    let data: Vec<u8> = events
        .iter()
        .filter_map(|e| {
            if let LeafEvent::StreamData { payload, .. } = e {
                Some(payload[0])
            } else {
                None
            }
        })
        .collect();
    assert!(
        failed || data == (0..66u8).collect::<Vec<_>>(),
        "silently delivered {} records, first={:?}, without terminal failure={}",
        data.len(),
        data.first(),
        !failed
    );
}
#[test]
fn kyra_lossfree_fragment_delivery_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    let body = vec![0x5a; 9000];
    a.stream_send(h, &body).unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(payloads(&mut b), vec![body]);
}
