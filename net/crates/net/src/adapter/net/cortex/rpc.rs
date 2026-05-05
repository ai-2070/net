//! nRPC — request/response on top of CortEX folds.
//!
//! See `docs/misc/NRPC_DESIGN.md` for the full architectural framing.
//! In short: an RPC server is a `RedexFold` whose state is the
//! in-flight call set, whose events are typed `(REQUEST, RESPONSE,
//! CANCEL, DEADLINE_EXCEEDED)`, whose `EventMeta::seq_or_ts` is the
//! correlation id, and whose `EventMeta::origin_hash` is the
//! AEAD-verified caller. The mesh-channel layer's queue-group
//! subscription mode (see `channel::SubscriptionMode`) does the
//! one-of-N work distribution across replica servers.
//!
//! This module is the **wire codec layer**: dispatch constants for
//! `EventMeta::dispatch`, payload structs for `RpcRequestPayload` /
//! `RpcResponsePayload`, and the `RpcStatus` enumeration. The fold
//! types and the `Mesh::serve_rpc` / `Mesh::call` glue layer build
//! on top.

use bytes::{Buf, BufMut};

use super::meta::EVENT_META_SIZE;

// ============================================================================
// `EventMeta::dispatch` byte assignments for nRPC.
//
// All four values live in the cortex-internal range (`0x00..0x7F`).
// Application/vendor dispatches stay in `0x80..0xFF`. Adapters that
// don't care about RPC ignore unknown dispatches as they ignore any
// other.
// ============================================================================

/// Caller → server. The first frame of an RPC call. `EventMeta::seq_or_ts`
/// is the caller-generated `call_id`; `EventMeta::origin_hash` is the
/// AEAD-verified caller. Payload is an [`RpcRequestPayload`].
pub const DISPATCH_RPC_REQUEST: u8 = 0x10;

/// Server → caller. The (terminal, for unary) frame of an RPC call.
/// `EventMeta::seq_or_ts` matches the request's `call_id`. Payload is
/// an [`RpcResponsePayload`].
pub const DISPATCH_RPC_RESPONSE: u8 = 0x11;

/// Caller → server. Cancellation signal. `EventMeta::seq_or_ts` matches
/// the request's `call_id`. Empty payload — the dispatch byte plus
/// the matching `call_id` is the whole signal. Server's fold removes
/// the in-flight entry and (if cooperative) flips the handler's
/// `CancellationToken`.
pub const DISPATCH_RPC_CANCEL: u8 = 0x12;

/// Server → caller. Deadline-exceeded signal. Emitted when the
/// server's fold sees `now_ns() > request.deadline_ns` before
/// starting the handler (or, optionally, when a long-running handler
/// is aborted by the deadline timer). `EventMeta::seq_or_ts` matches
/// the request's `call_id`. Empty payload.
pub const DISPATCH_RPC_DEADLINE_EXCEEDED: u8 = 0x13;

// ============================================================================
// `RpcRequestPayload::flags` bit assignments.
// ============================================================================

/// Set if the request is safe to retry. Server may dedup against the
/// `(origin_hash, call_id)` pair within its idempotency window;
/// replay returns the cached response without re-running the handler.
/// **Caller's contract**: a request marked `IDEMPOTENT` whose
/// (origin_hash, call_id) reappears must produce a byte-equivalent
/// response when re-folded. Application code is responsible for
/// honoring this.
pub const FLAG_RPC_IDEMPOTENT: u16 = 1 << 0;

/// Set if the server may emit multiple `DISPATCH_RPC_RESPONSE` events
/// for this call. Without it, the first response terminates the
/// call. With it, each response except the terminal one carries
/// `headers["nrpc-streaming"] = b"continue"`; the terminal response
/// has either `b"end"` (success) or a non-`Ok` status. Phase 3.
pub const FLAG_RPC_STREAMING_RESPONSE: u16 = 1 << 1;

/// Set if the request carries W3C Trace Context headers
/// (`traceparent`, `tracestate`). Server propagates them to its own
/// span emission. Phase 3.
pub const FLAG_RPC_PROPAGATE_TRACE: u16 = 1 << 2;

// Bits `3..=15` reserved; producers MUST write zero, consumers MUST
// ignore unknown bits (forward-compat with future flags).

// ============================================================================
// `RpcResponsePayload::status` enumeration.
// ============================================================================

