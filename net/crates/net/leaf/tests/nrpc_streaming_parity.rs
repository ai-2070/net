//! The streaming half of the nRPC codec pin.
//!
//! `tests/fixture_parity.rs` pins the leaf's client codec against the
//! shared `cross_lang_wire` fixture; this file pins the Stage 4
//! streaming extension — request decode, response encode, request
//! chunks, BOTH grant frames, the streaming markers and the terminal
//! mapping — **byte for byte** against the core definitions the wire
//! spec extracted, and closes the spec's §12 discrepancy ledger: the
//! mirror must refuse exactly what core refuses (empty header name,
//! empty service, over-cap fields) and accept exactly what core
//! accepts (empty header values, empty bodies, unknown dispatches).
//!
//! Most assertions below are either a concrete byte vector written
//! by hand (never through the codec under test) or an exact error
//! identity (`LeafError::Rpc(RpcError::Malformed(msg))` with the
//! message compared). The five `*_round_trips_*` witnesses are the
//! documented exception: they build their payloads by hand but
//! assert encode→decode symmetry (a field-faithful round trip plus
//! `encoded_len`), not a byte pin. No "does not panic" passes.

use bytes::Bytes;
use net_leaf::rpc_wire::*;
use net_leaf::{LeafError, RpcError};

/// Extract the exact error identity: `LeafError::Rpc(Malformed(msg))`
/// with `msg` compared verbatim. Anything else panics with detail.
fn refuse<T>(result: net_leaf::Result<T>) -> String {
    match result {
        Err(LeafError::Rpc(RpcError::Malformed(msg))) => msg,
        Err(other) => panic!("expected RpcError::Malformed, got {other:?}"),
        Ok(_) => panic!("expected a refusal, got Ok"),
    }
}

fn round_trip_request(payload: &RpcRequestPayload) {
    let mut buf = Vec::new();
    payload.encode_into(&mut buf).expect("encode must accept");
    assert_eq!(
        buf.len(),
        payload.encoded_len(),
        "encoded_len must equal the bytes written"
    );
    let decoded = RpcRequestPayload::decode(Bytes::from(buf)).expect("decode must accept");
    assert_eq!(&decoded, payload, "the round trip must be field-faithful");
}

fn round_trip_response(payload: &RpcResponsePayload) {
    let mut buf = Vec::new();
    payload.encode_into(&mut buf).expect("encode must accept");
    assert_eq!(
        buf.len(),
        payload.encoded_len(),
        "encoded_len must equal the bytes written"
    );
    let decoded = RpcResponsePayload::decode(Bytes::from(buf)).expect("decode must accept");
    assert_eq!(&decoded, payload, "the round trip must be field-faithful");
}

fn round_trip_chunk(payload: &RpcRequestChunkPayload) {
    let mut buf = Vec::new();
    payload.encode_into(&mut buf).expect("encode must accept");
    assert_eq!(
        buf.len(),
        payload.encoded_len(),
        "encoded_len must equal the bytes written"
    );
    let decoded = RpcRequestChunkPayload::decode(Bytes::from(buf)).expect("decode must accept");
    assert_eq!(&decoded, payload, "the round trip must be field-faithful");
}

// ---------------------------------------------------------------------------
// (a) the four payload codecs: round-trips at the wire caps, and refusal
// exactly where core refuses.
// ---------------------------------------------------------------------------

#[test]
fn request_payload_round_trips_at_the_wire_caps() {
    let service = "s".repeat(MAX_RPC_SERVICE_NAME_LEN); // 255, the cap
    let mut headers = Vec::new();
    for i in 0..MAX_RPC_HEADERS {
        // 64-byte names (the cap) and 4096-byte values (the cap).
        headers.push((format!("h{i:063}"), vec![0xAB; MAX_RPC_HEADER_VALUE_LEN]));
    }
    let payload = RpcRequestPayload {
        service,
        deadline_ns: u64::MAX,
        flags: FLAG_RPC_STREAMING_RESPONSE
            | FLAG_RPC_CLIENT_STREAMING_REQUEST
            | FLAG_RPC_REQUEST_END,
        headers,
        body: Bytes::from_static(b"at the caps"),
    };
    round_trip_request(&payload);
}

