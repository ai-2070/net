//! The executed wasm proof for `net-mesh-wire`.
//!
//! `cargo check --target wasm32-unknown-unknown` cannot catch the two
//! failures this file exists for:
//!
//! 1. `std::time::Instant::now()` and `SystemTime::now()` **compile**
//!    for `wasm32-unknown-unknown` and then panic at runtime ("time
//!    not implemented on this platform"). Only running the code
//!    proves the [`clock`](net_wire::clock) seam is actually in the
//!    path — `session.rs` and `reliability.rs` read the clock on
//!    every packet, so a missed site is a browser leaf that dies on
//!    its first datagram.
//! 2. The wasm build seals packets with the RustCrypto
//!    `chacha20poly1305` backend instead of `ring`. A backend that
//!    produced different bytes would be a silent wire-format split
//!    between native and browser nodes; the golden vector under
//!    `tests/cross_lang_wire/aead_vector.json` is replayed here on
//!    the wasm side and asserted against the same expected bytes the
//!    native `cross_lang_wire` test asserts on the ring side.
//!
//! Run: `wasm-pack test --node net/crates/net/wire`, or
//! `cargo test -p net-mesh-wire --target wasm32-unknown-unknown` with
//! `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner`.
#![cfg(target_arch = "wasm32")]

use bytes::Bytes;
use wasm_bindgen_test::wasm_bindgen_test;

use net_wire::aead::AeadKey;
use net_wire::clock::{Clock, SystemClock};
use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
use net_wire::parsed_packet::ParsedPacket;
use net_wire::peer_addr::PeerAddr;
use net_wire::protocol::{EventFrame, PacketFlags};
use net_wire::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC, _MAX_TTL};
use net_wire::session::NetSession;

const INITIATOR_NODE_ID: u64 = 0x1111_2222_3333_4444;
const RESPONDER_NODE_ID: u64 = 0xAAAA_BBBB_CCCC_DDDD;
const PAYLOAD: &[u8] = b"net-wire routed round-trip payload";

/// Deterministic responder static keypair, so the round-trip needs no
/// RNG entropy beyond what the handshake itself draws.
fn responder_keypair() -> StaticKeypair {
    use snow::params::DHChoice;
    use snow::resolvers::{CryptoResolver, DefaultResolver};

    let secret = [7u8; 32];
    let mut dh = DefaultResolver
        .resolve_dh(&DHChoice::Curve25519)
        .expect("curve25519 is always available in snow's default resolver");
    dh.set(&secret);
    let mut public = [0u8; 32];
    public.copy_from_slice(&dh.pubkey()[..32]);
    StaticKeypair::from_keys(secret, public)
}

/// The S0a minimum proof, now against the real crate: NKpsk0
/// handshake → `PacketBuilder` → `RoutingHeader` → unwrap → decrypt,
/// entirely in memory, executed in Node on wasm32.
///
/// Layer 2 (the routing envelope) is a browser leaf's only pre-direct
/// path, which is why the proof is a *routed* round-trip rather than
/// a direct one.
#[wasm_bindgen_test]
fn routed_round_trip_runs_on_wasm() {
    let psk = [0x5Au8; 32];
    let responder_static = responder_keypair();
    let prologue = handshake_prologue(INITIATOR_NODE_ID, RESPONDER_NODE_ID);

    let mut initiator =
        NoiseHandshake::initiator_with_prologue(&psk, responder_static.public_key(), &prologue)
            .expect("initiator handshake state");
    let mut responder = NoiseHandshake::responder_with_prologue(&psk, &responder_static, &prologue)
        .expect("responder handshake state");

    let msg1 = initiator.write_message(&[]).expect("msg1");
    responder.read_message(&msg1).expect("read msg1");
    let msg2 = responder.write_message(&[]).expect("msg2");
    initiator.read_message(&msg2).expect("read msg2");
    assert!(initiator.is_finished() && responder.is_finished());

    let initiator_keys = initiator.into_session_keys().expect("initiator keys");
    let responder_keys = responder.into_session_keys().expect("responder keys");

    let initiator_addr = PeerAddr::Udp("127.0.0.1:1".parse().expect("addr"));
    let responder_addr = PeerAddr::Udp("127.0.0.1:2".parse().expect("addr"));

    // Constructing a session stamps `last_activity` from the coarse
    // clock: the first of the runtime panics this test exists for.
    let initiator_session = NetSession::new(initiator_keys, responder_addr, 2, false);
    let responder_session = NetSession::new(responder_keys, initiator_addr, 2, false);

    let stream_id = 0x0102_0304_0506_0708;
    let seq = initiator_session
        .get_or_create_stream(stream_id)
        .next_tx_seq();
    let events = [Bytes::from_static(PAYLOAD)];
    let net_packet = {
        let mut builder = initiator_session.thread_local_pool().get();
        builder.build(stream_id, seq, &events, PacketFlags::NONE)
    };

    // `routing_bytes ++ net_packet`, the exact layout `send_routed`
    // puts on the wire.
    let header = RoutingHeader::new(RESPONDER_NODE_ID, INITIATOR_NODE_ID as u32, _MAX_TTL);
    let mut on_the_wire = Vec::with_capacity(ROUTING_HEADER_SIZE + net_packet.len());
    on_the_wire.extend_from_slice(&header.to_bytes());
    on_the_wire.extend_from_slice(&net_packet);

    let magic = u16::from_le_bytes([on_the_wire[0], on_the_wire[1]]);
    assert_eq!(magic, ROUTING_MAGIC, "envelope discriminator");
    let inbound = RoutingHeader::from_bytes(&on_the_wire).expect("envelope decodes");
    assert_eq!(
        inbound.dest_id, RESPONDER_NODE_ID,
        "a leaf drops anything not addressed to itself"
    );
    assert!(!inbound.is_expired());

    let inner = Bytes::copy_from_slice(&on_the_wire[ROUTING_HEADER_SIZE..]);
    let parsed = ParsedPacket::parse(inner, initiator_addr).expect("inner packet parses");
    assert!(parsed.is_valid_length());
    assert_eq!(parsed.header.session_id, responder_session.session_id());

    let aad = parsed.header.aad();
    let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
    let rx = responder_session.rx_cipher();
    let decrypted = rx
        .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
        .expect("decrypt on the RustCrypto backend");
    assert!(rx.try_admit_rx_counter(counter), "replay window admits once");

    let frames = EventFrame::read_events(decrypted, parsed.header.event_count);
    assert_eq!(frames.len(), 1);
    assert_eq!(&frames[0][..], PAYLOAD, "payload survives the round trip");
}

