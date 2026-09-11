//! S0a's minimum proof (plan §Stage 0): a **routed**
//! handshake/envelope round-trip through the extracted wire code.
//!
//! Compilation alone would pass while omitting Layer 2 — the leaf's
//! only pre-direct path — so this exercises the whole chain:
//! NKpsk0 handshake → `PacketBuilder` packet → `RoutingHeader`
//! envelope → bytes → unwrap → AEAD decrypt → payload.
//!
//! No sockets, no tokio.

use s0a_wire::roundtrip::{routed_roundtrip, PAYLOAD};

#[test]
fn routed_envelope_round_trips_through_the_extracted_wire_code() {
    let recovered = routed_roundtrip(0x2A).expect("routed round-trip completes");
    assert_eq!(
        recovered.as_slice(),
        PAYLOAD,
        "the responder must recover the initiator's payload byte for byte",
    );
}

/// A misaddressed envelope is refused rather than decrypted — the
/// non-forwarding leaf rule (§7), and the reason the envelope codec
/// has to travel with the wire modules at all.
#[test]
fn an_envelope_addressed_elsewhere_is_refused() {
    use bytes::Bytes;
    use s0a_wire::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};

    let header = RoutingHeader::new(0xDEAD_BEEF_0000_0001, 0x1234, 8);
    let mut wire = Vec::new();
    wire.extend_from_slice(&header.to_bytes());
    wire.extend_from_slice(b"inner packet bytes");

    let decoded = RoutingHeader::from_bytes(&wire).expect("envelope decodes");
    assert_eq!(u16::from_le_bytes([wire[0], wire[1]]), ROUTING_MAGIC);
    assert_eq!(decoded.dest_id, 0xDEAD_BEEF_0000_0001);
    assert_ne!(
        decoded.dest_id,
        s0a_wire::roundtrip::RESPONDER_NODE_ID,
        "this envelope is for someone else",
    );

    // And the codec round-trips the mutable fields the forwarder
    // touches, which is what `write_to` is for.
    let mut buf = bytes::BytesMut::new();
    decoded.write_to(&mut buf);
    let frozen: Bytes = buf.freeze();
    assert_eq!(frozen.len(), ROUTING_HEADER_SIZE);
    assert_eq!(
        RoutingHeader::from_bytes(&frozen).expect("re-decodes"),
        decoded,
    );
}