#[test]
fn request_payload_round_trips_an_empty_body_and_empty_header_values() {
    let mut payload = RpcRequestPayload::unary("svc", 0, Bytes::new());
    // Core accepts empty header VALUES (only names are 1..); the
    // mirror must accept the same set — §12 is about refusing exactly
    // what core refuses, no more and no less.
    payload
        .headers
        .push(("empty-value".to_string(), Vec::new()));
    round_trip_request(&payload);
}

#[test]
fn request_payload_round_trips_a_max_body() {
    let payload = RpcRequestPayload::unary(
        "svc",
        1,
        Bytes::from(vec![0x5A; MAX_RPC_BODY_LEN]), // 4 MiB, the cap
    );
    round_trip_request(&payload);
}

#[test]
fn request_codec_refuses_an_empty_service_at_encode_and_decode() {
    let payload = RpcRequestPayload::unary("", 0, Bytes::new());
    assert_eq!(refuse(payload.validate()), "empty service name");
    assert_eq!(
        refuse(encode_request_frame(0, 0, 0, &payload)),
        "empty service name",
        "§12.4: the mirror must never emit a 0-length-service frame"
    );
    // Decode side: a bare 0 service-length byte, which core rejects
    // (`Truncated("empty service name")`).
    assert_eq!(
        refuse(RpcRequestPayload::decode(Bytes::from_static(&[0]))),
        "empty service name"
    );
}

#[test]
fn request_codec_refuses_an_empty_header_name_at_encode_and_decode() {
    let mut payload = RpcRequestPayload::unary("svc", 0, Bytes::new());
    payload.headers.push((String::new(), b"v".to_vec()));
    assert_eq!(refuse(payload.validate()), "empty header name");
    assert_eq!(
        refuse(encode_request_frame(0, 0, 0, &payload)),
        "empty header name",
        "§12.3: core's decode rejects name_len == 0, so no encoder may emit one"
    );

    // Hand-built wire: service "svc", deadline 0, flags 0, one header
    // with a 0-length name, empty body.
    let mut wire = vec![3u8];
    wire.extend_from_slice(b"svc");
    wire.extend_from_slice(&0u64.to_le_bytes());
    wire.extend_from_slice(&0u16.to_le_bytes());
    wire.push(1); // header count
    wire.push(0); // empty header name — core refuses (rpc.rs:1046-1048)
    wire.extend_from_slice(&1u16.to_le_bytes());
    wire.push(b'v');
    wire.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        refuse(RpcRequestPayload::decode(Bytes::from(wire))),
        "empty header name"
    );
}

