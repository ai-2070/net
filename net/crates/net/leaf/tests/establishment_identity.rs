//! S6-01: a responder authenticates the **individual** initiator
//! before treating a fresh session as that initiator's.
//!
//! The reviewer's reproduction, made a standing witness. She
//! discarded honest A after taking its public announcement, built an
//! independent NKpsk0 initiator with the domain PSK and B's *public*
//! responder static, claimed A's node id, and B classified it as A,
//! admitted it under the production routed-responder conditions,
//! installed a session keyed as A and handed it an application
//! payload addressed to A. Nothing in that chain needed a byte of
//! A's private key material.
//!
//! Every case here runs through the public leaf API on real compiled
//! production code: `LeafNode::classify_datagram`,
//! `accept_handshake`, `on_datagram`, `open_stream`, `stream_send`,
//! `take_outbound`, `PendingHandshake`, `LeafSession` and the
//! counters. There is no test double and no reimplemented handshake.
//!
//! The controls matter as much as the negatives: a refusal that
//! passes because nothing works at all is not a repair, so the
//! honest initiator is installed in the same file, over the same
//! helpers, in both the routed and the direct shape.

use std::time::Duration;

use bytes::Bytes;
use net_leaf::counters::DropReason;
use net_leaf::establish::{
    EstablishmentProof, ProofRole, PROOF_DEADLINE_MS, SUBPROTOCOL_ESTABLISHMENT_PROOF,
};
use net_leaf::node::Inbound;
use net_leaf::session::{rtc_addr, LeafSession, PendingHandshake};
use net_leaf::{LeafIdentity, LeafNode, Reliability};
use net_wire::route_codec::RoutingHeader;

/// The relay that carries a routed establishment and is party to
/// neither end of it.
const RELAY: u64 = 0x5555_4444_3333_2222;

fn node(seed: u64) -> LeafNode {
    LeafNode::new(LeafIdentity::generate().unwrap(), seed)
}

fn psk() -> [u8; 32] {
    let mut p = [0u8; 32];
    getrandom::fill(&mut p).unwrap();
    p
}

/// A routing envelope, exactly as a relay forwards one.
fn wrap(from: u64, to: u64, packet: &[u8]) -> Bytes {
    #[allow(clippy::cast_possible_truncation)]
    let mut b = RoutingHeader::new(to, from as u32, 8).to_bytes().to_vec();
    b.extend_from_slice(packet);
    Bytes::from(b)
}

/// Seal `payload` under `session` on the establishment-proof
/// subprotocol and hand it to `to` as a relayed datagram.
fn deliver_proof_frame(
    to: &mut LeafNode,
    from: u64,
    session: &LeafSession,
    origin_hash: u64,
    payload: &[u8],
    now: net_wire::clock::Instant,
) {
    for packet in session
        .build_packets(
            u64::from(SUBPROTOCOL_ESTABLISHMENT_PROOF),
            SUBPROTOCOL_ESTABLISHMENT_PROOF,
            0,
            origin_hash,
            false,
            payload,
        )
        .expect("the proof frame builds")
    {
        let node_id = to.node_id();
        to.on_datagram(from, wrap(from, node_id, &packet), now);
    }
}

/// Two leaves that have discovered each other: each holds the
/// other's verified signed announcement, neither holds a session.
fn discovered() -> (LeafNode, LeafNode) {
    let mut a = node(1);
    let mut b = node(2);
    let from_a = a.build_announcement(&["probe".to_string()]).unwrap();
    let from_b = b.build_announcement(&["probe".to_string()]).unwrap();
    assert!(b.ingest_announcement(&from_a));
    assert!(a.ingest_announcement(&from_b));
    a.drain_events();
    b.drain_events();
    (a, b)
}

