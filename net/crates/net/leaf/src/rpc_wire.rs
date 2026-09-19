//! The nRPC frame codec a leaf **client** needs — a named second
//! copy, pinned from both directions.
//!
//! # Why this file exists, and what keeps it honest
//!
//! The production codec is the head of
//! `net/crates/net/src/adapter/net/cortex/rpc.rs` plus
//! `cortex/meta.rs`. Stage 5 tried to move it into `net-mesh-wire`
//! (plan §7 assigns the wire-level subprotocol codecs there) and
//! stopped: the codec is not a contiguous head. It is lines
//! 1..~1110 **plus** 1227..~1500, with a core-only region between
//! them (the mesh inbound per-channel dispatcher map), inside a
//! 7 756-line file, and `EventMeta` would have to move with it out
//! of `cortex/meta.rs`, which has seven-plus consumers across
//! `cortex/{adapter,watermark,memories,tasks,workflow}`. That
//! extraction is a clean follow-on slice; it is not a safe
//! mid-wave edit.
//!
//! So this is the **client half only** — what a leaf must be able to
//! write and read, and nothing else:
//!
//! | | here | not here |
//! |---|---|---|
//! | [`EventMeta`] | 24-byte prefix | the checksum-verifying read path |
//! | route | the 8-byte `RpcRouteV1` discriminator | the dispatcher map |
//! | request | encode | decode (a leaf serves nothing) |
//! | response | decode | encode |
//! | [`RpcStatus`] | both directions (it is 11 values) | — |
//! | streaming | — | chunks, grants, the four folds |
//!
//! **The pin, both directions.** A leaf-generated frame lives at
//! `net/crates/net/tests/cross_lang_wire/nrpc_frame.json`, and the
//! core's `cross_lang_wire` test decodes it with the production
//! `RpcRequestPayload::decode`, re-encodes it, and asserts the bytes
//! are identical — so a drift on either side reddens the other
//! side's suite. `tests/nrpc_frame_parity.rs` is this side of the
//! same fixture.
//!
//! Everything below is little-endian, which is the whole file's only
//! global rule.

use bytes::{Buf, BufMut, Bytes};

use crate::error::{LeafError, Result};

/// Size of the [`EventMeta`] prefix on every nRPC frame.
pub const EVENT_META_SIZE: usize = 24;

/// Size of the `RpcRouteV1` discriminator: one canonical `u64`
/// channel hash.
pub const RPC_ROUTE_V1_SIZE: usize = 8;

/// Byte offset of the frame-specific payload: past the `EventMeta`
/// prefix AND the route discriminator.
pub const RPC_FRAME_BODY_OFFSET: usize = EVENT_META_SIZE + RPC_ROUTE_V1_SIZE;

/// Caller → server: the first frame of a call. `seq_or_ts` is the
/// caller-generated `call_id`.
pub const DISPATCH_RPC_REQUEST: u8 = 0x10;

/// Server → caller: the terminal frame of a unary call.
pub const DISPATCH_RPC_RESPONSE: u8 = 0x11;

/// Caller → server: cancellation. Empty payload — the dispatch byte
/// plus the matching `call_id` is the whole signal.
pub const DISPATCH_RPC_CANCEL: u8 = 0x12;

/// Server → caller: the server saw the deadline pass before starting
/// work. Empty payload.
pub const DISPATCH_RPC_DEADLINE_EXCEEDED: u8 = 0x13;

/// Maximum service-name length on the wire.
pub const MAX_RPC_SERVICE_NAME_LEN: usize = 255;
/// Maximum headers in one frame.
pub const MAX_RPC_HEADERS: usize = 32;
/// Maximum length of one header name.
pub const MAX_RPC_HEADER_NAME_LEN: usize = 64;
/// Maximum length of one header value.
pub const MAX_RPC_HEADER_VALUE_LEN: usize = 4096;
/// Maximum request or response body length.
pub const MAX_RPC_BODY_LEN: usize = 4 * 1024 * 1024;

/// A header name/value pair. Names are case-sensitive UTF-8; values
/// are opaque bytes.
pub type RpcHeader = (String, Vec<u8>);