#[test]
fn request_codec_refuses_over_cap_fields_at_encode_and_decode() {
    // Encode side: every length prefix refuses instead of truncating.
    let over_service =
        RpcRequestPayload::unary("s".repeat(MAX_RPC_SERVICE_NAME_LEN + 1), 0, Bytes::new());
    assert_eq!(
        refuse(over_service.validate_wire_bounds()),
        "service is 256 bytes, over the 255-byte wire limit"
    );
    assert_eq!(
        refuse(encode_request_frame(0, 0, 0, &over_service)),
        "service is 256 bytes, over the 255-byte wire limit"
    );

    let mut over_headers = RpcRequestPayload::unary("svc", 0, Bytes::new());
    for i in 0..=MAX_RPC_HEADERS {
        over_headers.headers.push((format!("h{i}"), Vec::new()));
    }
    assert_eq!(
        refuse(over_headers.validate_wire_bounds()),
        "headers is 33 bytes, over the 32-byte wire limit"
    );

    let mut over_name = RpcRequestPayload::unary("svc", 0, Bytes::new());
    over_name
        .headers
        .push(("n".repeat(MAX_RPC_HEADER_NAME_LEN + 1), Vec::new()));
    assert_eq!(
        refuse(over_name.validate_wire_bounds()),
        "header name is 65 bytes, over the 64-byte wire limit"
    );

    let mut over_value = RpcRequestPayload::unary("svc", 0, Bytes::new());
    over_value
        .headers
        .push(("n".to_string(), vec![0; MAX_RPC_HEADER_VALUE_LEN + 1]));
    assert_eq!(
        refuse(over_value.validate_wire_bounds()),
        "header value is 4097 bytes, over the 4096-byte wire limit"
    );

    let over_body = RpcRequestPayload::unary("svc", 0, Bytes::from(vec![0; MAX_RPC_BODY_LEN + 1]));
    assert_eq!(
        refuse(over_body.validate_wire_bounds()),
        "body is 4194305 bytes, over the 4194304-byte wire limit"
    );

    // Decode side: over-cap values are rejected on the declared
    // length, before any allocation.
    fn request_wire(tail: &[u8]) -> Bytes {
        let mut wire = vec![3u8];
        wire.extend_from_slice(b"svc");
        wire.extend_from_slice(&0u64.to_le_bytes());
        wire.extend_from_slice(&0u16.to_le_bytes());
        wire.extend_from_slice(tail);
        Bytes::from(wire)
    }
    assert_eq!(
        refuse(RpcRequestPayload::decode(request_wire(&[33]))),
        "header count exceeds MAX_RPC_HEADERS"
    );
    assert_eq!(
        refuse(RpcRequestPayload::decode(request_wire(&[1, 65]))),
        "header name exceeds MAX_RPC_HEADER_NAME_LEN"
    );
    let mut tail = vec![1, 1, b'x'];
    tail.extend_from_slice(&(MAX_RPC_HEADER_VALUE_LEN as u16 + 1).to_le_bytes());
    assert_eq!(
        refuse(RpcRequestPayload::decode(request_wire(&tail))),
        "header value exceeds MAX_RPC_HEADER_VALUE_LEN"
    );
    let mut tail = vec![0]; // zero headers
    tail.extend_from_slice(&((MAX_RPC_BODY_LEN as u32) + 1).to_le_bytes());
    assert_eq!(
        refuse(RpcRequestPayload::decode(request_wire(&tail))),
        "body exceeds MAX_RPC_BODY_LEN"
    );
}

#[test]
fn validate_wire_bounds_is_the_size_mirror_while_validate_adds_the_empty_rules() {
    let mut payload = RpcRequestPayload::unary("", 0, Bytes::new());
    payload.headers.push((String::new(), Vec::new()));
    payload
        .validate_wire_bounds()
        .expect("core's validate_wire_bounds is caps-only: empty names sit within bounds");
    assert_eq!(
        refuse(payload.validate()),
        "empty service name",
        "the leaf's encode gate layers the §12.4 empty rules on top"
    );

    let mut named = RpcRequestPayload::unary("svc", 0, Bytes::new());
    named.headers.push((String::new(), Vec::new()));
    assert_eq!(refuse(named.validate()), "empty header name");
}

#[test]
fn response_payload_round_trips_status_headers_and_body() {
    round_trip_response(&RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![("content-type".to_string(), b"application/json".to_vec())],
        body: Bytes::from_static(b"NMO1"),
    });
    round_trip_response(&RpcResponsePayload {
        status: RpcStatus::Application(0x8001),
        headers: vec![("empty-value".to_string(), Vec::new())],
        body: Bytes::new(),
    });
    round_trip_response(&RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![],
        body: Bytes::from(vec![0xFF; MAX_RPC_BODY_LEN]), // max body
    });
}

#[test]
fn response_codec_refuses_empty_header_names_and_over_cap_bodies() {
    let named = RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![(String::new(), b"v".to_vec())],
        body: Bytes::new(),
    };
    named
        .validate_wire_bounds()
        .expect("caps-only: an empty name sits within bounds");
    assert_eq!(
        refuse(named.encode_into(&mut Vec::new())),
        "empty header name",
        "core's decode refuses name_len == 0, so encode must never emit it"
    );

    let over_body = RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![],
        body: Bytes::from(vec![0; MAX_RPC_BODY_LEN + 1]),
    };
    assert_eq!(
        refuse(over_body.encode_into(&mut Vec::new())),
        "body is 4194305 bytes, over the 4194304-byte wire limit"
    );

    // Decode side, hand-built.
    // status 0, one header with a 0-length name.
    let wire = [0x00, 0x00, 0x01, 0x00];
    assert_eq!(
        refuse(RpcResponsePayload::decode(Bytes::copy_from_slice(&wire))),
        "empty header name"
    );
    // status 0, zero headers, declared body 4 MiB + 1.
    let mut wire = vec![0x00, 0x00, 0x00];
    wire.extend_from_slice(&((MAX_RPC_BODY_LEN as u32) + 1).to_le_bytes());
    assert_eq!(
        refuse(RpcResponsePayload::decode(Bytes::from(wire))),
        "body exceeds MAX_RPC_BODY_LEN"
    );
    // A single byte cannot even carry the status.
    assert_eq!(
        refuse(RpcResponsePayload::decode(Bytes::from_static(&[0x00]))),
        "status"
    );
}