/// The reviewer's impostor, verbatim in shape: a same-domain PSK
/// holder claiming another node's verified identity.
///
/// It is refused. No session is installed under the claimed
/// identity, no stream can be opened to it, no application byte is
/// ever addressed to it, and the refusal is counted once under its
/// own name.
#[test]
fn an_impostor_with_the_domain_psk_is_refused_the_identity_it_claims() {
    let shared = psk();
    let mut a = node(1);
    let aid = a.node_id();
    let announcement = a.build_announcement(&["probe".to_string()]).unwrap();
    // The impostor gets A's PUBLIC announcement and nothing else: no
    // identity secret, no pending handshake, no session.
    drop(a);

    let mut b = node(2);
    let bid = b.node_id();
    assert!(b.ingest_announcement(&announcement));
    b.drain_events();

    let (impostor, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(1, 1),
    )
    .expect("the domain PSK and B's public static are enough to build message 1");

    // The production routed-responder admission conditions, checked
    // exactly as the driver checks them.
    let Inbound::Handshake {
        from,
        packet,
        relayed,
    } = b.classify_datagram(RELAY, wrap(aid, bid, &msg1))
    else {
        panic!("message 1 must classify as a relayed handshake");
    };
    assert!(relayed && from == aid && !b.has_session(from));

    let msg2 = b
        .accept_handshake(from, &shared, &packet, 2)
        .expect("the responder half still completes on message 1");
    let impostor_session = impostor.read_msg2(&msg2).expect("so does the initiator");

    // **Keys, not an identity.** This is the repair: the attempt is
    // bounded and provisional, and nothing above the transport
    // belongs to the claimed peer.
    assert!(
        !b.has_session(aid),
        "a domain-PSK holder that claimed A's id must not hold a session keyed as A"
    );
    assert!(b.provisional_attempt(aid).is_some());
    assert!(
        b.open_stream(
            aid,
            "review-identity-proof",
            Reliability::FireAndForget,
            None,
            None
        )
        .is_err(),
        "there is no session under A's identity to open an application stream on"
    );
    assert!(
        b.take_outbound().is_empty(),
        "nothing is addressed to the claimed identity before it is proven"
    );

    // The impostor now does the only thing left: it signs an
    // establishment proof with the identity it actually owns, for
    // the identity it is claiming.
    let impostor_identity = LeafIdentity::generate().unwrap();
    let forged = EstablishmentProof::sign(
        impostor_identity.entity(),
        ProofRole::Initiator,
        aid,
        bid,
        impostor_session.handshake_hash(),
    );
    let before = b.counters().drops(DropReason::EstablishmentUnproven);
    deliver_proof_frame(
        &mut b,
        aid,
        &impostor_session,
        impostor_identity.origin_hash(),
        &forged.encode(),
        net_leaf::clock::now(),
    );
    assert_eq!(
        b.counters().drops(DropReason::EstablishmentUnproven),
        before + 1,
        "the refusal advances its own counter by exactly one"
    );
    assert!(
        !b.has_session(aid),
        "a proof signed by a key that is not A's proves nothing about A"
    );
    assert!(
        b.take_outbound().is_empty(),
        "a refused establishment answers nothing"
    );

    // And the reviewer's final question: is there any application
    // payload for the impostor to read? There is nothing to send,
    // because there is no session to send it on.
    let nonce = b"review-payload-intended-for-honest-A";
    assert!(b
        .open_stream(
            aid,
            "review-identity-proof",
            Reliability::FireAndForget,
            None,
            None
        )
        .is_err());
    let stolen = b.take_outbound().into_iter().any(|out| {
        out.peer == aid
            && impostor_session
                .open_packet(&out.packet)
                .is_ok_and(|p| p.events.iter().any(|e| e.as_ref() == nonce))
    });
    assert!(
        !stolen,
        "no session attributed to honest A exists, so no A-addressed application byte \
         can reach a domain-PSK holder without A's private identity material"
    );
}