/// The 24-byte prefix every nRPC frame starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventMeta {
    /// Event classifier — one of the `DISPATCH_RPC_*` bytes.
    pub dispatch: u8,
    /// `FLAG_RPC_*` bits. A leaf client writes 0: unary only.
    pub flags: u8,
    /// The AEAD-verified caller's origin hash.
    pub origin_hash: u64,
    /// The correlation id — the `call_id` for every RPC dispatch.
    pub seq_or_ts: u64,
    /// Corruption-detection checksum over the frame. Zero on the
    /// mesh path, which is what the production publish site writes:
    /// the value protects RedEX *on-disk* records, and a mesh frame
    /// is already covered by the packet AEAD.
    pub checksum: u32,
}

impl EventMeta {
    /// Build a prefix with zeroed pad bytes.
    pub fn new(dispatch: u8, flags: u8, origin_hash: u64, seq_or_ts: u64, checksum: u32) -> Self {
        Self {
            dispatch,
            flags,
            origin_hash,
            seq_or_ts,
            checksum,
        }
    }

    /// Encode to the 24-byte wire form. Bytes 2..4 are the reserved
    /// pad: zero on write, ignored on read.
    pub fn to_bytes(&self) -> [u8; EVENT_META_SIZE] {
        let mut out = [0u8; EVENT_META_SIZE];
        out[0] = self.dispatch;
        out[1] = self.flags;
        out[4..12].copy_from_slice(&self.origin_hash.to_le_bytes());
        out[12..20].copy_from_slice(&self.seq_or_ts.to_le_bytes());
        out[20..24].copy_from_slice(&self.checksum.to_le_bytes());
        out
    }

    /// Decode from a slice at least [`EVENT_META_SIZE`] long.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < EVENT_META_SIZE {
            return None;
        }
        Some(Self {
            dispatch: bytes[0],
            flags: bytes[1],
            origin_hash: u64::from_le_bytes(bytes[4..12].try_into().ok()?),
            seq_or_ts: u64::from_le_bytes(bytes[12..20].try_into().ok()?),
            checksum: u32::from_le_bytes(bytes[20..24].try_into().ok()?),
        })
    }
}

/// Outcome of a call. Net-native numbering; `0x8000..=0xFFFF` is the
/// application-defined range, and reserved values in
/// `0x000A..=0x7FFF` decode as [`RpcStatus::Application`] rather
/// than failing — forward compatibility with future assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcStatus {
    /// Success.
    Ok,
    /// No service of that name on the server.
    NotFound,
    /// Channel-auth or token-scope refusal.
    Unauthorized,
    /// The server saw the deadline pass before starting work.
    Timeout,
    /// The server's per-service queue is full.
    Backpressure,
    /// The caller cancelled.
    Cancelled,
    /// The handler failed. The body carries a UTF-8 diagnostic.
    Internal,
    /// The request payload version is unsupported.
    UnknownVersion,
    /// v0.4 capability-auth refusal.
    CapabilityDenied,
    /// Org admission refusal (the §12 gate a provisional leaf meets).
    AdmissionDenied,
    /// Application-defined status.
    Application(u16),
}

impl RpcStatus {
    /// Encode to the wire `u16`.
    pub fn to_wire(self) -> u16 {
        match self {
            Self::Ok => 0x0000,
            Self::NotFound => 0x0001,
            Self::Unauthorized => 0x0002,
            Self::Timeout => 0x0003,
            Self::Backpressure => 0x0004,
            Self::Cancelled => 0x0005,
            Self::Internal => 0x0006,
            Self::UnknownVersion => 0x0007,
            Self::CapabilityDenied => 0x0008,
            Self::AdmissionDenied => 0x0009,
            Self::Application(v) => v,
        }
    }

    /// Decode from the wire `u16`.
    pub fn from_wire(v: u16) -> Self {
        match v {
            0x0000 => Self::Ok,
            0x0001 => Self::NotFound,
            0x0002 => Self::Unauthorized,
            0x0003 => Self::Timeout,
            0x0004 => Self::Backpressure,
            0x0005 => Self::Cancelled,
            0x0006 => Self::Internal,
            0x0007 => Self::UnknownVersion,
            0x0008 => Self::CapabilityDenied,
            0x0009 => Self::AdmissionDenied,
            other => Self::Application(other),
        }
    }