#[test]
fn request_chunk_payload_round_trips_and_mirrors_the_request_refusals() {
    round_trip_chunk(&RpcRequestChunkPayload {
        call_id: 0x0102_0304_0506_0708,
        flags: FLAG_RPC_REQUEST_END,
        headers: vec![("t".to_string(), b"v".to_vec())],
        body: Bytes::from_static(b"chunk body"),
    });
    round_trip_chunk(&RpcRequestChunkPayload {
        call_id: 9,
        flags: 0,
        headers: vec![],
        body: Bytes::new(),
    });

    // Refusals mirror the request codec: empty header names and
    // over-cap bodies, at encode and decode.
    let named = RpcRequestChunkPayload {
        call_id: 1,
        flags: 0,
        headers: vec![(String::new(), b"v".to_vec())],
        body: Bytes::new(),
    };
    assert_eq!(
        refuse(named.encode_into(&mut Vec::new())),
        "empty header name"
    );
    let mut wire = 1u64.to_le_bytes().to_vec();
    wire.extend_from_slice(&0u16.to_le_bytes());
    wire.extend_from_slice(&[1, 0, 0x00, 0x01, b'v', 0, 0, 0, 0]);
    assert_eq!(
        refuse(RpcRequestChunkPayload::decode(Bytes::from(wire))),
        "empty header name"
    );
    assert_eq!(
        refuse(RpcRequestChunkPayload::decode(Bytes::from_static(
            &[0u8; 7]
        ))),
        "call_id"
    );
    let mut wire = 1u64.to_le_bytes().to_vec();
    wire.extend_from_slice(&0u16.to_le_bytes());
    wire.push(0); // zero headers
    wire.extend_from_slice(&((MAX_RPC_BODY_LEN as u32) + 1).to_le_bytes());
    assert_eq!(
        refuse(RpcRequestChunkPayload::decode(Bytes::from(wire))),
        "body exceeds MAX_RPC_BODY_LEN"
    );
}

// ---------------------------------------------------------------------------
// (b) grant codecs: big-endian credit bytes, byte-identical.
// ---------------------------------------------------------------------------

#[test]
fn stream_grant_is_a_four_byte_big_endian_credit() {
    assert_eq!(encode_stream_grant(5), vec![0x00, 0x00, 0x00, 0x05]);
    assert_eq!(
        encode_stream_grant(0x0102_0304),
        vec![0x01, 0x02, 0x03, 0x04]
    );
    assert_eq!(decode_stream_grant(&[0x00, 0x00, 0x00, 0x05]), Some(5));
    assert_eq!(
        decode_stream_grant(&[0x01, 0x02, 0x03, 0x04]),
        Some(0x0102_0304)
    );
    assert_eq!(
        decode_stream_grant(&[0x00, 0x00, 0x00]),
        None,
        "not exactly 4 bytes is a malformed grant"
    );
    assert_eq!(decode_stream_grant(&[0u8; 5]), None);
}

#[test]
fn stream_grant_frame_ends_in_the_big_endian_credit_bytes() {
    let frame = encode_stream_grant_frame(0xAAAA, 0x1234, 0xCAFE, 5);
    assert_eq!(frame.len(), 36, "24 B meta ‖ 8 B route ‖ 4 B credit");
    assert_eq!(&frame[..2], &[DISPATCH_RPC_STREAM_GRANT, 0]);
    assert_eq!(decode_route(&frame), Some(0xCAFE));
    assert_eq!(
        &frame[RPC_FRAME_BODY_OFFSET..],
        &[0x00, 0x00, 0x00, 0x05],
        "the credit rides at RPC_FRAME_BODY_OFFSET as a BE u32"
    );
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::StreamGrant {
            call_id: 0x1234,
            credits: 5
        })
    );
}