/// Outcome of an nRPC call. Net-native numbering with documented
/// gRPC equivalents (see comments). Numeric stability: callers and
/// servers across versions agree on `0x0000..=0x7FFF`; the
/// application-defined range is `0x8000..=0xFFFF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum RpcStatus {
    /// Success. Payload carries the application response. Terminal
    /// (or, for streaming responses, may be one of many — see the
    /// streaming flag).
    /// gRPC equivalent: `OK` (0).
    Ok = 0x0000,
    /// No service registered with the requested name on the server.
    /// gRPC equivalent: `NOT_FOUND` (5).
    NotFound = 0x0001,
    /// Caller's token doesn't list the requested service in scope,
    /// or the channel-level capability check failed.
    /// gRPC equivalent: `PERMISSION_DENIED` (7).
    Unauthorized = 0x0002,
    /// Server observed `now_ns() > deadline_ns` before starting work.
    /// (For the in-flight case after the handler started, see
    /// [`DISPATCH_RPC_DEADLINE_EXCEEDED`].)
    /// gRPC equivalent: `DEADLINE_EXCEEDED` (4).
    Timeout = 0x0003,
    /// Server's per-service queue is at `max_in_flight` capacity.
    /// gRPC equivalent: `RESOURCE_EXHAUSTED` (8).
    Backpressure = 0x0004,
    /// Caller emitted `DISPATCH_RPC_CANCEL` before the server
    /// completed.
    /// gRPC equivalent: `CANCELLED` (1).
    Cancelled = 0x0005,
    /// Handler panicked or returned an error not classified as one
    /// of the above. Payload carries a UTF-8 diagnostic.
    /// gRPC equivalent: `INTERNAL` (13).
    Internal = 0x0006,
    /// Request payload version not supported by the server. Should
    /// normally be caught earlier by subprotocol-version
    /// negotiation; the in-payload guard is the floor.
    /// gRPC equivalent: `UNIMPLEMENTED` (12).
    UnknownVersion = 0x0007,
    /// Application-defined status. The wire carries the raw u16;
    /// callers / servers agree on the meaning out of band.
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
            Self::Application(v) => v,
        }
    }

    /// Decode from the wire `u16`. Reserved values
    /// (`0x0008..=0x7FFF`) decode as `Application(v)` rather than
    /// failing — forward-compat with future status assignments.
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
            other => Self::Application(other),
        }
    }

    /// True iff `self == Ok`. Convenience for the hot caller-side
    /// success-or-error branch.
    #[inline]
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

// ============================================================================
// Request / response payloads.
//
// These ride in the bytes AFTER the 24-byte `EventMeta` prefix on a
// CortEX-adapted event. The cortex adapter handles meta + tail
// concatenation; this codec produces only the tail.
// ============================================================================

/// Header name + value pair. Used for trace-context propagation,
/// idempotency-key carriage, content-type hints. Names are
/// case-sensitive UTF-8; values are opaque bytes.
pub type RpcHeader = (String, Vec<u8>);

/// Maximum service-name length on the wire (matches
/// `MAX_CHANNEL_NAME_LEN` upstream; reasonable upper bound for a
/// human-readable identifier).
pub const MAX_RPC_SERVICE_NAME_LEN: usize = 255;

/// Maximum number of headers in a single request or response.
/// Prevents pathological `headers.len()` reads from a malformed
/// peer; legitimate callers stay well below this.
pub const MAX_RPC_HEADERS: usize = 32;

/// Maximum length of a single header name (UTF-8 bytes).
pub const MAX_RPC_HEADER_NAME_LEN: usize = 64;

/// Maximum length of a single header value (bytes).
pub const MAX_RPC_HEADER_VALUE_LEN: usize = 4096;

/// Maximum length of a request or response body. Larger payloads
/// must use streaming responses (Phase 3) or chunk at the
/// application layer. Comparable to gRPC's default `max_message_size`
/// of 4 MiB; tuned downward to match RedEX's
/// `MAX_REDEX_HEAP_PAYLOAD` ceiling.
pub const MAX_RPC_BODY_LEN: usize = 4 * 1024 * 1024;

