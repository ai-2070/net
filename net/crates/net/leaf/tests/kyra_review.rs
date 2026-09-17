//! Kyra's Stage 5 review probes, landed VERBATIM.
//!
//! Only this header was added, and only because the crate denies
//! `missing_docs` on every target: not one assertion, helper or name
//! below is the parent's. They reproduced 12 failures out of 15 when
//! they landed, and each repair row turns named probes green without
//! touching the file.
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
fn kyra_small_reliable_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    a.stream_send(h, b"small").unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(payloads(&mut b), vec![b"small".to_vec()]);
}
#[test]
fn kyra_reliable_fragmented_payload_is_delivered() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    let data = vec![0x55; net_leaf::frame::MAX_FRAGMENT_PAYLOAD + 1];
    a.stream_send(h, &data).unwrap();
    transfer(&mut a, &mut b);
    assert!(
        payloads(&mut b) == vec![data],
        "complete authenticated fragment set must deliver once"
    );
}
#[test]
fn kyra_session_replacement_resets_receive_sequence() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    a.stream_send(h, b"old").unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(payloads(&mut b), vec![b"old".to_vec()]);
    connect(&mut a, &mut b, 2);
    let h = stream(&mut a, &b);
    a.stream_send(h, b"new").unwrap();
    transfer(&mut a, &mut b);
    assert_eq!(
        payloads(&mut b),
        vec![b"new".to_vec()],
        "new session sequence zero must not be old-session duplicate"
    );
}
#[test]
fn kyra_channel_publication_is_not_misclassified_as_stream() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let label = (0..100)
        .map(|i| format!("kyra/channel/{i}"))
        .find(|s| Channel::new(s).unwrap().publish_stream_id() & LEAF_STREAM_DISCRIMINATOR != 0)
        .unwrap();
    a.publish(b.node_id(), &label, b"channel data").unwrap();
    transfer(&mut a, &mut b);
    let events = b.drain_events();
    assert!(
        matches!(events.as_slice(), [LeafEvent::ChannelMessage { .. }]),
        "ordinary channel publication must be a channel event, got {events:?}"
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
fn kyra_reply_from_another_session_cannot_complete_call() {
    let (mut caller, mut intended, mut other) = (node(100), node(200), node(300));
    connect(&mut caller, &mut intended, 1);
    connect(&mut caller, &mut other, 2);
    let mut rx = caller
        .call(intended.node_id(), "app.test", b"request", Some(60_000))
        .unwrap();
    let out = caller.take_outbound();
    assert_eq!(out.len(), 2);
    // The unrelated peer knows a guessed call ID, not the intended session key.
    let reply_name = caller.reply_channel_for("app.test").unwrap();
    let c = Channel::new(&reply_name).unwrap();
    other
        .publish(
            caller.node_id(),
            &reply_name,
            &response(100, other.origin_hash(), c.canonical()),
        )
        .unwrap();
    transfer(&mut other, &mut caller);
    assert!(
        rx.try_recv().unwrap().is_none(),
        "reply from a different authenticated session completed this call"
    );
    intended
        .publish(
            caller.node_id(),
            &reply_name,
            &response(100, intended.origin_hash(), c.canonical()),
        )
        .unwrap();
    transfer(&mut intended, &mut caller);
    assert_eq!(rx.try_recv().unwrap().unwrap().unwrap().as_ref(), b"ok");
}
#[test]
fn kyra_same_peer_wrong_reply_route_cannot_complete_call() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let mut rx = a
        .call(b.node_id(), "app.test", b"request", Some(60_000))
        .unwrap();
    a.take_outbound();
    let wrong = "unrelated.channel";
    let c = Channel::new(wrong).unwrap();
    b.publish(
        a.node_id(),
        wrong,
        &response(100, b.origin_hash(), c.canonical()),
    )
    .unwrap();
    transfer(&mut b, &mut a);
    assert!(
        rx.try_recv().unwrap().is_none(),
        "reply on unrelated canonical channel completed call"
    );
}
#[test]
fn kyra_reordered_stream_events_keep_their_own_sequences() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = stream(&mut a, &b);
    a.stream_send(h, b"zero").unwrap();
    a.stream_send(h, b"one").unwrap();
    let p = a.take_outbound();
    assert_eq!(p.len(), 2);
    b.on_datagram(a.node_id(), p[1].packet.clone(), net_leaf::clock::now());
    assert!(b.drain_events().is_empty());
    b.on_datagram(a.node_id(), p[0].packet.clone(), net_leaf::clock::now());
    let seqs: Vec<_> = b
        .drain_events()
        .into_iter()
        .filter_map(|e| {
            if let LeafEvent::StreamData { seq, .. } = e {
                Some(seq)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        seqs,
        vec![0, 1],
        "released held event must keep its own sequence"
    );
}

#[test]
fn kyra_correct_peer_and_route_reply_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let mut rx = a
        .call(b.node_id(), "app.test", b"request", Some(60_000))
        .unwrap();
    a.take_outbound();
    let channel = a.reply_channel_for("app.test").unwrap();
    let c = Channel::new(&channel).unwrap();
    b.publish(
        a.node_id(),
        &channel,
        &response(100, b.origin_hash(), c.canonical()),
    )
    .unwrap();
    transfer(&mut b, &mut a);
    assert_eq!(rx.try_recv().unwrap().unwrap().unwrap().as_ref(), b"ok");
}
#[test]
fn kyra_fire_and_forget_fragment_control() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let h = a
        .open_stream(
            b.node_id(),
            "ff",
            Reliability::FireAndForget,
            Some(LEAF_STREAM_DISCRIMINATOR | 7),
            Some(7),
        )
        .unwrap();
    let data = vec![0x55; net_leaf::frame::MAX_FRAGMENT_PAYLOAD + 1];
    a.stream_send(h, &data).unwrap();
    transfer(&mut a, &mut b);
    assert!(payloads(&mut b) == vec![data]);
}
#[test]
fn kyra_session_replacement_fails_old_pending_call() {
    let (mut a, mut b) = (node(100), node(200));
    connect(&mut a, &mut b, 1);
    let mut rx = a
        .call(b.node_id(), "app.test", b"request", Some(60_000))
        .unwrap();
    a.take_outbound();
    connect(&mut a, &mut b, 2);
    assert!(
        matches!(
            rx.try_recv().unwrap(),
            Some(Err(net_leaf::RpcError::SessionLost))
        ),
        "replacement must settle calls owned by removed session"
    );
}

