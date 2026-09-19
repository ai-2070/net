//! Wire-format golden vectors — Rust reference test.
//!
//! Loads `tests/cross_lang_wire/*.json` and asserts the implementation
//! both **emits** and **accepts** exactly what the fixtures pin. The
//! surface is the one `net-mesh-wire` owns since Stage 2 of
//! `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md`: the
//! packet header, the routing envelope, the event framing, the NACK
//! and stream-window payloads, the packet AEAD, and the capability
//! announcement in its current form.
//!
//! Two properties are under test that a round-trip test alone would
//! miss:
//!
//! 1. The *bytes* are pinned, not just self-consistency — extracting
//!    the modules into another crate must not move a field or flip an
//!    endianness, and a round-trip is blind to both.
//! 2. Everything is reached through the paths the core re-exports
//!    (`net::adapter::net::…`), so a re-export that silently changed
//!    shape fails here rather than in a downstream crate.
//!
//! The AEAD vector is replayed on the `chacha20poly1305` backend by
//! the wasm test in `wire/tests/wasm_wire.rs`; this file runs the
//! `ring` side of the same vector.
#![cfg(feature = "net")]

use bytes::{Bytes, BytesMut};
use net::adapter::net::behavior::capability::CapabilityAnnouncement;
use net::adapter::net::subprotocol::stream_window::StreamWindow;
use net::adapter::net::{
    EventFrame, NackPayload, NetHeader, PacketFlags, RoutingHeader, NONCE_SIZE,
};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("tests/cross_lang_wire/{name}");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd-length hex in fixture");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
        .collect()
}

fn expected_hex(v: &Value) -> String {
    v["hex"].as_str().expect("fixture `hex`").to_string()
}

#[test]
fn net_header_matches_the_fixture_bytes() {
    let f = fixture("net_header.json");
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&unhex(
        f["fields"]["nonce_hex"].as_str().expect("nonce_hex"),
    ));

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
        expected_hex(&f),
        "NetHeader encoding drifted from the pinned bytes"
    );

    // …and the decoder accepts exactly those bytes back.
    let decoded = NetHeader::from_bytes(&unhex(&expected_hex(&f))).expect("fixture must decode");
    assert_eq!(decoded.session_id, 0x0123_4567_89ab_cdef);
    assert_eq!(decoded.stream_id, 0x1122_3344_5566_7788);
    assert_eq!(decoded.sequence, 42);
    assert_eq!(decoded.payload_len, 64);
    assert_eq!(decoded.event_count, 3);
    assert_eq!(decoded.nonce, nonce);
    assert!(decoded.flags.is_reliable());
    assert!(decoded.validate(), "fixture header must pass validation");
}

#[test]
fn routing_header_matches_the_fixture_bytes() {
    let f = fixture("routing_header.json");
    let header = RoutingHeader::new(0xAABB_CCDD_EEFF_0011, 0x1234_5678, 16);
    assert_eq!(
        hex(&header.to_bytes()),
        expected_hex(&f),
        "RoutingHeader encoding drifted — Layer 2 is the only pre-direct \
         path a browser leaf has, so this envelope is load-bearing for it"
    );

    let decoded = RoutingHeader::from_bytes(&unhex(&expected_hex(&f))).expect("fixture decodes");
    assert_eq!(decoded.dest_id, 0xAABB_CCDD_EEFF_0011);
    assert_eq!(decoded.src_id, 0x1234_5678);
    assert_eq!(decoded.ttl, 16);
    assert_eq!(decoded.hop_count, 0);
}

#[test]
fn event_frame_matches_the_fixture_bytes() {
    let f = fixture("event_frame.json");
    let events: Vec<Bytes> = f["events_utf8"]
        .as_array()
        .expect("events_utf8")
        .iter()
        .map(|e| Bytes::from(e.as_str().expect("utf8 event").to_owned()))
        .collect();

    let mut buf = BytesMut::new();
    let written = EventFrame::write_events(&events, &mut buf);
    assert_eq!(written, buf.len());
    assert_eq!(hex(&buf), expected_hex(&f), "event framing drifted");

    let read = EventFrame::read_events(
        Bytes::from(unhex(&expected_hex(&f))),
        u16::try_from(events.len()).expect("count fits"),
    );
    assert_eq!(read, events, "framing must round-trip the fixture bytes");
}