/// The honest control, over the same routed shape: a real A completes
/// and is installed. The refusal above cannot be passing by breaking
/// establishment in general.
#[test]
fn the_honest_initiator_is_installed_after_its_establishment_proof() {
    let (mut a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let b_noise = a
        .announcement_for(bid)
        .expect("a discovered b")
        .noise_pubkey
        .expect("a leaf announces its noise_pubkey");

    a.set_peer_relay(bid, RELAY);
    b.set_peer_relay(aid, RELAY);
    let msg1 = a.begin_handshake(bid, &shared, &b_noise, 1).unwrap();
    let Inbound::Handshake { from, packet, .. } =
        b.classify_datagram(RELAY, a.route_outbound(bid, msg1).packet)
    else {
        panic!("message 1");
    };
    let msg2 = b.accept_handshake(from, &shared, &packet, 1).unwrap();
    assert!(
        !b.has_session(aid),
        "even the honest initiator is provisional until it proves itself"
    );
    let attempt = b.provisional_attempt(aid).expect("the attempt exists");

    let Inbound::Handshake { from, packet, .. } =
        a.classify_datagram(RELAY, b.route_outbound(aid, msg2).packet)
    else {
        panic!("message 2");
    };
    a.complete_handshake(from, &packet).unwrap();
    assert!(
        a.has_session(bid),
        "the initiator authenticated B's static key"
    );

    // A's proof, queued by `complete_handshake`, crosses the relay.
    let mut crossed = 0;
    for out in a.take_outbound() {
        assert_eq!(out.peer, RELAY, "it goes through the relay, blind");
        let Inbound::Session {
            from: src, packet, ..
        } = b.classify_datagram(RELAY, out.packet)
        else {
            panic!("the proof rides the session");
        };
        b.on_datagram(src, packet, net_leaf::clock::now());
        crossed += 1;
    }
    assert_eq!(crossed, 1, "exactly one proof frame");

    assert!(b.has_session(aid), "the proof is what installs the session");
    assert_eq!(b.take_verified_admissions(), vec![aid]);
    assert_eq!(b.provisional_attempt(aid), None);
    assert_eq!(b.counters().drops(DropReason::EstablishmentUnproven), 0);
    assert_ne!(attempt, 0);

    // Application traffic now flows under the proven identity.
    let handle = b
        .open_stream(aid, "positions", Reliability::FireAndForget, None, None)
        .expect("a proven peer has a session");
    b.stream_send(handle, b"frame one").expect("send");
    assert_eq!(b.take_outbound().len(), 1);
}

/// A PSK from outside the trust domain is still refused on message 1,
/// before there is any attempt at all. Existing behaviour, preserved.
#[test]
fn a_wrong_domain_psk_is_still_refused_before_any_admission() {
    let (mut a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let wrong = psk();
    let shared = psk();
    let msg1 = a
        .begin_handshake(bid, &wrong, b.identity().noise().public_key(), 1)
        .unwrap();
    assert!(b.accept_handshake(aid, &shared, &msg1, 1).is_err());
    assert!(!b.has_session(aid));
    assert_eq!(
        b.provisional_attempt(aid),
        None,
        "a failed MAC creates no attempt to retire"
    );
}

/// One establishment's valid proof is worthless for another.
///
/// The transcript binds the final Noise handshake hash, so a proof
/// captured from a real completed establishment cannot promote a
/// second one — which is what a proof bound only to the peer id, the
/// announcement or the signalling dialog would have allowed.
#[test]
fn a_proof_from_another_establishment_cannot_promote_this_one() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let b_static = *b.identity().noise().public_key();

    // Establishment one, completed honestly end to end.
    let (first, msg1) =
        PendingHandshake::initiate(&shared, &b_static, aid, bid, rtc_addr(1, 1)).unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    let first = first.read_msg2(&msg2).unwrap();
    let genuine = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        first.handshake_hash(),
    );
    deliver_proof_frame(
        &mut b,
        aid,
        &first,
        a.origin_hash(),
        &genuine.encode(),
        net_leaf::clock::now(),
    );
    assert!(b.has_session(aid), "establishment one is proven");
    b.drop_session(aid, "the channel closed");
    b.drain_events();

    // Establishment two: a fresh handshake, and the captured proof
    // from establishment one replayed into it.
    let (second, msg1) =
        PendingHandshake::initiate(&shared, &b_static, aid, bid, rtc_addr(2, 1)).unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 2).unwrap();
    let second = second.read_msg2(&msg2).unwrap();
    assert_ne!(
        first.handshake_hash(),
        second.handshake_hash(),
        "each establishment has its own transcript"
    );

    let before = b.counters().drops(DropReason::EstablishmentUnproven);
    deliver_proof_frame(
        &mut b,
        aid,
        &second,
        a.origin_hash(),
        &genuine.encode(),
        net_leaf::clock::now(),
    );
    assert_eq!(
        b.counters().drops(DropReason::EstablishmentUnproven),
        before + 1
    );
    assert!(
        !b.has_session(aid),
        "a proof for a different handshake installs nothing"
    );

    // The proof for THIS establishment does install it, so the
    // refusal above is about the transcript and nothing else.
    let correct = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        second.handshake_hash(),
    );
    deliver_proof_frame(
        &mut b,
        aid,
        &second,
        a.origin_hash(),
        &correct.encode(),
        net_leaf::clock::now(),
    );
    assert!(b.has_session(aid));
}