#[test]
fn kyra_other_entity_cannot_replace_victim_and_authorize_signal() {
    use net_leaf::{announce, clock, signal, SignalKind};
    let attacker = LeafIdentity::generate().unwrap();
    let victim = LeafIdentity::generate().unwrap();
    let mut receiver = node(1);
    let honest =
        announce::build_announcement(&victim, &[], 1, clock::now_unix_nanos(), 300).unwrap();
    assert!(receiver.ingest_announcement(&honest));
    let honest_signal = signal::sign(
        &victim,
        receiver.node_id(),
        6,
        SignalKind::Offer,
        b"v=0".to_vec(),
        clock::now_unix_secs() + 10,
    );
    assert!(
        receiver.accept_signal(honest_signal, clock::now()),
        "honest identity/signal control"
    );
    let ann =
        announce::build_announcement(&attacker, &[], 2, clock::now_unix_nanos(), 300).unwrap();
    let mut doc: serde_json::Value = serde_json::from_slice(&ann).unwrap();
    doc["node_id"] = victim.node_id().into();
    let sig = attacker
        .entity()
        .sign(&announce::signed_transcript(&doc).unwrap());
    doc["signature"] = net_leaf::identity::hex_lower(&sig).into();
    let stored = receiver.ingest_announcement(&serde_json::to_vec(&doc).unwrap());
    let mut env = signal::sign(
        &attacker,
        receiver.node_id(),
        7,
        SignalKind::Offer,
        b"v=0".to_vec(),
        clock::now_unix_secs() + 10,
    );
    env.from = victim.node_id();
    env.signature = attacker.entity().sign(&env.signing_bytes()).to_vec();
    let accepted = receiver.accept_signal(env, clock::now());
    assert!(
        !stored && !accepted,
        "victim-key replacement={stored}, spoofed-origin signal accepted={accepted}"
    );
}
#[test]
fn kyra_two_distinct_ice_candidates_in_one_dialog_survive_dedup() {
    use net_leaf::{announce, clock, signal, SignalKind};
    let sender = LeafIdentity::generate().unwrap();
    let mut receiver = node(1);
    assert!(receiver.ingest_announcement(
        &announce::build_announcement(&sender, &[], 1, clock::now_unix_nanos(), 300).unwrap()
    ));
    let first = signal::sign(
        &sender,
        receiver.node_id(),
        8,
        SignalKind::Candidate,
        b"candidate-one".to_vec(),
        clock::now_unix_secs() + 10,
    );
    let second = signal::sign(
        &sender,
        receiver.node_id(),
        8,
        SignalKind::Candidate,
        b"candidate-two".to_vec(),
        clock::now_unix_secs() + 10,
    );
    assert!(receiver.accept_signal(first.clone(), clock::now()));
    assert!(
        !receiver.accept_signal(first, clock::now()),
        "exact replay refused control"
    );
    assert!(
        receiver.accept_signal(second, clock::now()),
        "distinct candidate rejected as replay"
    );
}
#[test]
fn kyra_expired_announcement_is_not_discovery_or_signal_authority() {
    use net_leaf::{announce, clock, signal, SignalKind};
    let sender = LeafIdentity::generate().unwrap();
    let mut receiver = node(1);
    let expired = announce::build_announcement(&sender, &["kyra.expired".into()], 1, 1, 0).unwrap();
    receiver.ingest_announcement(&expired);
    let discovery = receiver.query("kyra.expired");
    let env = signal::sign(
        &sender,
        receiver.node_id(),
        8,
        SignalKind::Offer,
        b"v=0".to_vec(),
        clock::now_unix_secs() + 10,
    );
    let accepted = receiver.accept_signal(env, clock::now());
    assert!(
        discovery == "[]" && !accepted,
        "expired discovery retained={}, signal accepted={accepted}",
        discovery != "[]"
    );
}
#[test]
fn kyra_incomplete_fragment_coverage_is_rejected() {
    use bytes::Bytes;
    use net_leaf::frame::{FRAG_FRAGMENTED, FRAG_LAST};
    use net_leaf::{LeafCounters, Reassembler};
    let mut r = Reassembler::new();
    let c = LeafCounters::new();
    let now = net_leaf::clock::now();
    assert!(r
        .accept(
            1,
            1,
            10,
            FRAG_FRAGMENTED,
            Bytes::from_static(b"later"),
            now,
            &c
        )
        .is_none());
    let result = r.accept(
        1,
        1,
        5,
        FRAG_FRAGMENTED | FRAG_LAST,
        Bytes::from_static(b"early"),
        now,
        &c,
    );
    assert!(
        result.is_none(),
        "group with a missing prefix and data after its declared end was assembled"
    );
}
#[test]
fn kyra_reliable_packet_build_retains_retransmit_owner() {
    use net_leaf::session::{rtc_addr, PendingHandshake};
    let a = LeafIdentity::generate().unwrap();
    let b = LeafIdentity::generate().unwrap();
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).unwrap();
    let (pending, one) = PendingHandshake::initiate(
        &psk,
        b.noise().public_key(),
        a.node_id(),
        b.node_id(),
        rtc_addr(1, 1),
    )
    .unwrap();
    let (_responder, two) = PendingHandshake::respond(
        &psk,
        b.noise(),
        a.node_id(),
        b.node_id(),
        rtc_addr(1, 1),
        &one,
    )
    .unwrap();
    let session = pending.read_msg2(&two).unwrap();
    assert!(!session.wire().has_unacked());
    let packets = session
        .build_packets(7, 0, 7, a.origin_hash(), true, b"reliable")
        .unwrap();
    assert_eq!(packets.len(), 1);
    assert!(
        session.wire().has_unacked(),
        "reliable send has no retained retransmit ownership"
    );
}
