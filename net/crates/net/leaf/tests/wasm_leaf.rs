//! The **executed wasm proof** for `net-mesh-leaf`, in headless
//! Chromium.
//!
//! `cargo check --target wasm32-unknown-unknown` cannot see any of
//! the three failures this file exists for:
//!
//! 1. **`cross_lang_wire` replayed inside wasm** — a Stage 5 exit
//!    criterion. Every pinned wire object is re-encoded and
//!    re-decoded here, on the wasm target, with the RustCrypto AEAD
//!    backend instead of `ring`. A backend or a codec that produced
//!    different bytes in a browser than on a server would be a
//!    silent wire-format split between native and browser nodes,
//!    and no `check` job can detect it.
//! 2. **`std::time::Instant::now()` compiles and then panics** on
//!    `wasm32-unknown-unknown` (S0a). The leaf reads the clock on
//!    every call deadline, every reassembly sweep and every signal
//!    envelope, so a missed [`net_leaf::clock`] site is a browser
//!    node that dies on its first packet.
//! 3. **The protocol actually runs.** A full NKpsk0 handshake, a
//!    fragmented publish, a reordered stream and the call table's
//!    dispositions all execute in the browser — the same code the
//!    native suite covers, on the target that matters.
//!
//! What is deliberately NOT here: a *peer*. The RTC transport needs
//! a real other end, so the connection itself is proven by the
//! Playwright matrix against a native anchor. The one `web_sys`
//! exception is the ICE-configuration witness below: what a page
//! declares becoming what the browser actually holds needs no peer
//! at all, and Stage 5 shipped a boundary that dropped every
//! `RTCIceServer` a page passed without one observation noticing.
//!
//! Run:
//! ```text
//! wasm-pack test --headless --chrome net/crates/net/leaf
//! ```
//! or with `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner`
//! and `CHROMEDRIVER` pointing at the Playwright Chromium's driver.

#![cfg(target_arch = "wasm32")]

use bytes::{Bytes, BytesMut};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use net_leaf::announce;
use net_leaf::clock;
use net_leaf::counters::{DropReason, LeafCounters};
use net_leaf::error::RpcError;
use net_leaf::frame::{self, Reassembler};
use net_leaf::identity::{EntityKeypair, LeafIdentity};
use net_leaf::node::LeafEvent;
use net_leaf::rpc_wire::{
    decode_route, encode_request_frame, EventMeta, RpcRequestPayload, DISPATCH_RPC_REQUEST,
};
use net_leaf::rtc::{new_connection, IceServer};
use net_leaf::session::{routing_id, rtc_addr};
use net_leaf::stream::{Reliability, RxStream, StreamRecord};
use net_leaf::test_vectors;
use net_wire::aead::AeadKey;
use net_wire::channel::name::channel_hash;
use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
use net_wire::parsed_packet::ParsedPacket;
use net_wire::pool::PacketBuilder;
use net_wire::protocol::{EventFrame, NackPayload, NetHeader, PacketFlags, NONCE_SIZE};
use net_wire::route_codec::RoutingHeader;
use net_wire::session::NetSession;
use net_wire::stream_window::StreamWindow;

// `run_in_browser`: the point is a real browser engine, not Node.
// `RTCPeerConnection` is not touched here, but the AEAD backend, the
// `web_time` clock and `getrandom`'s `wasm_js` backend are all
// browser-resolved.
wasm_bindgen_test_configure!(run_in_browser);

const ANCHOR: u64 = 0xAAAA_BBBB_CCCC_DDDD;
const PSK: [u8; 32] = [0x4B; 32];

// ───────────────────────── cross_lang replay ─────────────────────────

/// The packet header, re-encoded on wasm32 and matched against the
/// pinned bytes.
#[wasm_bindgen_test]
fn the_net_header_vector_replays_inside_wasm() {
    let f = json(test_vectors::NET_HEADER);
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&unhex(field(&f, "/fields/nonce_hex")));

    let header = NetHeader::new(
        0x0123_4567_89ab_cdef,
        0x1122_3344_5566_7788,
        42,
        nonce,
        64,
        3,
        PacketFlags::RELIABLE,
    );
    assert_eq!(
        hex(&header.to_bytes()),
        field(&f, "/hex"),
        "the header encoding differs on wasm32 — native and browser \
         nodes would not interoperate"
    );
    let decoded = NetHeader::from_bytes(&unhex(field(&f, "/hex"))).expect("decodes");
    assert_eq!(decoded.session_id, 0x0123_4567_89ab_cdef);
    assert_eq!(decoded.nonce, nonce);
    assert!(decoded.flags.is_reliable());
    assert!(decoded.validate());
}