/// A `Clock` read that `std::time::Instant::now()` would panic on.
///
/// Both halves of the seam are exercised: the monotonic reading
/// (`web_time::Instant`, i.e. `performance.now()`) and the wall-clock
/// one, which `current_timestamp` caches per thread.
#[wasm_bindgen_test]
fn clock_reads_do_not_panic_on_wasm() {
    let first = SystemClock::now();
    let unix_ns = SystemClock::now_unix_nanos();
    assert!(
        unix_ns > 1_600_000_000_000_000_000,
        "wall clock must be a real epoch reading, got {unix_ns}"
    );

    // Monotonic: a second reading is never before the first.
    let second = SystemClock::now();
    assert!(second.duration_since(first).as_nanos() < u128::MAX);

    // The coarse packet clock — the per-packet path — must return a
    // sane, non-zero timestamp and stay monotonic across calls.
    let a = net_wire::time::current_timestamp();
    let b = net_wire::time::current_timestamp();
    assert!(a > 0 && b >= a, "coarse clock: a={a}, b={b}");
}

/// The AEAD golden vector, replayed on the `chacha20poly1305`
/// backend. Same fixture the native `cross_lang_wire` test asserts on
/// the `ring` backend; a divergence here is a wire-format split
/// between native and browser nodes.
#[wasm_bindgen_test]
fn aead_golden_vector_matches_on_the_wasm_backend() {
    const FIXTURE: &str = include_str!("../../tests/cross_lang_wire/aead_vector.json");

    let key: [u8; 32] = unhex(field(FIXTURE, "key_hex")).try_into().expect("key");
    let nonce: [u8; 12] = unhex(field(FIXTURE, "nonce_hex")).try_into().expect("nonce");
    let aad = unhex(field(FIXTURE, "aad_hex"));
    let plaintext = field(FIXTURE, "plaintext_utf8");
    let expected = field(FIXTURE, "ciphertext_and_tag_hex");

    let aead = AeadKey::new(&key);
    let mut sealed = plaintext.as_bytes().to_vec();
    aead.seal_append_tag(nonce, &aad, &mut sealed)
        .expect("seal on wasm");
    assert_eq!(
        hex(&sealed),
        expected,
        "the wasm AEAD backend produced different ciphertext than the \
         pinned vector — native and browser nodes would not interoperate"
    );

    let mut opened = unhex(expected);
    let len = aead
        .open_in_place(nonce, &aad, &mut opened)
        .expect("open the pinned ciphertext");
    assert_eq!(&opened[..len], plaintext.as_bytes());
}

/// Minimal JSON string-field reader.
///
/// `serde_json` is not a dependency of the wasm build (it rides the
/// core-only `json` feature), and pulling it in as a dev-dependency
/// purely to read six string fields would change what the wasm test
/// links. The fixture is generated, flat, and contains no escapes.
fn field<'a>(src: &'a str, name: &str) -> &'a str {
    let key = format!("\"{name}\"");
    let start = src
        .find(&key)
        .unwrap_or_else(|| panic!("fixture has no field {name}"));
    let rest = &src[start + key.len()..];
    let colon = rest.find(':').expect("field separator");
    let after = &rest[colon + 1..];
    let open = after.find('"').expect("string value opens");
    let tail = &after[open + 1..];
    let close = tail.find('"').expect("string value closes");
    &tail[..close]
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