/// nRPC request payload. Lives after the 24-byte `EventMeta` prefix
/// in a `DISPATCH_RPC_REQUEST` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequestPayload {
    /// Service-name dispatch key. The server's fold looks this up
    /// in its `serve_rpc` registry and routes to the registered
    /// handler.
    pub service: String,
    /// Absolute deadline (unix nanos). `0` means no deadline; the
    /// caller will cancel via `DISPATCH_RPC_CANCEL` if it changes
    /// its mind.
    pub deadline_ns: u64,
    /// Bitfield of `FLAG_RPC_*` constants.
    pub flags: u16,
    /// Headers (trace context, idempotency key, content-type, etc.).
    /// Capped at `MAX_RPC_HEADERS` entries, name <= `MAX_RPC_HEADER_NAME_LEN`,
    /// value <= `MAX_RPC_HEADER_VALUE_LEN`.
    pub headers: Vec<RpcHeader>,
    /// Application-defined request body. Caller and server agree on
    /// the codec out-of-band; nRPC doesn't interpret these bytes.
    pub body: Vec<u8>,
}

/// nRPC response payload. Lives after the 24-byte `EventMeta`
/// prefix in a `DISPATCH_RPC_RESPONSE` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcResponsePayload {
    /// Outcome of the call. Decoded on the caller side via
    /// [`RpcStatus::from_wire`].
    pub status: RpcStatus,
    /// Headers (trace context, content-type, content-encoding,
    /// etc.). Same caps as `RpcRequestPayload::headers`.
    pub headers: Vec<RpcHeader>,
    /// For `status == Ok`: the application response body.
    /// For non-`Ok` statuses: a UTF-8 diagnostic string (callers
    /// `String::from_utf8_lossy` for display; the bytes are not
    /// guaranteed to be valid UTF-8 against a malicious server).
    pub body: Vec<u8>,
}

// ============================================================================
// Codec.
//
// All wire integers are little-endian. Lengths are u32_le where the
// upper bound exceeds u16, u16_le where it fits, u8 where it fits.
// ============================================================================

/// Errors from the request / response codecs.
#[derive(Debug, thiserror::Error)]
pub enum RpcCodecError {
    /// Buffer ended mid-field.
    #[error("truncated payload at {0}")]
    Truncated(&'static str),
    /// Length prefix exceeds the configured maximum.
    #[error("length {actual} exceeds limit {limit} for {field}")]
    TooLarge {
        /// Field name whose declared length exceeded the cap (e.g.
        /// `"body"`, `"headers"`, `"service"`). Stable strings —
        /// callers may match on them for diagnostics.
        field: &'static str,
        /// The length the wire claimed for the field.
        actual: usize,
        /// The maximum the codec accepts (one of the `MAX_RPC_*`
        /// constants).
        limit: usize,
    },
    /// String field contains non-UTF-8 bytes.
    #[error("non-utf8 string in {0}")]
    InvalidUtf8(&'static str),
}

impl RpcRequestPayload {
    /// Encode to the wire format. The result is the bytes that
    /// follow the 24-byte `EventMeta` prefix in the RedEX payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 + self.body.len());
        // service
        let svc = self.service.as_bytes();
        buf.put_u8(svc.len() as u8);
        buf.extend_from_slice(svc);
        // deadline_ns
        buf.put_u64_le(self.deadline_ns);
        // flags
        buf.put_u16_le(self.flags);
        // headers
        encode_headers(&self.headers, &mut buf);
        // body
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
        buf
    }

    /// Decode from the wire bytes following the `EventMeta` prefix.
    /// All length fields are bounded by the `MAX_RPC_*` constants;
    /// over-cap inputs error rather than allocate unbounded
    /// buffers.
    pub fn decode(data: &[u8]) -> Result<Self, RpcCodecError> {
        let mut cur = std::io::Cursor::new(data);
        // service
        if cur.remaining() < 1 {
            return Err(RpcCodecError::Truncated("service length"));
        }
        let svc_len = cur.get_u8() as usize;
        if svc_len == 0 {
            return Err(RpcCodecError::Truncated("empty service name"));
        }
        if svc_len > MAX_RPC_SERVICE_NAME_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "service",
                actual: svc_len,
                limit: MAX_RPC_SERVICE_NAME_LEN,
            });
        }
        if cur.remaining() < svc_len {
            return Err(RpcCodecError::Truncated("service bytes"));
        }
        let svc_start = cur.position() as usize;
        let svc_end = svc_start + svc_len;
        let service = std::str::from_utf8(&data[svc_start..svc_end])
            .map_err(|_| RpcCodecError::InvalidUtf8("service"))?
            .to_string();
        cur.set_position(svc_end as u64);
        // deadline_ns
        if cur.remaining() < 8 {
            return Err(RpcCodecError::Truncated("deadline_ns"));
        }
        let deadline_ns = cur.get_u64_le();
        // flags
        if cur.remaining() < 2 {
            return Err(RpcCodecError::Truncated("flags"));
        }
        let flags = cur.get_u16_le();
        // headers
        let headers = decode_headers(&mut cur, data)?;
        // body
        if cur.remaining() < 4 {
            return Err(RpcCodecError::Truncated("body length"));
        }
        let body_len = cur.get_u32_le() as usize;
        if body_len > MAX_RPC_BODY_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "body",
                actual: body_len,
                limit: MAX_RPC_BODY_LEN,
            });
        }
        if cur.remaining() < body_len {
            return Err(RpcCodecError::Truncated("body bytes"));
        }
        let body_start = cur.position() as usize;
        let body_end = body_start + body_len;
        let body = data[body_start..body_end].to_vec();
        Ok(Self {
            service,
            deadline_ns,
            flags,
            headers,
            body,
        })
    }
}