/// The routing envelope — Layer 2, a leaf's only pre-direct path.
#[wasm_bindgen_test]
fn the_routing_header_vector_replays_inside_wasm() {
    let f = json(test_vectors::ROUTING_HEADER);
    let header = RoutingHeader::new(0xAABB_CCDD_EEFF_0011, 0x1234_5678, 16);
    assert_eq!(hex(&header.to_bytes()), field(&f, "/hex"));
    let decoded = RoutingHeader::from_bytes(&unhex(field(&f, "/hex"))).expect("decodes");
    assert_eq!(decoded.dest_id, 0xAABB_CCDD_EEFF_0011);
    assert_eq!(decoded.src_id, 0x1234_5678);
    assert_eq!(decoded.ttl, 16);
}

/// The event framing, which every batched packet's payload uses.
#[wasm_bindgen_test]
fn the_event_frame_vector_replays_inside_wasm() {
    let f = json(test_vectors::EVENT_FRAME);
    let events: Vec<Bytes> = f
        .pointer("/events_utf8")
        .and_then(Value::as_array)
        .expect("events_utf8")
        .iter()
        .map(|e| Bytes::from(e.as_str().expect("utf8 event").to_owned()))
        .collect();

    let mut buf = BytesMut::new();
    let written = EventFrame::write_events(&events, &mut buf);
    assert_eq!(written, buf.len());
    assert_eq!(hex(&buf), field(&f, "/hex"));
    assert_eq!(
        EventFrame::read_events(
            Bytes::from(unhex(field(&f, "/hex"))),
            u16::try_from(events.len()).expect("count fits")
        ),
        events
    );
}

/// The reliable-stream NACK and the `0x0B00` credit grant — the two
/// control payloads the leaf's dispatcher decodes.
#[wasm_bindgen_test]
fn the_stream_control_vectors_replay_inside_wasm() {
    let f = json(test_vectors::NACK_PAYLOAD);
    let nack = NackPayload {
        next_expected: 0x0102_0304_0506_0708,
        missing_bitmap: 0b1011,
    };
    assert_eq!(hex(&nack.to_bytes()), field(&f, "/hex"));
    let decoded = NackPayload::from_bytes(&unhex(field(&f, "/hex"))).expect("decodes");
    assert_eq!(decoded.next_expected, nack.next_expected);
    assert_eq!(decoded.missing_bitmap, nack.missing_bitmap);

    let f = json(test_vectors::STREAM_WINDOW);
    let grant = StreamWindow {
        stream_id: 0x00FF_00FF_00FF_00FF,
        total_consumed: 123_456,
        ack_seq: 99,
    };
    assert_eq!(hex(&grant.encode()), field(&f, "/hex"));
    assert_eq!(
        StreamWindow::decode(&unhex(field(&f, "/hex"))).expect("decodes"),
        grant
    );
}

/// The packet AEAD on the RustCrypto backend, against the vector the
/// core asserts on `ring`. A divergence is a mesh whose nodes cannot
/// talk depending on which target they were built for.
#[wasm_bindgen_test]
fn the_aead_vector_replays_on_the_wasm_backend() {
    let f = json(net_wire::test_vectors::AEAD_VECTOR);
    let key: [u8; 32] = unhex(field(&f, "/key_hex")).try_into().expect("key");
    let nonce: [u8; 12] = unhex(field(&f, "/nonce_hex")).try_into().expect("nonce");
    let aad = unhex(field(&f, "/aad_hex"));
    let plaintext = field(&f, "/plaintext_utf8");
    let expected = field(&f, "/ciphertext_and_tag_hex");

    let aead = AeadKey::new(&key);
    let mut sealed = plaintext.as_bytes().to_vec();
    aead.seal_append_tag(nonce, &aad, &mut sealed)
        .expect("seal on wasm");
    assert_eq!(hex(&sealed), expected);

    let mut opened = unhex(expected);
    let len = aead
        .open_in_place(nonce, &aad, &mut opened)
        .expect("open the pinned ciphertext");
    assert_eq!(&opened[..len], plaintext.as_bytes());
}