#[test]
fn request_grant_payload_is_exactly_twelve_mixed_endian_bytes() {
    // call_id u64 LITTLE-endian, credits u32 BIG-endian: the load-
    // bearing mixed encoding of §5.
    assert_eq!(
        encode_request_grant(0x0102_0304_0506_0708, 5),
        vec![
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // call_id LE
            0x00, 0x00, 0x00, 0x05, // credits BE
        ]
    );
    let bytes: &[u8] = &[
        0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00, 0x05,
    ];
    assert_eq!(
        decode_request_grant(bytes),
        Some(RpcRequestGrantPayload {
            call_id: 0x0102_0304_0506_0708,
            credits: 5
        })
    );
    assert_eq!(decode_request_grant(&[0u8; 11]), None);
    assert_eq!(decode_request_grant(&[0u8; 13]), None);
}

#[test]
fn request_grant_frame_is_the_pinned_forty_four_bytes() {
    // Whole-frame byte identity, written by hand: dispatch, flags,
    // zero pad, origin LE, call_id LE, zero checksum, route LE, then
    // the 12-byte mixed-endian grant.
    let exact = encode_request_grant_frame(1, 2, 3, 5);
    let expected: Vec<u8> = vec![
        0x16, 0x00, 0x00, 0x00, // dispatch ‖ flags ‖ pad
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // origin_hash LE
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // seq_or_ts LE
        0x00, 0x00, 0x00, 0x00, // checksum (zero on the mesh path)
        0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // route LE
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // grant call_id LE
        0x00, 0x00, 0x00, 0x05, // grant credits BE
    ];
    assert_eq!(exact, expected);
    assert_eq!(exact.len(), 44);

    let frame = encode_request_grant_frame(0xAAAA, 0x0102_0304_0506_0708, 0xCAFE, 5);
    assert_eq!(&frame[..2], &[DISPATCH_RPC_REQUEST_GRANT, 0]);
    assert_eq!(decode_route(&frame), Some(0xCAFE));
    assert_eq!(
        &frame[RPC_FRAME_BODY_OFFSET..],
        &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00, 0x05,]
    );
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::RequestGrant(RpcRequestGrantPayload {
            call_id: 0x0102_0304_0506_0708,
            credits: 5
        }))
    );
}

// ---------------------------------------------------------------------------
// (c) decode_frame over all seven dispatches; unknown dispatches tolerated.
// ---------------------------------------------------------------------------

#[test]
fn decode_frame_dispatches_all_seven_rpc_dispatch_bytes() {
    let request = RpcRequestPayload::unary("svc", 7, Bytes::from_static(b"body"));
    let frame = encode_request_frame(1, 2, 3, &request).expect("encodes");
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::Request(request.clone()))
    );

    let response = RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: vec![],
        body: Bytes::from_static(b"x"),
    };
    let frame = encode_response_frame(1, 2, 3, &response).expect("encodes");
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::Response {
            call_id: 2,
            payload: response
        })
    );

    let frame = encode_cancel_frame(1, 2, 3);
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::Cancel { call_id: 2 })
    );

    let frame = encode_deadline_exceeded_frame(1, 2, 3);
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::DeadlineExceeded { call_id: 2 })
    );

    let frame = encode_stream_grant_frame(1, 2, 3, 5);
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::StreamGrant {
            call_id: 2,
            credits: 5
        })
    );

    let chunk = RpcRequestChunkPayload {
        call_id: 2,
        flags: FLAG_RPC_REQUEST_END,
        headers: vec![],
        body: Bytes::from_static(b"c"),
    };
    let frame = encode_chunk_frame(1, 3, &chunk).expect("encodes");
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::RequestChunk(chunk))
    );

    let frame = encode_request_grant_frame(1, 2, 3, 5);
    assert_eq!(
        decode_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::RequestGrant(RpcRequestGrantPayload {
            call_id: 2,
            credits: 5
        }))
    );
}