#[test]
fn nack_payload_matches_the_fixture_bytes() {
    let f = fixture("nack_payload.json");
    let nack = NackPayload {
        next_expected: 0x0102_0304_0506_0708,
        missing_bitmap: 0b1011,
    };
    assert_eq!(hex(&nack.to_bytes()), expected_hex(&f));

    let decoded = NackPayload::from_bytes(&unhex(&expected_hex(&f))).expect("fixture decodes");
    assert_eq!(decoded.next_expected, nack.next_expected);
    assert_eq!(decoded.missing_bitmap, nack.missing_bitmap);
}

#[test]
fn stream_window_matches_the_fixture_bytes() {
    let f = fixture("stream_window.json");
    let grant = StreamWindow {
        stream_id: 0x00FF_00FF_00FF_00FF,
        total_consumed: 123_456,
        ack_seq: 99,
    };
    assert_eq!(hex(&grant.encode()), expected_hex(&f));

    let decoded = StreamWindow::decode(&unhex(&expected_hex(&f))).expect("fixture decodes");
    assert_eq!(decoded, grant);
}

/// The AEAD backend seam must not change the wire format.
///
/// Native builds seal with `ring`, `wasm32` builds with the RustCrypto
/// `chacha20poly1305` crate. Both are RFC 8439, so for one
/// (key, nonce, aad, plaintext) the ciphertext and tag are identical —
/// this test proves it for the `ring` side, and the wasm test replays
/// the same fixture on the other. A mismatch means a mesh whose nodes
/// were built for different targets cannot talk.
#[test]
fn aead_golden_vector_matches_on_the_native_backend() {
    // The wire crate owns the canonical copy (it has to: the wasm
    // test cannot `include_str!` across a package boundary). The
    // repository copy under `tests/cross_lang_wire/` is the mirror
    // the other-language consumers this fixture set is shaped for
    // would read, and is asserted byte-equal below.
    let f: Value = serde_json::from_str(net_wire::test_vectors::AEAD_VECTOR)
        .expect("the wire crate's AEAD vector must parse");
    let key: [u8; 32] = unhex(f["key_hex"].as_str().expect("key_hex"))
        .try_into()
        .expect("32-byte key");
    let nonce: [u8; 12] = unhex(f["nonce_hex"].as_str().expect("nonce_hex"))
        .try_into()
        .expect("12-byte nonce");
    let aad = unhex(f["aad_hex"].as_str().expect("aad_hex"));
    let plaintext = f["plaintext_utf8"].as_str().expect("plaintext_utf8");
    let expected = f["ciphertext_and_tag_hex"]
        .as_str()
        .expect("ciphertext_and_tag_hex");

    let aead = net_wire::aead::AeadKey::new(&key);

    let mut sealed = plaintext.as_bytes().to_vec();
    aead.seal_append_tag(nonce, &aad, &mut sealed)
        .expect("seal must succeed");
    assert_eq!(hex(&sealed), expected, "AEAD ciphertext||tag drifted");

    let mut opened = unhex(expected);
    let len = aead
        .open_in_place(nonce, &aad, &mut opened)
        .expect("the fixture ciphertext must open");
    assert_eq!(&opened[..len], plaintext.as_bytes());
}

/// The announcement's **current** form. The three optional
/// browser-leaf fields are Stage 4's; this fixture exists so that
/// adding them is a visible, deliberate change to a pinned byte
/// string rather than an invisible one.
#[test]
fn capability_announcement_matches_the_fixture_bytes() {
    let f = fixture("capability_announcement.json");
    let bytes = f["bytes_utf8"].as_str().expect("bytes_utf8");

    let decoded = CapabilityAnnouncement::from_bytes(bytes.as_bytes())
        .expect("the pinned announcement must decode");
    assert_eq!(decoded.node_id, 0xA1B2_C3D4_E5F6_0708);
    assert_eq!(decoded.version, 7);
    assert_eq!(decoded.timestamp_ns, 1_700_000_000_000_000_000);
    assert_eq!(decoded.ttl_secs, 300);
    assert!(decoded.signature.is_none());
    assert_eq!(decoded.hop_count, 0);
    assert!(decoded.reflex_addr.is_none());

    let re_encoded = String::from_utf8(decoded.to_bytes()).expect("announcement is UTF-8 JSON");
    assert_eq!(
        re_encoded, bytes,
        "announcement encoding drifted from the pinned JSON — if a field \
         was added, Stage 4 owns that change and this fixture moves with it"
    );

    // The `json` object in the fixture is the same document, so a
    // reader that consumes the fixture structurally sees what the
    // byte string says.
    assert_eq!(
        serde_json::from_str::<Value>(bytes).expect("bytes_utf8 parses"),
        f["json"],
        "fixture's `json` and `bytes_utf8` disagree"
    );
}