/// The leaf's own announcement encoder and its signature, inside
/// wasm — where both the Ed25519 implementation and the JSON writer
/// are the browser's.
#[wasm_bindgen_test]
fn the_leaf_announcement_vector_replays_inside_wasm() {
    let f = json(test_vectors::CAPABILITY_ANNOUNCEMENT_LEAF);
    let pinned = field(&f, "/bytes_utf8");
    let identity = LeafIdentity::from_secrets(EntityKeypair::from_secret([0x21; 32]), [0x22; 32]);
    assert_eq!(
        identity.node_id().to_string(),
        field(&f, "/identity/node_id"),
        "the node-id derivation must be identical on wasm32"
    );

    let produced = announce::build_announcement(
        &identity,
        &["stage5.browser".to_string()],
        7,
        1_700_000_000_000_000_000,
        300,
    )
    .expect("build");
    assert_eq!(
        String::from_utf8(produced).expect("UTF-8"),
        pinned,
        "the announcement bytes differ on wasm32"
    );

    let verified = announce::verify_announcement(pinned.as_bytes()).expect("verifies on wasm");
    assert_eq!(verified.node_id, identity.node_id());
    assert!(verified.capabilities.contains(&"leaf".to_string()));
    assert!(verified.capabilities.contains(&"transport:rtc".to_string()));

    // And the pre-Stage-4 / Stage-4a announcements still verify as
    // *documents* the leaf's codec-free verifier can transcribe.
    for vector in [
        test_vectors::CAPABILITY_ANNOUNCEMENT,
        test_vectors::CAPABILITY_ANNOUNCEMENT_RTC,
    ] {
        let document = json(vector);
        let bytes = field(&document, "/bytes_utf8");
        let parsed: Value = serde_json::from_str(bytes).expect("parses");
        let transcript = announce::signed_transcript(&parsed).expect("transcript");
        assert_eq!(
            String::from_utf8(transcript).expect("UTF-8"),
            bytes,
            "these fixtures carry no signature and no hop_count, so the \
             canonical transcript is the document itself — if this differs, \
             the verifier's transcript is not the core's canonical form"
        );
    }
}

/// The leaf's nRPC client codec, inside wasm.
#[wasm_bindgen_test]
fn the_nrpc_frame_vector_replays_inside_wasm() {
    let f = json(test_vectors::NRPC_FRAME);
    let pinned = field(&f, "/hex");
    let route: u64 = field(&f, "/fields/route_canonical_hash")
        .parse()
        .expect("route");
    assert_eq!(
        route,
        channel_hash(field(&f, "/fields/route_channel")),
        "the canonical channel hash must be identical on wasm32"
    );

    let mut payload = RpcRequestPayload::unary(
        "net.mesh.enroll",
        1_700_000_000_000_000_000,
        Bytes::from_static(b"join"),
    );
    payload.headers = vec![(
        "content-type".to_string(),
        b"application/octet-stream".to_vec(),
    )];
    let frame = encode_request_frame(
        field(&f, "/fields/origin_hash").parse().expect("origin"),
        field(&f, "/fields/call_id").parse().expect("call id"),
        route,
        &payload,
    )
    .expect("encode");
    assert_eq!(hex(&frame), pinned, "the nRPC frame differs on wasm32");

    let bytes = unhex(pinned);
    let meta = EventMeta::from_bytes(&bytes).expect("meta");
    assert_eq!(meta.dispatch, DISPATCH_RPC_REQUEST);
    assert_eq!(decode_route(&bytes), Some(route));
}