#[test]
fn decode_frame_tolerates_unknown_dispatches_without_error() {
    for dispatch in [0x00, 0x0F, 0x17, 0x80, 0xFF] {
        let mut frame = EventMeta::new(dispatch, 0, 1, 2, 0).to_bytes().to_vec();
        frame.extend_from_slice(&3u64.to_le_bytes());
        frame.extend_from_slice(&[0xFF; 9]);
        assert_eq!(
            decode_frame(Bytes::from(frame)).expect("never an error"),
            None,
            "dispatch {dispatch:#04x} must be tolerated as unknown"
        );
    }
}

#[test]
fn decode_frame_names_the_exact_malformed_field() {
    assert_eq!(
        refuse(decode_frame(Bytes::from_static(&[0u8; 23]))),
        "frame shorter than 24"
    );

    // A payload-carrying dispatch without the route discriminator.
    let mut frame = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 1, 2, 0)
        .to_bytes()
        .to_vec();
    frame.extend_from_slice(&[0u8; 7]); // 31 bytes: one short
    assert_eq!(
        refuse(decode_frame(Bytes::from(frame))),
        "frame carries no route discriminator"
    );

    // Wrong-size grant payloads.
    let mut frame = encode_stream_grant_frame(1, 2, 3, 5);
    frame.push(0);
    assert_eq!(
        refuse(decode_frame(Bytes::from(frame))),
        "stream grant payload is not 4 bytes"
    );
    let mut frame = encode_request_grant_frame(1, 2, 3, 5);
    frame.push(0);
    assert_eq!(
        refuse(decode_frame(Bytes::from(frame))),
        "request grant payload is not 12 bytes"
    );

    // Truncated payloads name their missing field.
    let mut frame = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 1, 2, 0)
        .to_bytes()
        .to_vec();
    frame.extend_from_slice(&3u64.to_le_bytes()); // route, no payload
    assert_eq!(refuse(decode_frame(Bytes::from(frame))), "service length");

    let mut frame = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 1, 2, 0)
        .to_bytes()
        .to_vec();
    frame.extend_from_slice(&3u64.to_le_bytes());
    frame.push(0x00); // one byte cannot carry the status
    assert_eq!(refuse(decode_frame(Bytes::from(frame))), "status");

    let mut frame = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, 1, 2, 0)
        .to_bytes()
        .to_vec();
    frame.extend_from_slice(&3u64.to_le_bytes());
    frame.extend_from_slice(&[0u8; 7]); // short of the call_id
    assert_eq!(refuse(decode_frame(Bytes::from(frame))), "call_id");
}

#[test]
fn decode_reply_frame_surfaces_only_server_to_caller_dispatches() {
    // A caller → server (or unknown) dispatch is `Ok(None)` WITHOUT
    // inspecting its payload — the wrapper answers "is this for the
    // client half?" and nothing else.
    for dispatch in [
        DISPATCH_RPC_REQUEST,
        DISPATCH_RPC_CANCEL,
        DISPATCH_RPC_STREAM_GRANT,
        DISPATCH_RPC_REQUEST_CHUNK,
    ] {
        let mut frame = EventMeta::new(dispatch, 0, 1, 2, 0).to_bytes().to_vec();
        frame.extend_from_slice(&3u64.to_le_bytes());
        frame.push(0xFF); // deliberately undecodable payload
        assert_eq!(
            decode_reply_frame(Bytes::from(frame)).expect("never an error"),
            None,
            "dispatch {dispatch:#04x} is not the client half"
        );
    }

    // REQUEST_GRANT is server → caller and surfaces for the caller's
    // upload credit.
    let frame = encode_request_grant_frame(1, 2, 3, 5);
    assert_eq!(
        decode_reply_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::RequestGrant(RpcRequestGrantPayload {
            call_id: 2,
            credits: 5
        }))
    );

    // The documented leniency: a bare 24-byte DEADLINE_EXCEEDED needs
    // no route.
    let frame = EventMeta::new(DISPATCH_RPC_DEADLINE_EXCEEDED, 0, 0, 0x77, 0)
        .to_bytes()
        .to_vec();
    assert_eq!(
        decode_reply_frame(Bytes::from(frame)).expect("decodes"),
        Some(RpcFrame::DeadlineExceeded { call_id: 0x77 })
    );

    // Unknown dispatches are tolerated, never an error.
    let mut frame = EventMeta::new(0x80, 0, 1, 2, 0).to_bytes().to_vec();
    frame.extend_from_slice(&3u64.to_le_bytes());
    assert_eq!(
        decode_reply_frame(Bytes::from(frame)).expect("tolerated"),
        None
    );
}