    /// True iff `Ok`.
    #[inline]
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// A request, as the leaf writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequestPayload {
    /// Service-name dispatch key.
    pub service: String,
    /// Absolute deadline in unix nanos; `0` means none.
    pub deadline_ns: u64,
    /// `FLAG_RPC_*` bits.
    pub flags: u16,
    /// Headers.
    pub headers: Vec<RpcHeader>,
    /// The application body.
    pub body: Bytes,
}

impl RpcRequestPayload {
    /// A unary request with no headers.
    pub fn unary(service: impl Into<String>, deadline_ns: u64, body: Bytes) -> Self {
        Self {
            service: service.into(),
            deadline_ns,
            flags: 0,
            headers: Vec::new(),
            body,
        }
    }

    /// Encoded length, without encoding.
    pub fn encoded_len(&self) -> usize {
        1 + self.service.len() + 8 + 2 + headers_len(&self.headers) + 4 + self.body.len()
    }

    /// Refuse anything the `u8` / `u16` / `u32` length prefixes would
    /// truncate.
    ///
    /// The production encoder `debug_assert`s these and truncates in
    /// release; a leaf refuses instead, because the caller is an
    /// application and an ambiguous encoding is not a programmer bug
    /// it can see.
    pub fn validate(&self) -> Result<()> {
        check("service", self.service.len(), MAX_RPC_SERVICE_NAME_LEN)?;
        check("headers", self.headers.len(), MAX_RPC_HEADERS)?;
        for (name, value) in &self.headers {
            check("header name", name.len(), MAX_RPC_HEADER_NAME_LEN)?;
            check("header value", value.len(), MAX_RPC_HEADER_VALUE_LEN)?;
        }
        check("body", self.body.len(), MAX_RPC_BODY_LEN)?;
        Ok(())
    }

    /// Append the wire bytes that follow the `EventMeta` prefix and
    /// the route discriminator.
    pub fn encode_into(&self, buf: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let svc = self.service.as_bytes();
        buf.put_u8(svc.len() as u8);
        buf.extend_from_slice(svc);
        buf.put_u64_le(self.deadline_ns);
        buf.put_u16_le(self.flags);
        encode_headers(&self.headers, buf);
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
        Ok(())
    }
}

/// A response, as the leaf reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcResponsePayload {
    /// The call's outcome.
    pub status: RpcStatus,
    /// Headers.
    pub headers: Vec<RpcHeader>,
    /// The application body, or a UTF-8 diagnostic for a non-`Ok`
    /// status.
    pub body: Bytes,
}

impl RpcResponsePayload {
    /// Decode the bytes that follow the `EventMeta` prefix and the
    /// route discriminator.
    pub fn decode(data: Bytes) -> Result<Self> {
        let mut cur = std::io::Cursor::new(data.as_ref());
        if cur.remaining() < 2 {
            return Err(malformed("status"));
        }
        let status = RpcStatus::from_wire(cur.get_u16_le());
        let headers = decode_headers(&mut cur)?;
        if cur.remaining() < 4 {
            return Err(malformed("body length"));
        }
        let body_len = cur.get_u32_le() as usize;
        if body_len > MAX_RPC_BODY_LEN {
            return Err(malformed("body exceeds MAX_RPC_BODY_LEN"));
        }
        if cur.remaining() < body_len {
            return Err(malformed("body bytes"));
        }
        let start = cur.position() as usize;
        Ok(Self {
            status,
            headers,
            // `slice`, not a copy: the body is the common case and
            // the largest field.
            body: data.slice(start..start + body_len),
        })
    }
}

/// Build a complete outbound REQUEST frame:
/// `EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload`.
///
/// The byte layout `MeshNode::publish_rpc_request_unsubscribed`
/// publishes, which is what the 4b harness built natively and handed
/// to the page. `route` is the **canonical `u64`** hash of the
/// physical channel the frame rides (`<service>.requests` in the
/// caller direction), not the `u16` wire bucket.
pub fn encode_request_frame(
    origin_hash: u64,
    call_id: u64,
    route: u64,
    payload: &RpcRequestPayload,
) -> Result<Vec<u8>> {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + payload.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    payload.encode_into(&mut buf)?;
    Ok(buf)
}