/// The enrollment request, signed and pinned, inside wasm.
///
/// This is the exchange that promotes a session out of PROVISIONAL,
/// so it is the difference between a browser node that can call a
/// service and one whose every call dies on its deadline. The
/// signature is produced by the browser's own Ed25519 code path
/// here; a byte of drift and the anchor's provider — which
/// reconstructs the challenge itself — refuses.
#[wasm_bindgen_test]
fn the_enrollment_request_replays_inside_wasm() {
    use net_leaf::enroll::{build_join_request, join_challenge, Invite, JoinOutcome};
    use net_leaf::identity::verify_entity_signature;

    let f = json(test_vectors::ENROLL_EXCHANGE);
    let invite = Invite::decode(&unhex(field(&f, "/invite/invite_hex"))).expect("invite decodes");
    let identity = LeafIdentity::from_secrets(EntityKeypair::from_secret([0x21; 32]), [0x22; 32]);
    let tags = vec!["browser".to_string(), "leaf".to_string()];

    let body = build_join_request(&identity, "chrome-tab", &tags, &invite).expect("build");
    assert_eq!(
        hex(&body),
        field(&f, "/join_request/join_request_hex"),
        "the enrollment request differs on wasm32 — the anchor would refuse it \
         and every session would stay provisional"
    );

    // magic(4) device(32) nonce(16) root(32) signature(64)
    let device: [u8; 32] = body[4..36].try_into().expect("device");
    let signature: [u8; 64] = body[84..148].try_into().expect("signature");
    let challenge = join_challenge(&device, "chrome-tab", &tags, &invite.nonce, &invite.root);
    verify_entity_signature(&device, &challenge, &signature)
        .expect("the browser's own signature must verify against the challenge");

    // Both outcome shapes, decoded by the browser.
    let admitted = JoinOutcome::decode(&unhex(field(&f, "/join_outcome/admitted_hex")))
        .expect("admitted decodes");
    assert_eq!(
        admitted.into_chain().expect("tag 0 promotes"),
        field(&f, "/join_outcome/admitted_chain_utf8").as_bytes()
    );
    let rejected = JoinOutcome::decode(&unhex(field(&f, "/join_outcome/rejected_hex")))
        .expect("rejected decodes");
    assert!(
        rejected.into_chain().is_err(),
        "tag 1 must leave the session provisional"
    );
}

// ────────────────────────── the clock seam ──────────────────────────

/// Every clock read the leaf makes, on the target where
/// `std::time::Instant::now()` panics.
#[wasm_bindgen_test]
fn every_clock_read_the_leaf_makes_works_in_a_browser() {
    let first = clock::now();
    let unix_secs = clock::now_unix_secs();
    let unix_nanos = clock::now_unix_nanos();
    assert!(
        unix_secs > 1_600_000_000,
        "the wall clock must be a real epoch reading, got {unix_secs}"
    );
    assert!(unix_nanos / 1_000_000_000 >= unix_secs - 1);

    let second = clock::now();
    assert!(second.duration_since(first).as_nanos() < u128::MAX);

    // The deadline arithmetic the call table and the reassembler use.
    let deadline = clock::Deadline::in_ms(50);
    assert!(!deadline.expired_at(clock::now()));
    assert!(deadline.expired_at(clock::now() + core::time::Duration::from_millis(100)));
}

// ───────────────────── the protocol, in a browser ────────────────────

fn anchor_static() -> StaticKeypair {
    let secret = x25519_dalek::StaticSecret::from([7u8; 32]);
    let public = x25519_dalek::PublicKey::from(&secret);
    StaticKeypair::from_keys([7u8; 32], *public.as_bytes())
}

/// A node with a live session against a real responder, built inside
/// the browser: the handshake, the key schedule and the session
/// install all run on wasm32.
fn connected() -> (net_leaf::node::LeafNode, NetSession) {
    let identity = LeafIdentity::generate().expect("the browser CSPRNG must work");
    let mut node = net_leaf::node::LeafNode::new(identity, 0x5000);
    let anchor_key = anchor_static();
    let prologue = handshake_prologue(routing_id(node.node_id()), routing_id(ANCHOR));
    let mut responder =
        NoiseHandshake::responder_with_prologue(&PSK, &anchor_key, &prologue).expect("responder");

    let msg1 = node
        .begin_handshake(ANCHOR, &PSK, anchor_key.public_key(), 0)
        .expect("msg1");
    let parsed = ParsedPacket::parse(msg1, rtc_addr(0, 1)).expect("msg1 parses");
    responder.read_message(&parsed.payload).expect("reads msg1");
    let msg2 = responder.write_message(&[]).expect("msg2");
    let msg2_packet = PacketBuilder::new(&[0u8; 32], 0).build_handshake(&msg2);

    node.complete_handshake(ANCHOR, &msg2_packet)
        .expect("install");
    let keys = responder.into_session_keys().expect("keys");
    (node, NetSession::new(keys, rtc_addr(1, 1), 2, false))
}

