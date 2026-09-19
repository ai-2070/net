//! The S0a minimum proof: a routed handshake/envelope round-trip
//! through the extracted wire code, with no sockets and no tokio.
//!
//! Shared by the native test (`tests/routed_roundtrip.rs`) and the
//! wasm export (`crate::wasm_wire_probe`), so the wasm artifact's size
//! includes the Noise handshake and the packet cipher rather than
//! dead-stripping them.
//!
//! Shape, matching production:
//!
//! 1. Two in-memory endpoints, NKpsk0 (`crypto::NoiseHandshake`) with
//!    the real `handshake_prologue(src, dest)` binding.
//! 2. Both sides build a `NetSession` from the derived `SessionKeys`.
//! 3. The initiator builds a Net packet through the session's
//!    `SharedLocalPool` / `PacketBuilder` (`pool.rs`).
//! 4. The packet is wrapped in a `RoutingHeader` addressed to the
//!    responder's node id and handed over as a plain byte vector —
//!    the same `routing_bytes ++ net_packet` layout as
//!    `MeshNode::send_routed` (`mesh.rs:28390-28394`).
//! 5. The responder discriminates on `ROUTING_MAGIC`, decodes the
//!    envelope, checks `dest_id`, strips it, parses the inner packet,
//!    decrypts it with its `rx_cipher`, and reads the event frames.

use bytes::Bytes;

use crate::crypto::{handshake_prologue, CryptoError, NoiseHandshake, StaticKeypair};
use crate::protocol::{EventFrame, PacketFlags};
use crate::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};
use crate::session::NetSession;

/// Node ids of the two endpoints.
pub const INITIATOR_NODE_ID: u64 = 0x1111_2222_3333_4444;
pub const RESPONDER_NODE_ID: u64 = 0xAAAA_BBBB_CCCC_DDDD;

/// The payload the round-trip carries.
pub const PAYLOAD: &[u8] = b"s0a routed round-trip payload";

/// A round-trip failure, with the stage that produced it.
#[derive(Debug)]
pub enum RoundtripError {
    /// Noise / AEAD failure.
    Crypto(CryptoError),
    /// The envelope or packet did not decode.
    Wire(&'static str),
}

impl From<CryptoError> for RoundtripError {
    fn from(e: CryptoError) -> Self {
        Self::Crypto(e)
    }
}

impl core::fmt::Display for RoundtripError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Crypto(e) => write!(f, "crypto: {e}"),
            Self::Wire(m) => write!(f, "wire: {m}"),
        }
    }
}