/// Build a CANCEL frame for `call_id`. Empty payload by contract.
pub fn encode_cancel_frame(origin_hash: u64, call_id: u64, route: u64) -> Vec<u8> {
    let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET);
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    buf
}

/// One decoded inbound nRPC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcFrame {
    /// A terminal reply for `call_id`.
    Response {
        /// Correlation id, from `EventMeta::seq_or_ts`.
        call_id: u64,
        /// The decoded reply.
        payload: RpcResponsePayload,
    },
    /// The server gave up on `call_id`'s deadline. No payload.
    DeadlineExceeded {
        /// Correlation id.
        call_id: u64,
    },
}

/// Decode an inbound event-plane frame if it is one of the two
/// server → caller dispatches a leaf client understands.
///
/// `Ok(None)` is a frame that is well-formed but not for the client
/// half — a REQUEST or CANCEL, which a leaf never serves. The caller
/// counts that as an unknown dispatch rather than an error.
pub fn decode_reply_frame(frame: Bytes) -> Result<Option<RpcFrame>> {
    let meta = EventMeta::from_bytes(&frame).ok_or_else(|| malformed("frame shorter than 24"))?;
    match meta.dispatch {
        DISPATCH_RPC_RESPONSE => {
            if frame.len() < RPC_FRAME_BODY_OFFSET {
                return Err(malformed("frame carries no route discriminator"));
            }
            Ok(Some(RpcFrame::Response {
                call_id: meta.seq_or_ts,
                payload: RpcResponsePayload::decode(frame.slice(RPC_FRAME_BODY_OFFSET..))?,
            }))
        }
        DISPATCH_RPC_DEADLINE_EXCEEDED => Ok(Some(RpcFrame::DeadlineExceeded {
            call_id: meta.seq_or_ts,
        })),
        _ => Ok(None),
    }
}

/// The `RpcRouteV1` discriminator of a frame, when it carries one.
pub fn decode_route(frame: &[u8]) -> Option<u64> {
    let bytes = frame.get(EVENT_META_SIZE..RPC_FRAME_BODY_OFFSET)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn headers_len(headers: &[RpcHeader]) -> usize {
    1 + headers
        .iter()
        .map(|(n, v)| 1 + n.len() + 2 + v.len())
        .sum::<usize>()
}

fn encode_headers(headers: &[RpcHeader], buf: &mut Vec<u8>) {
    buf.put_u8(headers.len() as u8);
    for (name, value) in headers {
        let nbytes = name.as_bytes();
        buf.put_u8(nbytes.len() as u8);
        buf.extend_from_slice(nbytes);
        buf.put_u16_le(value.len() as u16);
        buf.extend_from_slice(value);
    }
}

fn decode_headers(cur: &mut std::io::Cursor<&[u8]>) -> Result<Vec<RpcHeader>> {
    if cur.remaining() < 1 {
        return Err(malformed("header count"));
    }
    let count = cur.get_u8() as usize;
    if count > MAX_RPC_HEADERS {
        return Err(malformed("header count exceeds MAX_RPC_HEADERS"));
    }
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        if cur.remaining() < 1 {
            return Err(malformed("header name length"));
        }
        let name_len = cur.get_u8() as usize;
        if name_len > MAX_RPC_HEADER_NAME_LEN {
            return Err(malformed("header name exceeds MAX_RPC_HEADER_NAME_LEN"));
        }
        if cur.remaining() < name_len {
            return Err(malformed("header name bytes"));
        }
        let mut name = vec![0u8; name_len];
        cur.copy_to_slice(&mut name);
        let name = String::from_utf8(name).map_err(|_| malformed("header name is not UTF-8"))?;
        if cur.remaining() < 2 {
            return Err(malformed("header value length"));
        }
        let value_len = cur.get_u16_le() as usize;
        if value_len > MAX_RPC_HEADER_VALUE_LEN {
            return Err(malformed("header value exceeds MAX_RPC_HEADER_VALUE_LEN"));
        }
        if cur.remaining() < value_len {
            return Err(malformed("header value bytes"));
        }
        let mut value = vec![0u8; value_len];
        cur.copy_to_slice(&mut value);
        headers.push((name, value));
    }
    Ok(headers)
}