/// The attempt's deadline retires its right to install.
///
/// A valid proof for a real establishment, arriving after the
/// attempt expired, installs nothing — and the expiry is judged
/// against the arrival's own clock reading, so the refusal is named
/// rather than inferred.
#[test]
fn a_proof_arriving_after_the_attempt_expired_installs_nothing() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let (pending, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(1, 1),
    )
    .unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    let session = pending.read_msg2(&msg2).unwrap();
    let proof = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        session.handshake_hash(),
    );

    let too_late = net_leaf::clock::now() + Duration::from_millis(PROOF_DEADLINE_MS + 1);
    deliver_proof_frame(
        &mut b,
        aid,
        &session,
        a.origin_hash(),
        &proof.encode(),
        too_late,
    );
    assert_eq!(b.counters().drops(DropReason::EstablishmentUnproven), 1);
    assert!(
        !b.has_session(aid),
        "an expired attempt cannot install, however good its proof"
    );
    assert_eq!(
        b.provisional_attempt(aid),
        None,
        "the attempt is gone, not merely flagged"
    );
    assert!(b.take_verified_admissions().is_empty());

    // Re-sending it changes nothing: there is no attempt left to own.
    deliver_proof_frame(
        &mut b,
        aid,
        &session,
        a.origin_hash(),
        &proof.encode(),
        net_leaf::clock::now(),
    );
    assert!(!b.has_session(aid));
}

/// `tick` retires an abandoned attempt with no traffic at all, so a
/// peer that vanishes after message 1 leaves nothing behind.
#[test]
fn the_periodic_sweep_retires_an_abandoned_attempt() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let (_pending, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(1, 1),
    )
    .unwrap();
    b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    assert!(b.provisional_attempt(aid).is_some());

    b.tick(net_leaf::clock::now());
    assert!(
        b.provisional_attempt(aid).is_some(),
        "the bound has not passed yet"
    );
    b.tick(net_leaf::clock::now() + Duration::from_millis(PROOF_DEADLINE_MS + 1));
    assert_eq!(b.provisional_attempt(aid), None);
    assert!(!b.has_session(aid));
}

