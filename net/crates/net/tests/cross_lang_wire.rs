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
    let f = fixture("aead_vector.json");
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