fn check(field: &str, actual: usize, limit: usize) -> Result<()> {
    if actual > limit {
        return Err(LeafError::Rpc(crate::error::RpcError::Malformed(format!(
            "{field} is {actual} bytes, over the {limit}-byte wire limit"
        ))));
    }
    Ok(())
}

fn malformed(what: &str) -> LeafError {
    LeafError::Rpc(crate::error::RpcError::Malformed(what.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_meta_layout_is_the_pinned_twenty_four_bytes() {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 0x0807_0605_0403_0201, 0x11, 0);
        let bytes = meta.to_bytes();
        assert_eq!(bytes.len(), 24);
        assert_eq!(bytes[0], 0x10, "dispatch");
        assert_eq!(bytes[1], 0);
        assert_eq!(&bytes[2..4], &[0, 0], "the pad is zero on write");
        assert_eq!(&bytes[4..12], &0x0807_0605_0403_0201u64.to_le_bytes());
        assert_eq!(&bytes[12..20], &0x11u64.to_le_bytes());
        assert_eq!(&bytes[20..24], &[0, 0, 0, 0]);
        assert_eq!(EventMeta::from_bytes(&bytes), Some(meta));
        assert_eq!(EventMeta::from_bytes(&bytes[..23]), None);
    }

    #[test]
    fn a_nonzero_pad_is_ignored_on_read() {
        let meta = EventMeta::new(0x11, 0, 1, 2, 3);
        let mut bytes = meta.to_bytes();
        bytes[2] = 0xFF;
        bytes[3] = 0xFF;
        assert_eq!(
            EventMeta::from_bytes(&bytes),
            Some(meta),
            "the reserved pad is 'zero on write, ignored on read'"
        );
    }

    #[test]
    fn a_request_frame_has_the_meta_route_payload_layout() {
        let payload = RpcRequestPayload::unary(
            "net.enrollment.join",
            0,
            Bytes::from_static(b"request body"),
        );
        let frame =
            encode_request_frame(0xAAAA, 0x1234, 0xCAFE_F00D_DEAD_BEEF, &payload).expect("encode");

        assert_eq!(&frame[..2], &[DISPATCH_RPC_REQUEST, 0]);
        assert_eq!(
            decode_route(&frame),
            Some(0xCAFE_F00D_DEAD_BEEF),
            "the route discriminator must be the canonical u64, LE"
        );
        let meta = EventMeta::from_bytes(&frame).expect("meta");
        assert_eq!(meta.seq_or_ts, 0x1234, "call_id rides EventMeta::seq_or_ts");
        assert_eq!(meta.origin_hash, 0xAAAA);

        // The payload region decodes field by field.
        let body = &frame[RPC_FRAME_BODY_OFFSET..];
        assert_eq!(body[0] as usize, "net.enrollment.join".len());
        assert_eq!(
            &body[1..1 + 19],
            b"net.enrollment.join",
            "the service name is length-prefixed UTF-8"
        );
        assert_eq!(frame.len(), RPC_FRAME_BODY_OFFSET + payload.encoded_len());
    }

    #[test]
    fn a_response_frame_round_trips_through_the_client_decoder() {
        // Encode a response the way a server would, then decode it
        // the way the leaf does.
        let mut frame = Vec::new();
        frame.extend_from_slice(&EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 7, 0x99, 0).to_bytes());
        frame.extend_from_slice(&0x1111u64.to_le_bytes());
        frame.put_u16_le(RpcStatus::Ok.to_wire());
        encode_headers(
            &[("content-type".into(), b"application/json".to_vec())],
            &mut frame,
        );
        frame.put_u32_le(4);
        frame.extend_from_slice(b"NMO1");

        let decoded = decode_reply_frame(Bytes::from(frame))
            .expect("decodes")
            .expect("is a client-half dispatch");
        match decoded {
            RpcFrame::Response { call_id, payload } => {
                assert_eq!(call_id, 0x99);
                assert!(payload.status.is_ok());
                assert_eq!(payload.body, Bytes::from_static(b"NMO1"));
                assert_eq!(
                    payload.headers,
                    vec![("content-type".to_string(), b"application/json".to_vec())]
                );
            }
            other => panic!("expected a Response, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_status_survives_the_round_trip_with_its_diagnostic() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 7, 5, 0).to_bytes());
        frame.extend_from_slice(&0u64.to_le_bytes());
        frame.put_u16_le(RpcStatus::AdmissionDenied.to_wire());
        frame.put_u8(0);
        frame.put_u32_le(6);
        frame.extend_from_slice(b"denied");

        let Some(RpcFrame::Response { payload, .. }) =
            decode_reply_frame(Bytes::from(frame)).expect("decodes")
        else {
            panic!("expected a Response");
        };
        assert_eq!(payload.status, RpcStatus::AdmissionDenied);
        assert_eq!(&payload.body[..], b"denied");
    }

    #[test]
    fn a_deadline_exceeded_frame_needs_no_payload() {
        let frame = EventMeta::new(DISPATCH_RPC_DEADLINE_EXCEEDED, 0, 0, 0x77, 0).to_bytes();
        assert_eq!(
            decode_reply_frame(Bytes::copy_from_slice(&frame)).expect("decodes"),
            Some(RpcFrame::DeadlineExceeded { call_id: 0x77 })
        );
    }

    #[test]
    fn a_request_or_cancel_dispatch_is_not_a_client_frame() {
        for dispatch in [DISPATCH_RPC_REQUEST, DISPATCH_RPC_CANCEL, 0x80] {
            let mut frame = EventMeta::new(dispatch, 0, 0, 1, 0).to_bytes().to_vec();
            frame.extend_from_slice(&0u64.to_le_bytes());
            assert_eq!(
                decode_reply_frame(Bytes::from(frame)).expect("well-formed"),
                None,
                "dispatch {dispatch:#04x} is not the client half"
            );
        }
    }

    #[test]
    fn every_status_round_trips_and_reserved_values_decode_as_application() {
        for status in [
            RpcStatus::Ok,
            RpcStatus::NotFound,
            RpcStatus::Unauthorized,
            RpcStatus::Timeout,
            RpcStatus::Backpressure,
            RpcStatus::Cancelled,
            RpcStatus::Internal,
            RpcStatus::UnknownVersion,
            RpcStatus::CapabilityDenied,
            RpcStatus::AdmissionDenied,
            RpcStatus::Application(0x8001),
        ] {
            assert_eq!(RpcStatus::from_wire(status.to_wire()), status);
        }
        assert_eq!(
            RpcStatus::from_wire(0x000A),
            RpcStatus::Application(0x000A),
            "a reserved value must decode forward-compatibly, not fail"
        );
    }

    #[test]
    fn an_over_cap_field_is_refused_rather_than_truncated() {
        let long = "s".repeat(MAX_RPC_SERVICE_NAME_LEN + 1);
        let payload = RpcRequestPayload::unary(long, 0, Bytes::new());
        let err = payload.validate().expect_err("must refuse");
        assert!(
            format!("{err}").contains("service"),
            "the error must name the field: {err}"
        );
        assert!(encode_request_frame(0, 0, 0, &payload).is_err());

        let mut headers = Vec::new();
        for i in 0..=MAX_RPC_HEADERS {
            headers.push((format!("h{i}"), Vec::new()));
        }
        let mut payload = RpcRequestPayload::unary("svc", 0, Bytes::new());
        payload.headers = headers;
        assert!(payload.validate().is_err(), "over-cap header count");
    }

    #[test]
    fn a_truncated_response_is_an_error_not_a_partial_decode() {
        for len in 0..8 {
            let mut frame = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 0, 1, 0)
                .to_bytes()
                .to_vec();
            frame.extend_from_slice(&0u64.to_le_bytes());
            frame.extend_from_slice(&vec![0u8; len]);
            // 0 bytes of payload => no status; 1 => no status; 3 =>
            // status but no header count; and so on. Every one of
            // them must error rather than invent a field.
            if len >= 7 {
                continue;
            }
            assert!(
                decode_reply_frame(Bytes::from(frame)).is_err(),
                "a {len}-byte payload must not decode"
            );
        }
    }
}