/// Run the full routed round-trip and return the payload the
/// responder recovered.
///
/// `psk_byte` seeds the pre-shared key so the wasm export takes a real
/// argument (and so the optimizer cannot fold the whole call away).
pub fn routed_roundtrip(psk_byte: u8) -> Result<Vec<u8>, RoundtripError> {
    let psk = [psk_byte ^ 0x5A; 32];

    // --- 1. NKpsk0 handshake between two in-memory endpoints -------
    //
    // Deterministic responder static: an X25519 secret is any 32
    // bytes (clamped by the curve), and the public half is fixed by
    // the pattern, so the initiator gets it out of band the way
    // `connect_via` does.
    let responder_static = responder_keypair();

    // The prologue binds the handshake to the (src, dest) node pair —
    // exactly what the routed path needs, since the envelope's
    // `src_id` is an unauthenticated 32-bit claim.
    let prologue = handshake_prologue(INITIATOR_NODE_ID, RESPONDER_NODE_ID);

    let mut initiator =
        NoiseHandshake::initiator_with_prologue(&psk, responder_static.public_key(), &prologue)?;
    let mut responder =
        NoiseHandshake::responder_with_prologue(&psk, &responder_static, &prologue)?;

    let msg1 = initiator.write_message(&[])?;
    let _ = responder.read_message(&msg1)?;
    let msg2 = responder.write_message(&[])?;
    let _ = initiator.read_message(&msg2)?;

    if !initiator.is_finished() || !responder.is_finished() {
        return Err(RoundtripError::Wire("handshake did not complete"));
    }

    let initiator_keys = initiator.into_session_keys()?;
    let responder_keys = responder.into_session_keys()?;

    // --- 2. Sessions on both sides ---------------------------------
    //
    // `NetSession::new` still takes a `SocketAddr` (see the report:
    // this is the Stage 1 `PeerAddr` job, not S0a's). Parsing a
    // literal is pure `core` and works on wasm32.
    let initiator_addr = "127.0.0.1:1".parse().map_err(|_| wire("bad addr"))?;
    let responder_addr = "127.0.0.1:2".parse().map_err(|_| wire("bad addr"))?;

    let initiator_session = NetSession::new(initiator_keys, responder_addr, 2, false);
    let responder_session = NetSession::new(responder_keys, initiator_addr, 2, false);

    // --- 3. Build a Net packet through the session's pool ----------
    let stream_id = 0x0102_0304_0506_0708;
    let seq = initiator_session.get_or_create_stream(stream_id).next_tx_seq();
    let events = [Bytes::from_static(PAYLOAD)];
    let net_packet = {
        let mut builder = initiator_session.thread_local_pool().get();
        builder.build(stream_id, seq, &events, PacketFlags::NONE)
    };

    // --- 4. Wrap in a routing envelope and "transmit" --------------
    let header = RoutingHeader::new(
        RESPONDER_NODE_ID,
        INITIATOR_NODE_ID as u32,
        crate::route_codec::_MAX_TTL,
    );
    let mut on_the_wire = Vec::with_capacity(ROUTING_HEADER_SIZE + net_packet.len());
    on_the_wire.extend_from_slice(&header.to_bytes());
    on_the_wire.extend_from_slice(&net_packet);

    // --- 5. Receive: discriminate, unwrap, parse, decrypt ----------
    if on_the_wire.len() < ROUTING_HEADER_SIZE {
        return Err(wire("short datagram"));
    }
    let magic = u16::from_le_bytes([on_the_wire[0], on_the_wire[1]]);
    if magic != ROUTING_MAGIC {
        return Err(wire("not a routing envelope"));
    }
    let inbound_header = RoutingHeader::from_bytes(&on_the_wire).ok_or(wire("bad envelope"))?;
    if inbound_header.dest_id != RESPONDER_NODE_ID {
        // A leaf drops anything not addressed to itself (§7,
        // "non-forwarding is a role").
        return Err(wire("envelope not addressed to this node"));
    }
    if inbound_header.is_expired() {
        return Err(wire("ttl expired"));
    }

    let inner = Bytes::copy_from_slice(&on_the_wire[ROUTING_HEADER_SIZE..]);
    let parsed =
        crate::parsed_packet::ParsedPacket::parse(inner, initiator_addr).ok_or(wire("bad packet"))?;
    if !parsed.is_valid_length() {
        return Err(wire("payload length mismatch"));
    }
    if parsed.header.session_id != responder_session.session_id() {
        return Err(wire("session id mismatch"));
    }

    let aad = parsed.header.aad();
    let counter = u64::from_le_bytes(
        parsed.header.nonce[4..12]
            .try_into()
            .map_err(|_| wire("short nonce"))?,
    );
    let rx = responder_session.rx_cipher();
    let decrypted = rx.decrypt_to_bytes(counter, &aad, parsed.payload.clone())?;
    if !rx.try_admit_rx_counter(counter) {
        return Err(wire("replay window rejected the counter"));
    }

    let mut frames = EventFrame::read_events(decrypted, parsed.header.event_count);
    if frames.len() != 1 {
        return Err(wire("expected exactly one event frame"));
    }
    Ok(frames.remove(0).to_vec())
}

#[inline]
fn wire(msg: &'static str) -> RoundtripError {
    RoundtripError::Wire(msg)
}

/// Deterministic responder static keypair, so the round-trip is
/// reproducible and needs no RNG on wasm.
fn responder_keypair() -> StaticKeypair {
    let secret = [7u8; 32];
    let public = x25519_base_mul(secret);
    StaticKeypair::from_keys(secret, public)
}

/// X25519 public key from a secret, via `ring`'s agreement API — the
/// crate already links `ring`, so this adds no dependency.
fn x25519_base_mul(secret: [u8; 32]) -> [u8; 32] {
    // `snow` will clamp and re-derive internally; the responder only
    // needs `public` to match what its own private key produces. Use
    // snow's own DH so the two agree by construction.
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    use snow::params::DHChoice;
    let resolver = DefaultResolver;
    let mut dh = resolver
        .resolve_dh(&DHChoice::Curve25519)
        .expect("curve25519 is always available in snow's default resolver");
    dh.set(&secret);
    let mut public = [0u8; 32];
    public.copy_from_slice(&dh.pubkey()[..32]);
    public
}