// ---------------------------------------------------------------------------
// (d) stream_terminal_payload: every reason variant, byte for byte.
// ---------------------------------------------------------------------------

#[test]
fn stream_terminal_payload_maps_every_reason_byte_for_byte() {
    // Hand-rolled expected wire — never through the codec under test:
    // u16le status ‖ u8 header count ‖ [u8 name_len ‖ name ‖ u16le
    // value_len ‖ value] ‖ u32le body_len ‖ body.
    fn wire(status: u16, marker: Option<&[u8]>, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&status.to_le_bytes());
        match marker {
            None => out.push(0),
            Some(value) => {
                out.push(1); // one header
                out.push(14); // len("nrpc-streaming")
                out.extend_from_slice(b"nrpc-streaming");
                out.extend_from_slice(&(value.len() as u16).to_le_bytes());
                out.extend_from_slice(value);
            }
        }
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn assert_terminal(
        reason: StreamTerminalReason,
        status: RpcStatus,
        marker: Option<&[u8]>,
        body: &[u8],
    ) {
        let payload = stream_terminal_payload(&reason);
        let expected_headers: Vec<RpcHeader> = match marker {
            None => vec![],
            Some(value) => vec![("nrpc-streaming".to_string(), value.to_vec())],
        };
        assert_eq!(payload.status, status, "status for {reason:?}");
        assert_eq!(payload.headers, expected_headers, "headers for {reason:?}");
        assert_eq!(&payload.body[..], body, "body for {reason:?}");
        let mut buf = Vec::new();
        payload
            .encode_into(&mut buf)
            .expect("a terminal always encodes");
        assert_eq!(
            buf,
            wire(status.to_wire(), marker, body),
            "bytes for {reason:?}"
        );
    }

    assert_terminal(
        StreamTerminalReason::Completed(StreamHandlerResult::Ok),
        RpcStatus::Ok,
        Some(b"end"),
        b"",
    );
    assert_terminal(
        StreamTerminalReason::Completed(StreamHandlerResult::Err(
            RpcStatus::Internal,
            "boom".to_string(),
        )),
        RpcStatus::Internal,
        None,
        b"boom",
    );
    assert_terminal(
        StreamTerminalReason::Completed(StreamHandlerResult::Err(
            RpcStatus::Application(0x8001),
            "app".to_string(),
        )),
        RpcStatus::Application(0x8001),
        None,
        b"app",
    );
    assert_terminal(
        StreamTerminalReason::Cancelled,
        RpcStatus::Cancelled,
        None,
        b"server observed CANCEL during streaming handler execution",
    );
    assert_terminal(
        StreamTerminalReason::ServeHandleDropped,
        RpcStatus::Cancelled,
        None,
        b"server observed CANCEL during streaming handler execution",
    );
    assert_terminal(
        StreamTerminalReason::SessionReplaced,
        RpcStatus::Cancelled,
        None,
        b"peer session replaced",
    );
    assert_terminal(
        StreamTerminalReason::Timeout,
        RpcStatus::Timeout,
        None,
        b"stream deadline_ns exceeded",
    );
    assert_terminal(
        StreamTerminalReason::CredentialExpired,
        RpcStatus::AdmissionDenied,
        None,
        &[0], // coarse `Denied`
    );
    assert_terminal(
        StreamTerminalReason::Revoked,
        RpcStatus::AdmissionDenied,
        None,
        &[0], // coarse `Denied`
    );
    assert_terminal(
        StreamTerminalReason::AuthorityUnavailable,
        RpcStatus::AdmissionDenied,
        None,
        &[0], // coarse `Denied`
    );
    assert_terminal(
        StreamTerminalReason::ResourceExhausted,
        RpcStatus::AdmissionDenied,
        None,
        &[2], // coarse `Unavailable`
    );
    assert_terminal(
        StreamTerminalReason::PumpFailed,
        RpcStatus::Internal,
        None,
        b"response pump failed",
    );

    // Only a genuine completion drains queued output ("no synthetic
    // success").
    assert!(StreamTerminalReason::Completed(StreamHandlerResult::Ok).drains_queued_output());
    for retirement in [
        StreamTerminalReason::Cancelled,
        StreamTerminalReason::Timeout,
        StreamTerminalReason::CredentialExpired,
        StreamTerminalReason::Revoked,
        StreamTerminalReason::AuthorityUnavailable,
        StreamTerminalReason::ResourceExhausted,
        StreamTerminalReason::SessionReplaced,
        StreamTerminalReason::ServeHandleDropped,
        StreamTerminalReason::PumpFailed,
    ] {
        assert!(
            !retirement.drains_queued_output(),
            "{retirement:?} must discard queued output"
        );
    }
}