/// The S0a minimum proof, at the leaf's own level: handshake →
/// publish → decrypt on the other side, in a browser.
#[wasm_bindgen_test]
fn a_handshake_and_a_publish_round_trip_inside_wasm() {
    let (mut node, anchor) = connected();
    assert!(node.has_session(ANCHOR));
    let events = node.drain_events();
    assert!(
        matches!(events.first(), Some(LeafEvent::Connected { .. })),
        "the handshake must surface a Connected event, got {events:?}"
    );

    node.publish(ANCHOR, "sensors/lidar", b"browser payload")
        .expect("publish");
    let out = node.take_outbound();
    assert_eq!(out.len(), 1, "one Batch per packet");

    let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
    let aad = parsed.header.aad();
    let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
    let plain = anchor
        .rx_cipher()
        .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
        .expect("the RustCrypto backend decrypts what the browser sealed");
    let frames = EventFrame::read_events(plain, parsed.header.event_count);
    assert_eq!(&frames[0][..], b"browser payload");
}

/// S0c's decision, executed: an over-cap payload fragments and
/// reassembles inside wasm, and no packet fails `validate()`.
#[wasm_bindgen_test]
fn fragmentation_and_reassembly_run_inside_wasm() {
    let (mut node, anchor) = connected();
    node.drain_events();
    let big: Vec<u8> = (0..frame::MAX_FRAGMENT_PAYLOAD * 2 + 7)
        .map(|i| (i % 251) as u8)
        .collect();
    node.publish(ANCHOR, "bulk/frames", &big).expect("publish");

    let counters = LeafCounters::new();
    let mut reassembler = Reassembler::new();
    let mut whole = None;
    for out in node.take_outbound() {
        let parsed = ParsedPacket::parse(out.packet, rtc_addr(0, 1)).expect("parses");
        assert!(
            parsed.header.validate(),
            "a fragment must pass validate() — the S0c black hole is exactly \
             the packet that does not"
        );
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        let plain = anchor
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("decrypts");
        let events = EventFrame::read_events(plain, parsed.header.event_count);
        whole = reassembler.accept(
            ANCHOR,
            parsed.header.fragment_id,
            parsed.header.fragment_offset,
            parsed.header.frag_flags,
            events[0].clone(),
            clock::now(),
            &counters,
        );
    }
    assert_eq!(whole.expect("the group completes").as_ref(), &big[..]);
    assert_eq!(counters.total_drops(), 0);
}

/// The consumer-side reorder and the fire-and-forget drop, inside
/// wasm — including the property the record rewrite added: a packet
/// released when a later arrival fills the gap keeps ITS OWN
/// sequence and header, not the gap-filling arrival's.
#[wasm_bindgen_test]
fn the_consumer_side_reorder_runs_inside_wasm() {
    fn record(seq: u64, tag: u8) -> StreamRecord {
        StreamRecord {
            seq,
            span: 1,
            stream_id: 7,
            origin_hash: 0xA0 + seq,
            channel_hash: tag as u16,
            payloads: vec![Bytes::from(vec![tag])],
        }
    }

    let counters = LeafCounters::new();
    let mut reliable = RxStream::new(Reliability::Reliable);
    assert!(reliable.accept(record(2, b'c'), &counters).is_empty());
    assert!(reliable.accept(record(1, b'b'), &counters).is_empty());
    let released = reliable.accept(record(0, b'a'), &counters);
    let order: Vec<(u64, u8, u64)> = released
        .iter()
        .map(|r| (r.seq, r.payloads[0][0], r.origin_hash))
        .collect();
    assert_eq!(
        order,
        vec![(0, b'a', 0xA0), (1, b'b', 0xA1), (2, b'c', 0xA2)],
        "reliable delivers in order, each with its own sequence and origin"
    );

    let mut lossy = RxStream::new(Reliability::FireAndForget);
    lossy.accept(record(0, b'a'), &counters);
    let out = lossy.accept(record(4, b'e'), &counters);
    assert_eq!(out.len(), 1, "fire-and-forget must not stall on a gap");
    assert_eq!(counters.drops(DropReason::FireAndForgetGap), 3);
}

/// The call table's dispositions, inside wasm: a timeout on the
/// swept deadline and a typed session loss, neither retried.
#[wasm_bindgen_test]
fn the_call_table_dispositions_run_inside_wasm() {
    let (mut node, _anchor) = connected();
    node.drain_events();

    let mut timing_out = node
        .call(ANCHOR, "net.mesh.enroll", b"join", Some(10))
        .expect("call");
    node.take_outbound();
    let later = clock::now() + core::time::Duration::from_millis(50);
    assert_eq!(node.tick(later), 1, "the deadline sweep must fire");
    assert_eq!(
        timing_out.try_recv().expect("alive").expect("resolved"),
        Err(RpcError::Timeout)
    );
    assert_eq!(
        node.take_outbound().len(),
        1,
        "a timed-out call must CANCEL, so the server stops working"
    );

    let mut lost = node
        .call(ANCHOR, "net.mesh.enroll", b"join", Some(60_000))
        .expect("call");
    node.take_outbound();
    node.drop_session(ANCHOR, "the DataChannel closed");
    assert_eq!(
        lost.try_recv().expect("alive").expect("resolved"),
        Err(RpcError::SessionLost),
        "a lost session fails typed — never a silent retry"
    );
    assert!(node.take_outbound().is_empty());
}