impl RpcResponsePayload {
    /// Encode to the wire format. The result is the bytes that
    /// follow the 24-byte `EventMeta` prefix in the RedEX payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.body.len());
        buf.put_u16_le(self.status.to_wire());
        encode_headers(&self.headers, &mut buf);
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
        buf
    }

    /// Decode from the wire bytes following the `EventMeta` prefix.
    pub fn decode(data: &[u8]) -> Result<Self, RpcCodecError> {
        let mut cur = std::io::Cursor::new(data);
        if cur.remaining() < 2 {
            return Err(RpcCodecError::Truncated("status"));
        }
        let status = RpcStatus::from_wire(cur.get_u16_le());
        let headers = decode_headers(&mut cur, data)?;
        if cur.remaining() < 4 {
            return Err(RpcCodecError::Truncated("body length"));
        }
        let body_len = cur.get_u32_le() as usize;
        if body_len > MAX_RPC_BODY_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "body",
                actual: body_len,
                limit: MAX_RPC_BODY_LEN,
            });
        }
        if cur.remaining() < body_len {
            return Err(RpcCodecError::Truncated("body bytes"));
        }
        let body_start = cur.position() as usize;
        let body_end = body_start + body_len;
        let body = data[body_start..body_end].to_vec();
        Ok(Self {
            status,
            headers,
            body,
        })
    }
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

fn decode_headers(
    cur: &mut std::io::Cursor<&[u8]>,
    data: &[u8],
) -> Result<Vec<RpcHeader>, RpcCodecError> {
    if cur.remaining() < 1 {
        return Err(RpcCodecError::Truncated("headers count"));
    }
    let count = cur.get_u8() as usize;
    if count > MAX_RPC_HEADERS {
        return Err(RpcCodecError::TooLarge {
            field: "headers",
            actual: count,
            limit: MAX_RPC_HEADERS,
        });
    }
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        if cur.remaining() < 1 {
            return Err(RpcCodecError::Truncated("header name length"));
        }
        let name_len = cur.get_u8() as usize;
        if name_len == 0 {
            return Err(RpcCodecError::Truncated("empty header name"));
        }
        if name_len > MAX_RPC_HEADER_NAME_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "header name",
                actual: name_len,
                limit: MAX_RPC_HEADER_NAME_LEN,
            });
        }
        if cur.remaining() < name_len {
            return Err(RpcCodecError::Truncated("header name bytes"));
        }
        let nstart = cur.position() as usize;
        let nend = nstart + name_len;
        let name = std::str::from_utf8(&data[nstart..nend])
            .map_err(|_| RpcCodecError::InvalidUtf8("header name"))?
            .to_string();
        cur.set_position(nend as u64);

        if cur.remaining() < 2 {
            return Err(RpcCodecError::Truncated("header value length"));
        }
        let value_len = cur.get_u16_le() as usize;
        if value_len > MAX_RPC_HEADER_VALUE_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "header value",
                actual: value_len,
                limit: MAX_RPC_HEADER_VALUE_LEN,
            });
        }
        if cur.remaining() < value_len {
            return Err(RpcCodecError::Truncated("header value bytes"));
        }
        let vstart = cur.position() as usize;
        let vend = vstart + value_len;
        let value = data[vstart..vend].to_vec();
        cur.set_position(vend as u64);
        headers.push((name, value));
    }
    Ok(headers)
}