// ---------------------------------------------------------------------------
// (e) markers and constants: the exact names, values and bit assignments.
// ---------------------------------------------------------------------------

#[test]
fn classify_streaming_chunk_follows_the_marker_rules() {
    fn ok(headers: Vec<RpcHeader>) -> RpcResponsePayload {
        RpcResponsePayload {
            status: RpcStatus::Ok,
            headers,
            body: Bytes::new(),
        }
    }
    let marker =
        |name: &str, value: &[u8]| -> Vec<RpcHeader> { vec![(name.to_string(), value.to_vec())] };

    assert_eq!(
        classify_streaming_chunk(&ok(vec![])),
        StreamingChunkKind::Unary,
        "no marker = unary semantics"
    );
    assert_eq!(
        classify_streaming_chunk(&ok(marker("nrpc-streaming", b"continue"))),
        StreamingChunkKind::Continue
    );
    assert_eq!(
        classify_streaming_chunk(&ok(marker("nrpc-streaming", b"end"))),
        StreamingChunkKind::Terminal
    );
    assert_eq!(
        classify_streaming_chunk(&ok(marker("nrpc-streaming", b"END"))),
        StreamingChunkKind::Terminal,
        "marker VALUES are byte-exact: END is unknown, hence terminal"
    );
    assert_eq!(
        classify_streaming_chunk(&ok(marker("nrpc-streaming", b"banana"))),
        StreamingChunkKind::Terminal,
        "an unknown marker value is defensively terminal"
    );
    assert_eq!(
        classify_streaming_chunk(&ok(marker("NRPC-Streaming", b"continue"))),
        StreamingChunkKind::Continue,
        "§12.11: header NAMES compare ASCII-case-insensitively"
    );
    assert_eq!(
        classify_streaming_chunk(&RpcResponsePayload {
            status: RpcStatus::Internal,
            headers: marker("nrpc-streaming", b"continue"),
            body: Bytes::new(),
        }),
        StreamingChunkKind::Terminal,
        "a non-Ok status is terminal regardless of the marker"
    );
}

#[test]
fn the_streaming_constants_pin_the_core_names_and_values() {
    assert_eq!(DISPATCH_RPC_STREAM_GRANT, 0x14);
    assert_eq!(DISPATCH_RPC_REQUEST_CHUNK, 0x15);
    assert_eq!(DISPATCH_RPC_REQUEST_GRANT, 0x16);

    assert_eq!(FLAG_RPC_STREAMING_RESPONSE, 0b0000_0010);
    assert_eq!(FLAG_RPC_PROPAGATE_TRACE, 0b0000_0100);
    assert_eq!(FLAG_RPC_CLIENT_STREAMING_REQUEST, 0b0001_0000);
    assert_eq!(FLAG_RPC_REQUEST_END, 0b0010_0000);

    assert_eq!(HEADER_NRPC_STREAMING, "nrpc-streaming");
    assert_eq!(HEADER_NRPC_STREAMING_CONTINUE, b"continue");
    assert_eq!(HEADER_NRPC_STREAMING_END, b"end");
    assert_eq!(
        HEADER_NRPC_STREAM_WINDOW_INITIAL,
        "nrpc-stream-window-initial"
    );
    assert_eq!(
        HEADER_NRPC_REQUEST_WINDOW_INITIAL,
        "nrpc-request-window-initial"
    );
}