/// Stage 4a + Stage 6: the same announcement **with** the optional
/// browser-leaf fields. Pinned so a cross-language reader sees the
/// exact key names, order and encodings — `noise_pubkey` as a
/// 32-element byte array, `rtc_addr` as a `host:port` string, and
/// `rtc_stun_addr` as a `host:port` string naming a *different*
/// endpoint from `rtc_addr` (that distinctness is the whole point
/// of announcing it, so the fixture shows it).
#[test]
fn the_rtc_announcement_fixture_round_trips_and_keeps_field_order() {
    let f = fixture("capability_announcement_rtc.json");
    let bytes = f["bytes_utf8"].as_str().expect("bytes_utf8");

    let decoded =
        CapabilityAnnouncement::from_bytes(bytes.as_bytes()).expect("pinned RTC form decodes");
    assert_eq!(decoded.noise_pubkey, Some([0x11u8; 32]));
    assert_eq!(
        decoded.rtc_bootstrap.as_deref(),
        Some("https://anchor.example/rtc")
    );
    assert_eq!(
        decoded.rtc_addr,
        Some("198.51.100.7:4433".parse().expect("addr"))
    );
    assert_eq!(decoded.rtc_stun_addr.as_deref(), Some("198.51.100.7:3478"));
    assert_ne!(
        decoded.rtc_stun_addr.as_deref(),
        decoded.rtc_addr.map(|a| a.to_string()).as_deref(),
        "the announced STUN endpoint must be a different endpoint from rtc_addr"
    );

    let re_encoded = String::from_utf8(decoded.to_bytes()).expect("UTF-8 JSON");
    assert_eq!(
        re_encoded, bytes,
        "the RTC announcement encoding drifted from the pinned JSON"
    );
}

/// …and the same announcement with all of them **absent** produces
/// exactly the pre-Stage-4 pinned bytes. This is the wire-compat
/// claim: a node that does not configure RTC is invisible to Stage
/// 4a and Stage 6, signature included.
#[test]
fn dropping_the_rtc_fields_reproduces_the_pre_stage4_announcement_bytes() {
    let with_rtc = fixture("capability_announcement_rtc.json");
    let mut ann = CapabilityAnnouncement::from_bytes(
        with_rtc["bytes_utf8"]
            .as_str()
            .expect("bytes_utf8")
            .as_bytes(),
    )
    .expect("decodes");
    ann.noise_pubkey = None;
    ann.rtc_bootstrap = None;
    ann.rtc_addr = None;
    ann.rtc_stun_addr = None;

    let plain = fixture("capability_announcement.json");
    let expected = plain["bytes_utf8"].as_str().expect("bytes_utf8");
    assert_eq!(
        String::from_utf8(ann.to_bytes()).expect("UTF-8"),
        expected,
        "with every optional RTC field cleared the encoding must be byte-identical \
         to the pre-Stage-4 fixture"
    );
    // …and the signed transcript follows, because the canonical
    // signer emits exactly this document with `signature` and
    // `hop_count` omitted: sign the cleared announcement and verify
    // it through the same path a pre-Stage-4 peer would.
    let keypair = net::adapter::net::EntityKeypair::generate();
    let mut signed = ann.clone();
    signed.entity_id = keypair.entity_id().clone();
    signed.sign(&keypair);
    signed.verify().expect("the cleared form verifies");
    let mut with_field_back = signed.clone();
    with_field_back.noise_pubkey = Some([0x11; 32]);
    assert!(
        with_field_back.verify().is_err(),
        "re-adding a Stage 4 field after signing must break verification — \
         proof the field is inside the transcript, not beside it"
    );
    let mut with_stun_back = signed.clone();
    with_stun_back.rtc_stun_addr = Some("198.51.100.7:3478".to_string());
    assert!(
        with_stun_back.verify().is_err(),
        "re-adding the announced STUN endpoint after signing must break \
         verification — proof Stage 6's field is inside the transcript too"
    );
}

/// The repository mirror and the package-owned constant are the same
/// bytes.
///
/// Repository-only by nature — it reads a checkout path. Its job is
/// to stop the two copies from drifting: the wasm test and the
/// vector test above both assert against the constant, while the
/// JSON under `tests/cross_lang_wire/` is what a Go / TypeScript /
/// Python consumer would read.
#[test]
fn the_repository_aead_fixture_mirrors_the_package_constant() {
    let on_disk = std::fs::read_to_string("tests/cross_lang_wire/aead_vector.json")
        .expect("repository copy must exist in a checkout");
    assert_eq!(
        on_disk,
        net_wire::test_vectors::AEAD_VECTOR,
        "tests/cross_lang_wire/aead_vector.json has drifted from \
         net_wire::test_vectors::AEAD_VECTOR — the wire crate owns the \
         canonical copy; regenerate the mirror from it"
    );
}