/// Supersession retires the predecessor's right to install.
///
/// A second message 1 for the same claimed identity starts a new
/// attempt with a new incarnation, and the first attempt's proof —
/// genuinely signed, for a real handshake — can no longer install
/// anything, because the object that held that right is gone.
#[test]
fn a_superseded_attempt_loses_its_right_to_install() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let b_static = *b.identity().noise().public_key();

    let (one, msg1) =
        PendingHandshake::initiate(&shared, &b_static, aid, bid, rtc_addr(1, 1)).unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    let first_attempt = b.provisional_attempt(aid).unwrap();
    let one = one.read_msg2(&msg2).unwrap();
    let proof_one = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        one.handshake_hash(),
    );

    let (two, msg1) =
        PendingHandshake::initiate(&shared, &b_static, aid, bid, rtc_addr(2, 1)).unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 2).unwrap();
    let second_attempt = b.provisional_attempt(aid).unwrap();
    assert_ne!(
        first_attempt, second_attempt,
        "the successor is a different attempt, named by its own incarnation"
    );
    let two = two.read_msg2(&msg2).unwrap();

    deliver_proof_frame(
        &mut b,
        aid,
        &one,
        a.origin_hash(),
        &proof_one.encode(),
        net_leaf::clock::now(),
    );
    assert!(
        !b.has_session(aid),
        "the superseded attempt's proof installs nothing"
    );
    assert_eq!(
        b.provisional_attempt(aid),
        Some(second_attempt),
        "and it does not disturb the attempt that replaced it"
    );
    assert!(b.take_verified_admissions().is_empty());

    let proof_two = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        two.handshake_hash(),
    );
    deliver_proof_frame(
        &mut b,
        aid,
        &two,
        a.origin_hash(),
        &proof_two.encode(),
        net_leaf::clock::now(),
    );
    assert!(b.has_session(aid), "the current attempt proves itself");
    assert_eq!(b.take_verified_admissions(), vec![aid]);
}

/// Close retires the attempt, and a genuinely valid proof arriving
/// afterwards installs nothing.
#[test]
fn closing_the_peer_revokes_an_unproven_establishment() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let (pending, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(1, 1),
    )
    .unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    let session = pending.read_msg2(&msg2).unwrap();
    let proof = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        session.handshake_hash(),
    );
    assert!(b.provisional_attempt(aid).is_some());

    b.drop_session(aid, "the page closed the peer");
    b.drain_events();
    assert_eq!(
        b.provisional_attempt(aid),
        None,
        "close deletes the unproven attempt rather than flagging it"
    );

    deliver_proof_frame(
        &mut b,
        aid,
        &session,
        a.origin_hash(),
        &proof.encode(),
        net_leaf::clock::now(),
    );
    assert!(
        !b.has_session(aid),
        "a valid proof for a closed attempt installs nothing"
    );
    assert!(b.take_verified_admissions().is_empty());
}