/// The browser CSPRNG: `getrandom`'s `wasm_js` backend, which S0a
/// found needs its own opt-in per major. An identity generated from
/// a broken backend would be all zeros.
#[wasm_bindgen_test]
fn identity_generation_uses_the_browser_csprng() {
    let a = LeafIdentity::generate().expect("generate");
    let b = LeafIdentity::generate().expect("generate");
    assert_ne!(
        a.node_id(),
        b.node_id(),
        "two generated identities must differ — a stubbed CSPRNG would \
         mint the same node id for every tab on the origin"
    );
    assert_ne!(a.entity().entity_id(), &[0u8; 32]);
    assert_ne!(a.noise().public_key(), &[0u8; 32]);
}

/// **The effective ICE configuration.** What a page declares is what
/// the browser ends up holding — read back off a real
/// `RTCPeerConnection`, not off the argument we handed it.
///
/// Stage 5 read `iceServers` with `as_string` on each element, so
/// the only shape `RTCIceServer` has — an object — evaluated to
/// nothing: every STUN and TURN server a page configured was
/// silently absent from the offer, and a recorded-options assertion
/// could not see it because the options were recorded before the
/// drop. This asserts the far side of the translation instead: the
/// URLs arrive, both of a multi-URL entry arrive, and a TURN entry
/// keeps the credentials without which the browser would not use it.
#[wasm_bindgen_test]
fn declared_ice_servers_are_the_connection_s_effective_configuration() {
    let declared = [
        IceServer {
            urls: vec!["stun:stun.example:3478".to_string()],
            username: None,
            credential: None,
        },
        IceServer {
            urls: vec![
                "turn:turn.example:3478".to_string(),
                "turn:turn.example:3479".to_string(),
            ],
            username: Some("leaf".to_string()),
            credential: Some("secret".to_string()),
        },
    ];

    let connection = new_connection(&declared).expect("a peer connection");
    let effective = js_sys::Reflect::get(
        &connection.get_configuration(),
        &JsValue::from_str("iceServers"),
    )
    .expect("getConfiguration() reports iceServers")
    .dyn_into::<js_sys::Array>()
    .expect("iceServers is an array");
    connection.close();

    assert_eq!(
        effective.length(),
        2,
        "the browser holds {} of the 2 declared ICE servers",
        effective.length()
    );

    let stun = effective.get(0);
    assert_eq!(
        configured_urls(&stun),
        vec!["stun:stun.example:3478".to_string()],
        "the STUN server a page declared did not reach the connection"
    );

    let turn = effective.get(1);
    assert_eq!(
        configured_urls(&turn),
        vec![
            "turn:turn.example:3478".to_string(),
            "turn:turn.example:3479".to_string()
        ],
        "a multi-URL RTCIceServer lost one of its URLs"
    );
    assert_eq!(
        string_field(&turn, "username"),
        Some("leaf".to_string()),
        "the TURN username did not survive the boundary"
    );
    assert_eq!(
        string_field(&turn, "credential"),
        Some("secret".to_string()),
        "the TURN credential did not survive the boundary — the server \
         is configured but unusable"
    );
}

// ──────────────────────────── helpers ───────────────────────────────

fn json(src: &str) -> Value {
    serde_json::from_str(src).expect("fixture parses")
}

fn field<'a>(document: &'a Value, pointer: &str) -> &'a str {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture has no {pointer}"))
}

/// One configured server's `urls`, whichever of the two shapes the
/// engine normalised it to.
fn configured_urls(server: &JsValue) -> Vec<String> {
    let urls = js_sys::Reflect::get(server, &JsValue::from_str("urls")).expect("urls");
    if let Some(single) = urls.as_string() {
        return vec![single];
    }
    js_sys::Array::from(&urls)
        .iter()
        .map(|url| url.as_string().expect("a url string"))
        .collect()
}

fn string_field(object: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(object, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}