// ===================================================================
// Stage 5 — the OTHER direction of the leaf's two second copies.
//
// `net-mesh-leaf` compiles to wasm32 and therefore cannot link this
// crate, so Stage 5 carries a deliberate second WRITER for two
// documents: the capability announcement and the nRPC client frame.
// The announcement's reasoning is in the leaf's `announce` module
// (its verifier is codec-free, so no field of a native announcement
// can be dropped or reordered by the leaf); the nRPC codec's is in
// the leaf's `rpc_wire` module (the production codec is not a
// severable head of `cortex/rpc.rs`).
//
// The tests below are the pin from THIS side: the leaf-generated
// fixtures must decode through the production decoders, re-encode
// byte-identically, and verify. A one-directional pin is exactly the
// gap that makes a second copy dangerous — a drift on either side
// now reddens the other side's suite.
// ===================================================================

/// The leaf's announcement decodes, re-encodes byte-identically and
/// verifies through the production `CapabilityAnnouncement`.
#[test]
fn the_leaf_generated_announcement_round_trips_through_the_production_codec() {
    let f = fixture("capability_announcement_leaf.json");
    let bytes = f["bytes_utf8"].as_str().expect("bytes_utf8");

    let decoded = CapabilityAnnouncement::from_bytes(bytes.as_bytes())
        .expect("a leaf's announcement must decode with the production decoder");

    // The leaf derives its node id and origin hash from its entity
    // key; the fixture names both, and the production `EntityId`
    // derivation must agree — otherwise the leaf would announce one
    // identity and publish packets under another.
    let expected_node_id: u64 = f["identity"]["node_id"]
        .as_str()
        .expect("identity.node_id")
        .parse()
        .expect("node_id is a u64");
    assert_eq!(decoded.node_id, expected_node_id);
    assert_eq!(
        decoded.entity_id.node_id(),
        expected_node_id,
        "the leaf's node id must be the core's keyed-BLAKE2s derivation \
         over the same entity key"
    );

    // §7's role tags, and the fields a leaf must NOT set.
    let tags: Vec<String> = decoded
        .capabilities
        .tags
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    for required in ["leaf", "transport:rtc"] {
        assert!(
            tags.iter().any(|t| t == required),
            "a leaf's announcement must carry the {required:?} tag, got {tags:?}"
        );
    }
    // The negotiation tag, checked against the CORE's constant rather
    // than a repeated literal: this is the string a native sender
    // matches on before it fragments a stream payload above
    // `MAX_EVENT_SIZE`, so a leaf spelling it differently from the
    // core does not produce a refusal — it produces a silent fallback
    // to the 8 104-byte cap that nothing else here would notice.
    assert!(
        tags.iter()
            .any(|t| t == net::adapter::net::behavior::capability::FRAGMENT_REASSEMBLY_TAG),
        "the pinned leaf announcement must carry the core's \
         FRAGMENT_REASSEMBLY_TAG ({:?}), got {tags:?}",
        net::adapter::net::behavior::capability::FRAGMENT_REASSEMBLY_TAG
    );
    assert!(
        decoded.reflex_addr.is_none(),
        "§7: a leaf omits reflex_addr — it has no observer-visible socket"
    );
    assert!(
        decoded.rtc_addr.is_none(),
        "a browser has no server-reflexive socket to advertise"
    );
    assert!(
        decoded.noise_pubkey.is_some(),
        "§5 Layer 1: the Noise static is what makes first contact possible"
    );
    assert_eq!(decoded.hop_count, 0, "a leaf originates at hop 0");

    // The signature the LEAF produced must verify under the CORE's
    // canonical transcript. This is the load-bearing assertion: it
    // proves the leaf's hand-written writer and the core's
    // `SignedPayloadCanonical` agree byte for byte.
    decoded
        .verify()
        .expect("the leaf's signature must verify against the core's canonical transcript");

    let re_encoded = String::from_utf8(decoded.to_bytes()).expect("announcement is UTF-8 JSON");
    assert_eq!(
        re_encoded, bytes,
        "the production encoder produced different bytes than the leaf's — \
         the two writers have drifted; net-mesh-leaf's `announce` module and \
         this fixture move together or not at all"
    );
}