/// The direct path demands exactly the same proof as the routed one.
///
/// There is one responder entry point, so this is not a second
/// policy: it is the same gate, reached without a routing envelope.
#[test]
fn the_direct_path_demands_the_same_proof_as_the_routed_one() {
    let (mut a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();

    // An impostor, direct: no relay, no routing header, a real
    // DataChannel's worth of bytes.
    let impostor_identity = LeafIdentity::generate().unwrap();
    let (impostor, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(3, 1),
    )
    .unwrap();
    assert_eq!(
        b.classify_datagram(aid, Bytes::copy_from_slice(&msg1)),
        Inbound::Handshake {
            from: aid,
            packet: Bytes::copy_from_slice(&msg1),
            relayed: false,
        },
        "a direct arrival classifies with relayed == false"
    );
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 3).unwrap();
    let impostor = impostor.read_msg2(&msg2).unwrap();
    assert!(!b.has_session(aid), "direct is no weaker than routed");

    let forged = EstablishmentProof::sign(
        impostor_identity.entity(),
        ProofRole::Initiator,
        aid,
        bid,
        impostor.handshake_hash(),
    );
    for packet in impostor
        .build_packets(
            u64::from(SUBPROTOCOL_ESTABLISHMENT_PROOF),
            SUBPROTOCOL_ESTABLISHMENT_PROOF,
            0,
            impostor_identity.origin_hash(),
            false,
            &forged.encode(),
        )
        .unwrap()
    {
        b.on_datagram(aid, packet, net_leaf::clock::now());
    }
    assert_eq!(b.counters().drops(DropReason::EstablishmentUnproven), 1);
    assert!(!b.has_session(aid));

    // Honest A, direct, over the same gate: installed.
    let msg1 = a
        .begin_handshake(bid, &shared, b.identity().noise().public_key(), 4)
        .unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 4).unwrap();
    a.complete_handshake(bid, &msg2).unwrap();
    for out in a.take_outbound() {
        assert_eq!(out.peer, bid, "no relay is involved");
        b.on_datagram(aid, out.packet, net_leaf::clock::now());
    }
    assert!(b.has_session(aid));
    assert_eq!(b.take_verified_admissions(), vec![aid]);
}
/// An attempt's terminal transition retires its unproven
/// establishment explicitly, not merely within the proof deadline.
///
/// The driver reaches a terminal term — ICE timeout, supersession, an
/// explicit reject — and closes that attempt's channel, but a proof
/// can still arrive over a relay inside
/// [`PROOF_DEADLINE_MS`]. `retire_provisional` is what makes "the
/// attempt owns the installation right" true by construction instead
/// of true within five seconds.
#[test]
fn retiring_the_attempt_revokes_an_unproven_establishment() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let (pending, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(1, 1),
    )
    .unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 1).unwrap();
    let session = pending.read_msg2(&msg2).unwrap();
    let proof = EstablishmentProof::sign(
        a.identity().entity(),
        ProofRole::Initiator,
        aid,
        bid,
        session.handshake_hash(),
    );

    assert!(
        b.retire_provisional(aid),
        "there was an unproven establishment to retire"
    );
    assert!(
        !b.retire_provisional(aid),
        "and retiring it twice is not a second retirement"
    );
    assert_eq!(b.provisional_attempt(aid), None);

    deliver_proof_frame(
        &mut b,
        aid,
        &session,
        a.origin_hash(),
        &proof.encode(),
        net_leaf::clock::now(),
    );
    assert!(
        !b.has_session(aid),
        "a valid proof for a retired attempt installs nothing, well inside the deadline"
    );
    assert!(b.take_verified_admissions().is_empty());
}

/// Application bytes sealed under an unproven establishment's keys
/// are never delivered and never acknowledged.
///
/// This is the "nothing application-visible" bound from the other
/// direction: the impostor holds real session keys, so it can seal a
/// real event-plane frame. It reaches no consumer, moves no receive
/// credit, and provokes no answer.
#[test]
fn application_bytes_on_an_unproven_establishment_are_never_delivered() {
    let (a, mut b) = discovered();
    let (aid, bid) = (a.node_id(), b.node_id());
    let shared = psk();
    let (impostor, msg1) = PendingHandshake::initiate(
        &shared,
        b.identity().noise().public_key(),
        aid,
        bid,
        rtc_addr(5, 1),
    )
    .unwrap();
    let msg2 = b.accept_handshake(aid, &shared, &msg1, 5).unwrap();
    let impostor = impostor.read_msg2(&msg2).unwrap();
    b.drain_events();

    for packet in impostor
        .build_packets(
            net_leaf::stream::LEAF_STREAM_DISCRIMINATOR | 3,
            0,
            0,
            a.origin_hash(),
            true,
            b"payload-from-an-unproven-peer",
        )
        .unwrap()
    {
        b.on_datagram(aid, packet, net_leaf::clock::now());
    }

    assert_eq!(
        b.counters().drops(DropReason::EstablishmentUnproven),
        1,
        "the frame is refused under the identity gate, not silently"
    );
    assert!(
        b.drain_events()
            .into_iter()
            .all(|e| !matches!(e, net_leaf::LeafEvent::StreamData { .. })),
        "no application record reaches a consumer"
    );
    assert!(
        b.take_outbound().is_empty(),
        "and nothing is acknowledged or credited back to it"
    );
    assert!(!b.has_session(aid));
}