/// Convenience: the byte layout of an `RpcRequestPayload` that lands
/// after the `EventMeta` prefix in a `DISPATCH_RPC_REQUEST` event.
/// Exposed so callers can budget the total event size at the bus
/// layer without doing the encode first.
pub fn request_wire_size(payload: &RpcRequestPayload) -> usize {
    EVENT_META_SIZE + payload.encode().len()
}

/// Same for `RpcResponsePayload` after the `EventMeta` prefix in a
/// `DISPATCH_RPC_RESPONSE` event.
pub fn response_wire_size(payload: &RpcResponsePayload) -> usize {
    EVENT_META_SIZE + payload.encode().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &[u8]) -> RpcHeader {
        (name.to_string(), value.to_vec())
    }

    // --------------------------------------------------------------------
    // Status code numbering.
    // --------------------------------------------------------------------

    /// Status codes have stable wire numbers. A regression that
    /// renumbered any of the canonical statuses would break
    /// every cross-version caller / server pair on the wire — pin
    /// the numbers explicitly so the test catches it before the
    /// bug ships.
    #[test]
    fn status_wire_numbers_are_stable() {
        for (status, expected) in [
            (RpcStatus::Ok, 0x0000u16),
            (RpcStatus::NotFound, 0x0001),
            (RpcStatus::Unauthorized, 0x0002),
            (RpcStatus::Timeout, 0x0003),
            (RpcStatus::Backpressure, 0x0004),
            (RpcStatus::Cancelled, 0x0005),
            (RpcStatus::Internal, 0x0006),
            (RpcStatus::UnknownVersion, 0x0007),
        ] {
            assert_eq!(status.to_wire(), expected, "{status:?}");
            assert_eq!(RpcStatus::from_wire(expected), status);
        }
    }

    /// Reserved numeric range (`0x0008..=0x7FFF`) decodes as
    /// `Application(v)` for forward-compat with future canonical
    /// assignments. A future status numbered `0x0008` would round-
    /// trip via `from_wire(0x0008)` until that variant is added,
    /// at which point the variant takes precedence.
    #[test]
    fn reserved_status_range_decodes_as_application_for_forward_compat() {
        let decoded = RpcStatus::from_wire(0x0008);
        assert_eq!(decoded, RpcStatus::Application(0x0008));
        assert_eq!(decoded.to_wire(), 0x0008);
    }

    /// Application range (`0x8000..=0xFFFF`) encodes / decodes
    /// transparently as `Application(v)`.
    #[test]
    fn application_status_range_roundtrips() {
        for v in [0x8000u16, 0x8001, 0xCAFE, 0xFFFF] {
            let s = RpcStatus::from_wire(v);
            assert_eq!(s, RpcStatus::Application(v));
            assert_eq!(s.to_wire(), v);
        }
    }

    // --------------------------------------------------------------------
    // Dispatch byte assignments.
    // --------------------------------------------------------------------

    /// Pin the `dispatch` byte assignments so a renumber surfaces
    /// here before it ships on the wire. These also live in the
    /// design doc; this test is the source-of-truth check.
    #[test]
    fn dispatch_byte_assignments_are_stable() {
        assert_eq!(DISPATCH_RPC_REQUEST, 0x10);
        assert_eq!(DISPATCH_RPC_RESPONSE, 0x11);
        assert_eq!(DISPATCH_RPC_CANCEL, 0x12);
        assert_eq!(DISPATCH_RPC_DEADLINE_EXCEEDED, 0x13);
    }

    // --------------------------------------------------------------------
    // RpcRequestPayload codec.
    // --------------------------------------------------------------------

    #[test]
    fn request_roundtrip_minimal() {
        let p = RpcRequestPayload {
            service: "hello".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        let bytes = p.encode();
        let decoded = RpcRequestPayload::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn request_roundtrip_full() {
        let p = RpcRequestPayload {
            service: "echo.v1".to_string(),
            deadline_ns: 1_700_000_000_000_000_000,
            flags: FLAG_RPC_IDEMPOTENT | FLAG_RPC_PROPAGATE_TRACE,
            headers: vec![
                header("traceparent", b"00-aabb..."),
                header("idempotency-key", &7u64.to_le_bytes()),
                header("content-type", b"application/json"),
            ],
            body: b"{\"hello\":\"world\"}".to_vec(),
        };
        let bytes = p.encode();
        let decoded = RpcRequestPayload::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn request_decode_rejects_empty_service() {
        let bytes = vec![0x00];
        let err = RpcRequestPayload::decode(&bytes).unwrap_err();
        assert!(matches!(err, RpcCodecError::Truncated(_)));
    }

    #[test]
    fn request_decode_rejects_oversize_body_length() {
        // Forge: service "x", deadline 0, flags 0, no headers,
        // body length = MAX_RPC_BODY_LEN + 1 (no body bytes).
        let mut bytes = vec![1u8, b'x'];
        bytes.extend_from_slice(&0u64.to_le_bytes()); // deadline
        bytes.extend_from_slice(&0u16.to_le_bytes()); // flags
        bytes.push(0); // 0 headers
        bytes.extend_from_slice(&((MAX_RPC_BODY_LEN as u32) + 1).to_le_bytes());
        let err = RpcRequestPayload::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, RpcCodecError::TooLarge { field, .. } if field == "body"),
            "got {err:?}",
        );
    }

    #[test]
    fn request_decode_rejects_oversize_headers_count() {
        // Forge: service "x", deadline 0, flags 0, headers count =
        // MAX_RPC_HEADERS + 1 (no header bytes).
        let mut bytes = vec![1u8, b'x'];
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.push((MAX_RPC_HEADERS as u8).wrapping_add(1));
        let err = RpcRequestPayload::decode(&bytes).unwrap_err();
        assert!(
            matches!(err, RpcCodecError::TooLarge { field, .. } if field == "headers"),
            "got {err:?}",
        );
    }

    #[test]
    fn request_decode_rejects_truncated_at_each_field() {
        // Build a valid payload then truncate at each field
        // boundary; every truncation must error rather than silently
        // accept partial state.
        let p = RpcRequestPayload {
            service: "svc".to_string(),
            deadline_ns: 1,
            flags: 0,
            headers: vec![header("h", b"v")],
            body: b"body".to_vec(),
        };
        let bytes = p.encode();
        // Try each prefix length up to but not including the full
        // length — every one must be a decode error.
        for trim_to in 0..bytes.len() {
            let truncated = &bytes[..trim_to];
            let result = RpcRequestPayload::decode(truncated);
            assert!(
                result.is_err(),
                "trim_to={trim_to} of {} must error, got {:?}",
                bytes.len(),
                result,
            );
        }
        // Full length must succeed.
        assert!(RpcRequestPayload::decode(&bytes).is_ok());
    }

    // --------------------------------------------------------------------
    // RpcResponsePayload codec.
    // --------------------------------------------------------------------

    #[test]
    fn response_roundtrip_ok_with_body() {
        let p = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![header("content-type", b"application/json")],
            body: b"{\"answer\":42}".to_vec(),
        };
        let bytes = p.encode();
        let decoded = RpcResponsePayload::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn response_roundtrip_application_status() {
        let p = RpcResponsePayload {
            status: RpcStatus::Application(0xBEEF),
            headers: vec![],
            body: b"app-specific diagnostic".to_vec(),
        };
        let bytes = p.encode();
        let decoded = RpcResponsePayload::decode(&bytes).unwrap();
        assert_eq!(decoded.status, RpcStatus::Application(0xBEEF));
        assert_eq!(decoded.body, p.body);
    }

    #[test]
    fn response_decode_rejects_empty_buffer() {
        let err = RpcResponsePayload::decode(&[]).unwrap_err();
        assert!(matches!(err, RpcCodecError::Truncated(_)));
    }

    // --------------------------------------------------------------------
    // Invariant: encoded sizes are reasonable.
    // --------------------------------------------------------------------

    /// Wire-size budget regression: a tiny request encodes in a
    /// small constant number of bytes plus body. Pre-fix the headers
    /// or service-length encoding could have grown unbounded; pin
    /// the small-case so a regression in either inflates the
    /// minimum.
    #[test]
    fn request_minimum_wire_size_is_bounded() {
        let p = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        let size = p.encode().len();
        // 1 (svc len) + 1 (svc bytes) + 8 (deadline) + 2 (flags) + 1 (headers count) + 4 (body len) = 17
        assert_eq!(size, 17, "minimum request encodes in 17 bytes");
        assert_eq!(request_wire_size(&p), EVENT_META_SIZE + 17);
    }

    #[test]
    fn response_minimum_wire_size_is_bounded() {
        let p = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        };
        let size = p.encode().len();
        // 2 (status) + 1 (headers count) + 4 (body len) = 7
        assert_eq!(size, 7, "minimum response encodes in 7 bytes");
        assert_eq!(response_wire_size(&p), EVENT_META_SIZE + 7);
    }
}