/// A tampered leaf announcement must fail the production verifier,
/// so the round trip above is not passing on a signature nobody
/// checks.
#[test]
fn a_tampered_leaf_announcement_fails_the_production_verifier() {
    let f = fixture("capability_announcement_leaf.json");
    let bytes = f["bytes_utf8"].as_str().expect("bytes_utf8");
    let tampered = bytes.replace("\"ttl_secs\":300", "\"ttl_secs\":301");
    assert_ne!(tampered, bytes, "the substitution must apply");
    let decoded = CapabilityAnnouncement::from_bytes(tampered.as_bytes()).expect("still decodes");
    assert!(
        decoded.verify().is_err(),
        "one changed signed field must fail verification"
    );
}

/// The leaf's nRPC REQUEST frame decodes through the production
/// `EventMeta` + `RpcRequestPayload` and re-encodes byte-identically.
#[test]
fn the_leaf_generated_nrpc_frame_round_trips_through_the_production_codec() {
    use net::adapter::net::channel::channel_hash;
    use net::adapter::net::cortex::{
        decode_rpc_route, encode_rpc_route, peek_request_service, EventMeta, RpcRequestPayload,
        DISPATCH_RPC_REQUEST, EVENT_META_SIZE, RPC_FRAME_BODY_OFFSET,
    };

    let f = fixture("nrpc_frame.json");
    let frame = unhex(&expected_hex(&f));

    let meta = EventMeta::from_bytes(&frame).expect("the EventMeta prefix must decode");
    assert_eq!(meta.dispatch, DISPATCH_RPC_REQUEST);
    assert_eq!(meta.flags, 0, "a leaf client is unary only");
    assert_eq!(
        meta.origin_hash.to_string(),
        f["fields"]["origin_hash"].as_str().expect("origin_hash")
    );
    assert_eq!(
        meta.seq_or_ts.to_string(),
        f["fields"]["call_id"].as_str().expect("call_id"),
        "the call_id must ride EventMeta::seq_or_ts, where the client's \
         publish site puts it"
    );
    assert_eq!(
        meta.checksum, 0,
        "a mesh frame's checksum is zero — the value protects on-disk \
         RedEX records, and a mesh frame is covered by the packet AEAD"
    );

    // The RpcRouteV1 discriminator must be the canonical hash of the
    // channel the fixture names, so mesh ingress selects exactly one
    // registered dispatcher.
    let route = decode_rpc_route(&frame).expect("the route discriminator must decode");
    let channel = f["fields"]["route_channel"]
        .as_str()
        .expect("route_channel");
    assert_eq!(
        route,
        channel_hash(channel),
        "the leaf's route discriminator is not the canonical hash of {channel}"
    );

    // The payload decodes through the production decoder, including
    // the header codec and the service peek the serve bridge uses.
    // `decode` takes the payload region — everything after the
    // `EventMeta` prefix and the route discriminator.
    let payload = RpcRequestPayload::decode(Bytes::from(frame[RPC_FRAME_BODY_OFFSET..].to_vec()))
        .expect("the production decoder must accept the leaf's payload");
    assert_eq!(
        payload.service,
        f["fields"]["service"].as_str().expect("service")
    );
    assert_eq!(
        payload.deadline_ns,
        f["fields"]["deadline_ns"].as_u64().expect("deadline_ns")
    );
    assert_eq!(payload.flags, 0);
    assert_eq!(
        payload.headers,
        vec![(
            "content-type".to_string(),
            b"application/octet-stream".to_vec()
        )]
    );
    assert_eq!(payload.body, Bytes::from_static(b"join"));
    assert_eq!(
        peek_request_service(&frame),
        Some(payload.service.as_str()),
        "the serve bridge's zero-decode service peek must read the leaf's frame"
    );

    // And the production encoder reproduces the leaf's bytes exactly.
    let mut rebuilt = Vec::with_capacity(frame.len());
    rebuilt.extend_from_slice(&meta.to_bytes());
    encode_rpc_route(&mut rebuilt, route);
    payload.encode_into(&mut rebuilt);
    assert_eq!(
        hex(&rebuilt),
        expected_hex(&f),
        "the production nRPC encoder produced different bytes than the \
         leaf's — the two copies have drifted; net-mesh-leaf's `rpc_wire` \
         module and this fixture move together or not at all"
    );
    assert_eq!(RPC_FRAME_BODY_OFFSET, EVENT_META_SIZE + 8);
}
