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
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use super::super::redex::{RedexError, RedexEvent, RedexFold};
use super::meta::{EventMeta, EVENT_META_SIZE};

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

/// Caller → server. Stream credit grant. Carries a 4-byte
/// big-endian `u32` in the payload after `EventMeta`: the number
/// of additional response chunks the caller is willing to accept
/// for the streaming call identified by `EventMeta::seq_or_ts`.
///
/// Only meaningful when the caller opted into flow control via
/// the `nrpc-stream-window-initial` request header
/// ([`HEADER_NRPC_STREAM_WINDOW_INITIAL`]). On a flow-controlled
/// stream the server's pump task awaits one credit per chunk; on
/// a non-flow-controlled stream (no header) the server ignores
/// every GRANT.
///
/// Phase 3.
pub const DISPATCH_RPC_STREAM_GRANT: u8 = 0x14;

// ============================================================================
// `RpcRequestPayload::flags` bit assignments.
// ============================================================================

// Bit 0 (`1 << 0`) is RESERVED — was previously documented as
// `FLAG_RPC_IDEMPOTENT`, but the server-side replay-cache (LRU of
// `(origin_hash, call_id) -> RpcResponsePayload`) was never landed,
// so the flag silently no-op'd despite a load-bearing contract in
// its doc-string. Removed to avoid shipping a documented behavior
// the runtime doesn't implement; reservation kept so a future
// re-add (with the LRU) preserves wire compatibility.

/// Set if the server may emit multiple `DISPATCH_RPC_RESPONSE` events
/// for this call. Without it, the first response terminates the
/// call. With it, each response except the terminal one carries
/// `headers["nrpc-streaming"] = b"continue"`; the terminal response
/// has either `b"end"` (success) or a non-`Ok` status.
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
    /// Compute the encoded byte length WITHOUT actually encoding.
    /// Used by [`request_wire_size`] and any caller that needs to
    /// budget event size at the bus layer (e.g., to refuse a
    /// request that wouldn't fit in the configured packet budget)
    /// without paying the encode cost.
    pub fn encoded_len(&self) -> usize {
        // service: u8 length + bytes
        1 + self.service.len()
            // deadline_ns: u64
            + 8
            // flags: u16
            + 2
            // headers: u8 count + per-header (u8 name_len + name + u16 value_len + value)
            + 1
            + self
                .headers
                .iter()
                .map(|(n, v)| 1 + n.len() + 2 + v.len())
                .sum::<usize>()
            // body: u32 length + bytes
            + 4
            + self.body.len()
    }

    /// Encode to the wire format. The result is the bytes that
    /// follow the 24-byte `EventMeta` prefix in the RedEX payload.
    ///
    /// **Encoder bounds:** every field that has a `MAX_RPC_*` cap
    /// is asserted against that cap. In debug builds an oversize
    /// field panics with a useful diagnostic so the programmer
    /// notices in tests; in release builds the assert is dropped
    /// (the decoder side still enforces the cap, so a malformed
    /// frame would be rejected by the receiver — but constructing
    /// one is always a caller bug).
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 + self.body.len());
        // service
        let svc = self.service.as_bytes();
        debug_assert!(
            svc.len() <= MAX_RPC_SERVICE_NAME_LEN,
            "service name {} exceeds MAX_RPC_SERVICE_NAME_LEN ({})",
            svc.len(),
            MAX_RPC_SERVICE_NAME_LEN,
        );
        buf.put_u8(svc.len() as u8);
        buf.extend_from_slice(svc);
        // deadline_ns
        buf.put_u64_le(self.deadline_ns);
        // flags
        buf.put_u16_le(self.flags);
        // headers
        encode_headers(&self.headers, &mut buf);
        // body
        debug_assert!(
            self.body.len() <= MAX_RPC_BODY_LEN,
            "body length {} exceeds MAX_RPC_BODY_LEN ({})",
            self.body.len(),
            MAX_RPC_BODY_LEN,
        );
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
    /// Compute the encoded byte length WITHOUT actually encoding.
    /// See [`RpcRequestPayload::encoded_len`].
    pub fn encoded_len(&self) -> usize {
        // status: u16
        2
            // headers: u8 count + per-header
            + 1
            + self
                .headers
                .iter()
                .map(|(n, v)| 1 + n.len() + 2 + v.len())
                .sum::<usize>()
            // body: u32 length + bytes
            + 4
            + self.body.len()
    }

    /// Encode to the wire format. The result is the bytes that
    /// follow the 24-byte `EventMeta` prefix in the RedEX payload.
    /// Same encoder-bounds policy as
    /// [`RpcRequestPayload::encode`] — see that method's doc.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.body.len());
        buf.put_u16_le(self.status.to_wire());
        encode_headers(&self.headers, &mut buf);
        debug_assert!(
            self.body.len() <= MAX_RPC_BODY_LEN,
            "body length {} exceeds MAX_RPC_BODY_LEN ({})",
            self.body.len(),
            MAX_RPC_BODY_LEN,
        );
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

/// Pull `traceparent` / `tracestate` out of `headers` if present.
/// Caller-side helper: callers building an `RpcRequestPayload`
/// with a `TraceContext` use [`build_trace_headers`] to emit the
/// matching headers; this is the inverse on the server side.
///
/// Returns `Some(TraceContext)` if `traceparent` is present;
/// `None` otherwise. `tracestate` defaults to empty when absent
/// — W3C says tracestate is optional even when traceparent is
/// set.
pub fn extract_trace_context(headers: &[RpcHeader]) -> Option<TraceContext> {
    let mut traceparent: Option<String> = None;
    let mut tracestate = String::new();
    for (name, value) in headers {
        // Header names are case-insensitive (matches W3C and HTTP
        // convention) — same comparison style as `parse_stream_
        // window_initial` for consistency. The wire spec doesn't
        // mandate case so a peer that emits `Traceparent` (capital
        // T) shouldn't be silently ignored.
        if name.eq_ignore_ascii_case("traceparent") {
            if let Ok(s) = std::str::from_utf8(value) {
                traceparent = Some(s.to_string());
            }
        } else if name.eq_ignore_ascii_case("tracestate") {
            if let Ok(s) = std::str::from_utf8(value) {
                tracestate = s.to_string();
            }
        }
    }
    traceparent.map(|tp| TraceContext {
        traceparent: tp,
        tracestate,
    })
}

/// Build the headers a caller appends to its
/// `RpcRequestPayload::headers` to propagate the trace context
/// across the call. Set `RpcRequestPayload::flags |= FLAG_RPC_PROPAGATE_TRACE`
/// alongside this so the server's fold knows to extract them.
///
/// Always emits `traceparent`. Emits `tracestate` only when
/// non-empty (matches the W3C convention of skipping empty
/// tracestate values on the wire).
pub fn build_trace_headers(ctx: &TraceContext) -> Vec<RpcHeader> {
    let mut headers = Vec::with_capacity(2);
    headers.push((
        "traceparent".to_string(),
        ctx.traceparent.clone().into_bytes(),
    ));
    if !ctx.tracestate.is_empty() {
        headers.push((
            "tracestate".to_string(),
            ctx.tracestate.clone().into_bytes(),
        ));
    }
    headers
}

fn encode_headers(headers: &[RpcHeader], buf: &mut Vec<u8>) {
    debug_assert!(
        headers.len() <= MAX_RPC_HEADERS,
        "headers count {} exceeds MAX_RPC_HEADERS ({})",
        headers.len(),
        MAX_RPC_HEADERS,
    );
    buf.put_u8(headers.len() as u8);
    for (name, value) in headers {
        let nbytes = name.as_bytes();
        debug_assert!(
            nbytes.len() <= MAX_RPC_HEADER_NAME_LEN,
            "header name {} exceeds MAX_RPC_HEADER_NAME_LEN ({})",
            nbytes.len(),
            MAX_RPC_HEADER_NAME_LEN,
        );
        debug_assert!(
            value.len() <= MAX_RPC_HEADER_VALUE_LEN,
            "header value {} exceeds MAX_RPC_HEADER_VALUE_LEN ({})",
            value.len(),
            MAX_RPC_HEADER_VALUE_LEN,
        );
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
    EVENT_META_SIZE + payload.encoded_len()
}

/// Same for `RpcResponsePayload` after the `EventMeta` prefix in a
/// `DISPATCH_RPC_RESPONSE` event.
pub fn response_wire_size(payload: &RpcResponsePayload) -> usize {
    EVENT_META_SIZE + payload.encoded_len()
}

// ============================================================================
// Mesh inbound dispatch hook.
//
// `MeshNode::dispatch_packet` normally pushes inbound channel
// events onto a per-shard `inbound` queue keyed by `shard_id`. The
// channel name / hash is stripped on the way in — by the time the
// event lands in the queue, only the payload remains.
//
// RPC needs per-channel routing (events for `<service>.requests`
// drive the server fold; events for `<service>.replies.<origin>`
// drive the client fold). Without channel info on the queued
// event, we can't filter from the consumer side.
//
// The hook below adds a per-channel-hash dispatcher map that the
// mesh's inbound dispatch consults BEFORE pushing to the shard
// queue. If a dispatcher is registered for the event's
// canonical [`ChannelHash`], the event is routed there directly
// (bypassing the shard queue); otherwise the existing shard-queue
// path runs.
//
// **Collision posture.** The dispatch event carries the canonical
// 32-bit [`ChannelHash`] (joint-collision threshold ~65 K
// channels, well above realistic deployment); the wire
// `NetHeader::channel_hash` is `u16` and may bucket-collide at
// scale, so the mesh's inbound dispatch indexes by the wire `u16`
// and dispatches every canonical entry registered in that bucket
// (the canonical match resolves on the dispatcher side). At
// typical sizing this is a single entry per bucket.
// ============================================================================

/// One inbound event delivered to a registered RPC dispatcher.
#[derive(Debug, Clone)]
pub struct RpcInboundEvent {
    /// Canonical [`ChannelHash`] (u32) of the channel this event
    /// arrived on — widened from the per-packet wire `u16`
    /// `NetHeader::channel_hash` via the registered-dispatcher
    /// table at receive time. Collision-resistant at realistic
    /// scale; the wire `u16` may bucket-collide but the canonical
    /// hash uniquely identifies the registered dispatcher target.
    pub channel_hash: super::super::channel::ChannelHash,
    /// Caller's `origin_hash` from the packet header (32-bit
    /// routing projection of the AEAD-verified peer's full
    /// `EntityKeypair::origin_hash()` — see `OriginStamp` doc).
    /// The dispatcher should treat this as routing metadata, not
    /// identity authentication.
    pub origin_hash: u32,
    /// Event payload bytes — the same bytes that would have been
    /// pushed onto the shard inbound queue. For RPC events these
    /// start with a 24-byte `EventMeta` followed by the
    /// `RpcRequestPayload` / `RpcResponsePayload` encoding.
    pub payload: bytes::Bytes,
}

/// Type-erased callback fired by the mesh's inbound dispatch
/// when an event arrives for a registered `channel_hash`. The
/// callback runs on the mesh's dispatch task, so the body should
/// be quick (push the event onto an mpsc / fold consumer rather
/// than do real work).
pub type RpcInboundDispatcher = Arc<dyn Fn(RpcInboundEvent) + Send + Sync + 'static>;

// ============================================================================
// Streaming-response protocol markers.
//
// When a caller sets `FLAG_RPC_STREAMING_RESPONSE` on the request,
// the server emits multiple `DISPATCH_RPC_RESPONSE` events for the
// same `call_id`. Non-terminal chunks carry the
// `nrpc-streaming = continue` header; the terminal chunk carries
// `nrpc-streaming = end` (or any non-`Ok` status, which is also
// terminal). The client-side stream collects chunks until it sees
// a terminal marker.
// ============================================================================

/// Header name nRPC uses to mark streaming-response chunks.
/// Present on every chunk of a streaming response, with one of two
/// values defined below.
pub const HEADER_NRPC_STREAMING: &str = "nrpc-streaming";

/// `nrpc-streaming` value on a non-terminal chunk. The client-side
/// stream yields the chunk's body and continues waiting for more.
pub const HEADER_NRPC_STREAMING_CONTINUE: &[u8] = b"continue";

/// `nrpc-streaming` value on the terminal chunk. The client-side
/// stream yields the chunk's body (if non-empty) and then closes.
/// A non-`Ok` status is also terminal, regardless of header — the
/// stream yields the error and closes.
pub const HEADER_NRPC_STREAMING_END: &[u8] = b"end";

/// Header on a streaming REQUEST that opts into flow control with
/// the given initial credit window. Value is the ASCII decimal
/// representation of a `u32` (e.g. `"32"`). When present, the
/// server's streaming fold creates a per-call semaphore initialized
/// to that count and the pump awaits one credit per emitted chunk.
/// The caller refills via [`DISPATCH_RPC_STREAM_GRANT`] events.
///
/// Absent → unbounded credit (the pump emits chunks as fast as
/// the publish path can take them). Long-running streams that
/// could outpace a slow consumer SHOULD opt into flow control —
/// without it, the server's sink mpsc grows unbounded under a
/// stalled caller.
pub const HEADER_NRPC_STREAM_WINDOW_INITIAL: &str = "nrpc-stream-window-initial";

/// Encode a stream-grant payload — 4 bytes big-endian `u32`
/// representing additional credit. Pair with [`decode_stream_grant`]
/// on the server side.
pub fn encode_stream_grant(amount: u32) -> Vec<u8> {
    amount.to_be_bytes().to_vec()
}

/// Decode a stream-grant payload. Returns `None` if the slice is
/// not exactly 4 bytes — defends the server fold against
/// malformed grants without killing the cortex adapter.
pub fn decode_stream_grant(payload: &[u8]) -> Option<u32> {
    if payload.len() != 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(payload);
    Some(u32::from_be_bytes(bytes))
}

/// Parse the `nrpc-stream-window-initial` header from a request's
/// header list. Returns `Some(window)` if a valid u32 ASCII-decimal
/// value is present, else `None` (no header / malformed value /
/// non-utf8 — all treated as "no flow control").
pub fn parse_stream_window_initial(headers: &[RpcHeader]) -> Option<u32> {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(HEADER_NRPC_STREAM_WINDOW_INITIAL) {
            return std::str::from_utf8(value).ok()?.parse::<u32>().ok();
        }
    }
    None
}

/// Inspect a `RpcResponsePayload`'s headers and decide whether
/// it's a non-terminal streaming chunk (`continue`), a terminal
/// streaming chunk (`end` OR non-`Ok` status), OR a unary
/// response (no streaming header at all). Used by the client-side
/// fold to demux streaming vs unary responses without needing a
/// separate flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingChunkKind {
    /// Non-terminal chunk — yield body, continue waiting.
    Continue,
    /// Terminal chunk — yield body (if any), close stream.
    Terminal,
    /// Not a streaming response — unary semantics apply.
    Unary,
}

/// Classify a response per the streaming-protocol markers.
pub fn classify_streaming_chunk(resp: &RpcResponsePayload) -> StreamingChunkKind {
    // Non-Ok status is always terminal regardless of header — the
    // stream surfaces the error and closes.
    if !resp.status.is_ok() {
        return StreamingChunkKind::Terminal;
    }
    // Walk headers for the streaming marker. Absence = unary
    // semantics (caller used `call`, not `call_streaming`).
    for (name, value) in &resp.headers {
        if name == HEADER_NRPC_STREAMING {
            return if value.as_slice() == HEADER_NRPC_STREAMING_END {
                StreamingChunkKind::Terminal
            } else if value.as_slice() == HEADER_NRPC_STREAMING_CONTINUE {
                StreamingChunkKind::Continue
            } else {
                // Unknown marker value — be defensive, treat as
                // terminal so a misbehaving server doesn't keep
                // a stream open forever.
                StreamingChunkKind::Terminal
            };
        }
    }
    StreamingChunkKind::Unary
}

// ============================================================================
// Server-side fold.
//
// `RpcServerFold` is the `RedexFold` half of the server. It sees
// REQUEST events on the channel the cortex adapter is opened against,
// spawns the user handler in a tokio task, and emits the RESPONSE
// via a callback the `Mesh::serve_rpc` glue layer wires up. The
// fold itself is small and pure — all I/O happens in the spawned
// task and the emitter callback.
//
// Cancellation: each in-flight call gets an `RpcCancellationToken`
// that the handler can `select!` on. CANCEL events flip the
// matching token; the handler observes `cancellation.cancelled()`
// firing and aborts cooperatively.
// ============================================================================

/// Cancellation signal for an in-flight RPC handler.
///
/// Created when the fold dispatches a REQUEST; cloned into the
/// handler's `RpcContext` and held in the fold's in-flight map. A
/// matching CANCEL event flips the token; handlers observe via
/// either [`Self::is_cancelled`] (synchronous probe) or
/// [`Self::cancelled`] (await for the signal).
#[derive(Clone, Default)]
pub struct RpcCancellationToken {
    inner: Arc<RpcCancellationInner>,
}

#[derive(Default)]
struct RpcCancellationInner {
    fired: AtomicBool,
    notify: Notify,
}

impl RpcCancellationToken {
    /// Construct a fresh, un-fired token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the token. Idempotent — repeated calls are no-ops.
    /// Wakes any task currently in [`Self::cancelled`].
    pub fn cancel(&self) {
        // Release pairs with the Acquire load in `is_cancelled`
        // so a handler that observes `is_cancelled() == true` is
        // guaranteed to see every prior write the canceller did.
        self.inner.fired.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Synchronous probe. `true` once `cancel()` has been called.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.fired.load(Ordering::Acquire)
    }

    /// Await the cancellation. Returns immediately if already
    /// cancelled. Otherwise registers as a waiter and returns when
    /// `cancel()` is called.
    ///
    /// Race-safe: registering the `notified()` future BEFORE the
    /// `is_cancelled` check means a `cancel()` racing this method
    /// either (a) is observed by the post-register check and we
    /// return immediately, OR (b) lands after we register and wakes
    /// our future. Either way we don't sleep past a cancellation.
    pub async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// W3C Trace Context — `traceparent` and `tracestate` headers
/// propagated through nRPC for distributed-tracing systems.
///
/// `traceparent` carries the trace id, parent span id, and flags;
/// `tracestate` carries vendor-specific tracing extensions. nRPC
/// is **transport-only** for these — it doesn't parse or generate
/// IDs, doesn't emit spans, doesn't talk to any tracing backend.
/// Application code (typically via `tracing-opentelemetry` or a
/// Datadog client) reads these on the server side and continues
/// the trace.
///
/// See <https://www.w3.org/TR/trace-context/> for the wire format
/// of each field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceContext {
    /// `traceparent` header value (e.g.
    /// `"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"`).
    /// Required by the W3C spec; nRPC treats it as opaque bytes.
    pub traceparent: String,
    /// `tracestate` header value — vendor-specific extensions.
    /// Optional in W3C; empty string when absent.
    pub tracestate: String,
}

/// Context handed to a `RpcHandler::call`. Carries everything the
/// handler needs to fulfill the request: the AEAD-verified caller
/// identity, the request payload, and a cancellation token.
pub struct RpcContext {
    /// AEAD-verified caller `origin_hash`. The bus sets this from
    /// the verified peer; not self-claimable from the request body.
    pub caller_origin: u64,
    /// Caller-generated correlation id. Same value on the matching
    /// CANCEL or RESPONSE.
    pub call_id: u64,
    /// Decoded request payload.
    pub payload: RpcRequestPayload,
    /// Cancellation signal. Handlers should `select!` on
    /// `cancellation.cancelled()` if their work is async-cancellable;
    /// long-running synchronous handlers should periodically check
    /// `cancellation.is_cancelled()`.
    pub cancellation: RpcCancellationToken,
    /// W3C Trace Context propagated from the caller, if the
    /// caller set `FLAG_RPC_PROPAGATE_TRACE` and supplied
    /// `traceparent` / `tracestate` headers in the request. The
    /// server's handler reads this to continue the distributed
    /// trace. `None` for calls that didn't propagate trace
    /// context.
    pub trace_context: Option<TraceContext>,
}

/// Handler-side error that doesn't fit the application's normal
/// `Ok(RpcResponsePayload)` channel. The fold maps these onto a
/// failure-status `RpcResponsePayload` for the caller.
#[derive(Debug, thiserror::Error)]
pub enum RpcHandlerError {
    /// Application-defined error. The fold encodes this as
    /// `RpcStatus::Application(code)` with `message` as the body.
    #[error("application error {code:#06x}: {message}")]
    Application {
        /// Application error code; surfaces as `RpcStatus::Application(code)`
        /// to the caller. Use `0x8000..=0xFFFF` to avoid the
        /// reserved canonical range.
        code: u16,
        /// Diagnostic. Becomes the response body (UTF-8 bytes).
        message: String,
    },
    /// Catch-all for handler-internal failures. The fold encodes this
    /// as `RpcStatus::Internal` with `message` as the body.
    #[error("internal: {0}")]
    Internal(String),
}

/// User-supplied handler. Implementors typically wrap their state
/// (or an `Arc<Mutex<State>>`) and route to the appropriate logic
/// based on `ctx.payload.service` or per-handler dispatch.
///
/// Multiple `Mesh::serve_rpc` registrations on different services
/// each install their own handler; a single handler typically
/// services one service.
#[async_trait::async_trait]
pub trait RpcHandler: Send + Sync + 'static {
    /// Process one request and return the response payload. The
    /// fold spawns this in a tokio task; the fold itself doesn't
    /// block on it. Handlers should respect `ctx.cancellation` for
    /// cooperative early-abort.
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError>;
}

/// Callback the fold invokes to publish a response back to the
/// caller. Wired up by `Mesh::serve_rpc` to publish on
/// `<service>.replies.<caller_origin>`. Type-erased so the fold
/// doesn't depend on the mesh layer directly.
///
/// Arguments: `(caller_origin, call_id, response_payload)`.
pub type RpcResponseEmitter = Arc<dyn Fn(u64, u64, RpcResponsePayload) + Send + Sync + 'static>;

/// Async counterpart of [`RpcResponseEmitter`] used by the
/// streaming fold's pump task to serialize per-call publishes.
///
/// The streaming pump awaits each emit before reading the next
/// chunk from the sink — this guarantees that chunks for one
/// `call_id` reach the network publish path in the order the
/// handler emitted them. (The unary fold has no such requirement
/// — it emits exactly one RESPONSE per call — so it sticks with
/// the simpler sync `RpcResponseEmitter`.)
pub type RpcAsyncResponseEmitter = Arc<
    dyn Fn(u64, u64, RpcResponsePayload) -> futures::future::BoxFuture<'static, ()>
        + Send
        + Sync
        + 'static,
>;

/// Server-side fold. Sees REQUEST events on the configured channel,
/// dispatches to the user-supplied handler, emits RESPONSE events
/// via the supplied emitter. CANCEL events flip the matching
/// in-flight token.
///
/// State `()` — the user's state lives on whatever the `RpcHandler`
/// captures (typically `Arc<Mutex<S>>`). The fold's own state (the
/// in-flight map) lives on `&mut self` and is shared with spawned
/// handler tasks via `Arc<Mutex<...>>` so the task can self-clean
/// on completion.
pub struct RpcServerFold {
    handler: Arc<dyn RpcHandler>,
    emit: RpcResponseEmitter,
    /// (caller_origin, call_id) → cancellation token for the
    /// in-flight handler. Inserted on REQUEST, removed by either
    /// the spawned handler task on completion or by the fold on
    /// CANCEL. Wrapped in `Arc<Mutex<...>>` so spawned tasks can
    /// remove their own entries without going back through the
    /// fold.
    in_flight: Arc<Mutex<HashMap<(u64, u64), RpcCancellationToken>>>,
    /// Optional per-service metrics handle. When `Some`, the
    /// spawned handler task bumps `handler_invocations_total` /
    /// `handler_in_flight` / `handler_panics_total` and records
    /// per-task wall-clock durations. `None` → no metrics
    /// (test-only path; production `Mesh::serve_rpc` always
    /// supplies one).
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
    /// Optional clock override for tests. `None` → real wall-clock
    /// `unix_nanos`. `Some(...)` → fixed value, lets tests pin
    /// deadline-already-passed behavior without sleeping.
    #[cfg(test)]
    test_now_ns: Option<u64>,
}

impl RpcServerFold {
    /// Construct a server fold around `handler`. `emit` is the
    /// callback that publishes RESPONSE events to the caller's
    /// reply channel — `Mesh::serve_rpc` wires this to the
    /// publisher for `<service>.replies.<caller_origin>`.
    /// Constructed without a metrics handle; production callers
    /// chain `.with_metrics(...)` to opt into per-service
    /// counters.
    pub fn new(handler: Arc<dyn RpcHandler>, emit: RpcResponseEmitter) -> Self {
        Self {
            handler,
            emit,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            metrics: None,
            #[cfg(test)]
            test_now_ns: None,
        }
    }

    /// Attach a per-service metrics handle. Hooks the spawned
    /// handler task to bump `handler_invocations_total`, balance
    /// `handler_in_flight`, count panics, and record handler
    /// duration into the histogram.
    pub fn with_metrics(
        mut self,
        metrics: Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>,
    ) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Test-only: pin the clock the fold uses for deadline
    /// short-circuit. Lets a unit test exercise the
    /// deadline-already-passed branch without waiting for wall
    /// time.
    #[cfg(test)]
    pub fn with_test_now_ns(mut self, now_ns: u64) -> Self {
        self.test_now_ns = Some(now_ns);
        self
    }

    /// Test-only: snapshot of the in-flight call set.
    #[cfg(test)]
    pub fn in_flight_keys(&self) -> Vec<(u64, u64)> {
        self.in_flight.lock().keys().copied().collect()
    }

    fn now_ns(&self) -> u64 {
        #[cfg(test)]
        if let Some(t) = self.test_now_ns {
            return t;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    /// `true` if the request's deadline has already elapsed at
    /// the server's current wall-clock — accounting for a small
    /// tolerance window that absorbs clock skew between caller
    /// and server. Without the tolerance, a request from a peer
    /// whose clock is a few hundred ms ahead of the server's
    /// would be timed out before the handler even saw it. Matches
    /// gRPC's default deadline-clock-skew tolerance shape (gRPC
    /// uses ~10 s).
    fn deadline_already_passed(&self, deadline_ns: u64) -> bool {
        if deadline_ns == 0 {
            return false;
        }
        self.now_ns().saturating_sub(DEADLINE_SKEW_TOLERANCE_NS) > deadline_ns
    }
}

/// Tolerance for clock skew between caller and server when the
/// server short-circuits a request whose `deadline_ns` looks like
/// it has already elapsed. The check is
/// `now_ns - SKEW > deadline_ns`, so a request from a peer whose
/// clock is up to `SKEW` nanoseconds ahead of ours never hits the
/// short-circuit path. 10 s matches gRPC's default and is well
/// within the threshold an NTP-disciplined cluster ever drifts to.
pub const DEADLINE_SKEW_TOLERANCE_NS: u64 = 10_000_000_000; // 10 seconds

impl RedexFold<()> for RpcServerFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        // Decode the meta header. A garbled meta means the event
        // doesn't even claim to be an RPC packet — log and skip
        // rather than killing the fold. Returning `Err(Decode)`
        // here would stop the entire cortex adapter for one
        // malformed event, which is wrong for an RPC server that
        // needs to keep serving.
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc server fold: event payload too short for EventMeta; skipping",
            );
            return Ok(());
        };
        let key = (meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(&ev.payload[EVENT_META_SIZE..]) {
                    Ok(p) => p,
                    Err(e) => {
                        // Malformed request payload. Surface as
                        // `UnknownVersion` to the caller — they sent
                        // bytes we couldn't parse, which usually
                        // means a wire-format mismatch (the most
                        // common cause). Log so operators can
                        // diagnose.
                        tracing::warn!(
                            error = %e,
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc server fold: malformed request payload",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::UnknownVersion,
                            headers: vec![],
                            body: format!("malformed request: {e}").into_bytes(),
                        };
                        (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                        return Ok(());
                    }
                };
                // Fast deadline-already-passed short-circuit.
                // Server-side `Timeout` without invoking the
                // handler. Includes a clock-skew tolerance window
                // so a peer with a slightly-fast clock isn't
                // prematurely timed out — see
                // `deadline_already_passed`.
                if self.deadline_already_passed(payload.deadline_ns) {
                    let resp = RpcResponsePayload {
                        status: RpcStatus::Timeout,
                        headers: vec![],
                        body: b"deadline already passed when request landed".to_vec(),
                    };
                    (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                    return Ok(());
                }
                // Refuse a duplicate REQUEST with the same
                // `(origin_hash, call_id)` — see streaming fold for
                // the full rationale. For the unary fold this would
                // spawn a second handler under the same key, and
                // whichever handler completes first removes the
                // in-flight entry — leaving the second handler's
                // CANCEL handling broken (CANCEL events look up
                // the now-missing key and no-op). Cleaner to refuse.
                {
                    let in_flight = self.in_flight.lock();
                    if in_flight.contains_key(&key) {
                        drop(in_flight);
                        tracing::warn!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc server fold: duplicate REQUEST for in-flight call_id; refusing",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::Internal,
                            headers: vec![],
                            body: b"duplicate REQUEST for already-in-flight call_id".to_vec(),
                        };
                        (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                        return Ok(());
                    }
                }
                let cancellation = RpcCancellationToken::new();
                self.in_flight.lock().insert(key, cancellation.clone());
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                let caller_origin = meta.origin_hash;
                let call_id = meta.seq_or_ts;
                // Decode the W3C Trace Context if the caller
                // signaled it via `FLAG_RPC_PROPAGATE_TRACE` and
                // included the `traceparent` / `tracestate`
                // headers. nRPC is transport-only — application
                // code reads `ctx.trace_context` to continue the
                // trace via whatever backend it has wired up.
                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let metrics = self.metrics.clone();
                // Keep a probe handle so the spawned task can detect
                // a CANCEL that fired during handler execution and
                // override its response with `RpcStatus::Cancelled`.
                let cancel_probe = cancellation.clone();
                tokio::spawn(async move {
                    // Server-side metrics: count this invocation;
                    // bump in_flight; time the handler; tally
                    // panics. Only fires when a metrics handle was
                    // attached via `with_metrics(...)` — test-only
                    // folds construct without one.
                    if let Some(m) = metrics.as_ref() {
                        m.handler_invocations_total
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.handler_in_flight
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let handler_started = std::time::Instant::now();
                    let ctx = RpcContext {
                        caller_origin,
                        call_id,
                        payload,
                        cancellation,
                        trace_context,
                    };
                    // Catch panics so a misbehaving handler can't
                    // take down the runtime. `AssertUnwindSafe` is
                    // load-bearing because `RpcHandler::call`
                    // returns a future that may borrow non-
                    // `UnwindSafe` types from the handler; we
                    // accept the assertion because the handler's
                    // state is untouched on panic (we just don't
                    // observe its in-progress mutations).
                    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                        handler.call(ctx),
                    ))
                    .await;
                    if let Some(m) = metrics.as_ref() {
                        m.handler_in_flight
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        m.record_handler_duration(handler_started.elapsed());
                        if outcome.is_err() {
                            m.handler_panics_total
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    // CANCEL-wins ordering: if the cancellation
                    // token fired at any point during handler
                    // execution, override the handler's outcome
                    // with `RpcStatus::Cancelled` so the caller
                    // (or hedge primary, retry layer, etc.) sees
                    // the documented `Cancelled` status code rather
                    // than whatever the handler happened to return
                    // before / despite cancellation. A cooperative
                    // handler that observes the token and bails
                    // early gets the same Cancelled framing as a
                    // handler that ignored cancellation and ran to
                    // completion — the caller's view is uniform.
                    let resp = if cancel_probe.is_cancelled() {
                        RpcResponsePayload {
                            status: RpcStatus::Cancelled,
                            headers: vec![],
                            body: b"server observed CANCEL during handler execution".to_vec(),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(payload)) => payload,
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: message.into_bytes(),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: message.into_bytes(),
                            },
                            Err(panic) => {
                                let panic_msg = panic
                                    .downcast_ref::<&'static str>()
                                    .map(|s| s.to_string())
                                    .or_else(|| panic.downcast_ref::<String>().cloned())
                                    .unwrap_or_else(|| "<non-string panic>".into());
                                tracing::error!(
                                    caller_origin = format!("{:#x}", caller_origin),
                                    call_id,
                                    panic = %panic_msg,
                                    "rpc server handler panicked",
                                );
                                RpcResponsePayload {
                                    status: RpcStatus::Internal,
                                    headers: vec![],
                                    body: format!("handler panicked: {panic_msg}").into_bytes(),
                                }
                            }
                        }
                    };
                    in_flight.lock().remove(&key);
                    emit(caller_origin, call_id, resp);
                });
            }
            DISPATCH_RPC_CANCEL => {
                if let Some(token) = self.in_flight.lock().remove(&key) {
                    token.cancel();
                }
                // Idempotent — CANCEL for an unknown call_id (e.g.
                // a CANCEL that races the handler's completion) is
                // a no-op rather than an error. The spawned handler
                // task observes `cancel_probe.is_cancelled()` after
                // its future resolves and overrides the response
                // with `RpcStatus::Cancelled` so the caller sees a
                // documented status code rather than the handler's
                // accidental Ok / Internal payload.
            }
            // RESPONSE / DEADLINE_EXCEEDED are server-emitted; if
            // the server's own fold sees them (e.g. from a replay)
            // there's nothing to do.
            _ => {}
        }
        Ok(())
    }
}

// ============================================================================
// Streaming server-side: handler trait + sink + fold.
// ============================================================================

/// Sink the handler writes to in order to emit streaming-response
/// chunks. Each `send` produces one non-terminal `RESPONSE` event
/// to the caller. The terminal frame is emitted automatically when
/// the sink is dropped — the handler returning `Ok(())` drops the
/// sink, which closes the stream cleanly. Returning
/// `Err(RpcHandlerError)` drops the sink and emits the error as a
/// terminal non-`Ok` RESPONSE.
///
/// `send` is best-effort and infallible: the underlying mpsc is
/// **bounded** at [`STREAMING_PUMP_CAPACITY`] chunks. If the pump
/// can't keep up (publish path is congested, caller hasn't granted
/// flow-control credits), `send` discards on overflow — same
/// observable shape as a closed receiver (caller cancelled mid-
/// stream). Counts the drop in `streaming_chunks_dropped_total` so
/// operators can see backpressure occurring. Cooperative
/// cancellation via `ctx.cancellation` is the right way for the
/// handler to notice the consumer is gone; opt-in flow control via
/// `CallOptions::stream_window_initial` is the right way to
/// throttle a fast handler against a slow consumer.
pub struct RpcResponseSink {
    inner: tokio::sync::mpsc::Sender<bytes::Bytes>,
    /// Optional metrics handle so a dropped-on-full chunk bumps the
    /// `streaming_chunks_dropped_total` counter. `None` for unit-
    /// test folds that construct without metrics.
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
}

impl RpcResponseSink {
    /// Emit one non-terminal chunk. Cheap (`try_send` on a
    /// [`STREAMING_PUMP_CAPACITY`]-bounded mpsc); never blocks. On
    /// overflow OR receiver-closed, the chunk is dropped and (when
    /// metrics are wired) `streaming_chunks_dropped_total` is
    /// incremented for the service.
    pub fn send(&self, body: impl Into<bytes::Bytes>) {
        if self.inner.try_send(body.into()).is_err() {
            if let Some(m) = self.metrics.as_ref() {
                m.streaming_chunks_dropped_total
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// Bounded capacity for the streaming pump's internal mpsc. A
/// runaway handler that produces chunks faster than the publish
/// path can drain them stops blocking the runtime past this many
/// queued chunks — additional chunks are dropped (and counted via
/// `streaming_chunks_dropped_total`). 1024 is generous for typical
/// streaming patterns; opt-in flow control via
/// `CallOptions::stream_window_initial` is the right primitive for
/// strict throttling.
pub const STREAMING_PUMP_CAPACITY: usize = 1024;

/// User-supplied streaming handler. Receives the same `RpcContext`
/// as a unary handler plus a `RpcResponseSink` for emitting chunks.
/// Returning `Ok(())` closes the stream cleanly with a terminal
/// `Ok` RESPONSE; `Err(RpcHandlerError)` closes the stream with a
/// terminal non-`Ok` RESPONSE carrying the diagnostic.
///
/// **Cancellation contract.** Long-running streams should
/// `select!` on `ctx.cancellation.cancelled()` so a caller-side
/// drop / deadline correctly stops the handler. Continuing to
/// `send` after cancellation is harmless (sink discards) but
/// wastes work.
#[async_trait::async_trait]
pub trait RpcStreamingHandler: Send + Sync + 'static {
    /// Process one streaming request. Emit chunks via `sink.send(...)`.
    /// Drop the sink (or return) to close the stream.
    async fn call(&self, ctx: RpcContext, sink: RpcResponseSink) -> Result<(), RpcHandlerError>;
}

/// Per-call flow-control map type. Keyed on
/// `(caller_origin_hash, call_id)`; value is a tokio
/// `Semaphore` shared between the pump task (which awaits
/// permits) and the fold's `apply()` method handling
/// STREAM_GRANT events (which add permits).
type FlowControlMap = Arc<Mutex<HashMap<(u64, u64), Arc<tokio::sync::Semaphore>>>>;

/// Server-side fold for streaming RPC. Parallel to `RpcServerFold`
/// but multi-fire emit: each handler invocation may produce many
/// `RESPONSE` events for the same `call_id`, marked
/// non-terminal/terminal via the `nrpc-streaming` header.
///
/// State `()` — like the unary fold, the handler owns user state
/// via captured `Arc<Mutex<S>>`. The fold's own state (in-flight
/// cancellation tokens) lives on `&mut self`.
pub struct RpcServerStreamingFold {
    handler: Arc<dyn RpcStreamingHandler>,
    emit: RpcAsyncResponseEmitter,
    in_flight: Arc<Mutex<HashMap<(u64, u64), RpcCancellationToken>>>,
    /// Per-call flow-control semaphore (when the caller opted in).
    /// `Some(sem)` means "pump must `acquire().await` one permit
    /// per chunk before emitting; STREAM_GRANT events
    /// `add_permits(n)`". Absence of an entry for a `(origin,
    /// call_id)` key means unbounded credit (no flow control —
    /// pump emits as fast as the publish path can take chunks).
    flow_control: FlowControlMap,
    /// Optional per-service metrics handle. Same shape as
    /// `RpcServerFold::metrics`; the streaming fold ALSO bumps
    /// `streaming_chunks_emitted_total` from the pump task on
    /// every chunk.
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
}

impl RpcServerStreamingFold {
    /// Construct a streaming server fold. `emit` publishes
    /// individual chunks (and the terminal frame) on the caller's
    /// reply channel.
    ///
    /// Uses the **async** emitter variant so the pump task can
    /// serialize per-call publishes — without that ordering
    /// guarantee, two chunks emitted in succession can race into
    /// the publish path and arrive at the caller out of order
    /// (or be eclipsed by the terminal frame and lost entirely).
    pub fn new(handler: Arc<dyn RpcStreamingHandler>, emit: RpcAsyncResponseEmitter) -> Self {
        Self {
            handler,
            emit,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            flow_control: Arc::new(Mutex::new(HashMap::new())),
            metrics: None,
        }
    }

    /// Attach a per-service metrics handle. Hooks the spawned
    /// handler task to bump `handler_invocations_total` /
    /// `handler_in_flight` / `handler_panics_total` /
    /// `handler_duration_*`, and the pump task to bump
    /// `streaming_chunks_emitted_total` per emitted chunk.
    pub fn with_metrics(
        mut self,
        metrics: Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>,
    ) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Test-only: snapshot of the in-flight call set.
    #[cfg(test)]
    pub fn in_flight_keys(&self) -> Vec<(u64, u64)> {
        self.in_flight.lock().keys().copied().collect()
    }
}

impl RedexFold<()> for RpcServerStreamingFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc streaming server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(&ev.payload[EVENT_META_SIZE..]) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc streaming server fold: malformed request payload",
                        );
                        // Surface as a terminal error chunk. Spawn
                        // because the apply method is sync and the
                        // emit is async; this is a one-shot publish
                        // so ordering doesn't matter here.
                        let resp = RpcResponsePayload {
                            status: RpcStatus::UnknownVersion,
                            headers: vec![(
                                HEADER_NRPC_STREAMING.to_string(),
                                HEADER_NRPC_STREAMING_END.to_vec(),
                            )],
                            body: format!("malformed request: {e}").into_bytes(),
                        };
                        let emit = self.emit.clone();
                        let caller_origin = meta.origin_hash;
                        let call_id = meta.seq_or_ts;
                        tokio::spawn(async move {
                            emit(caller_origin, call_id, resp).await;
                        });
                        return Ok(());
                    }
                };
                // Refuse a duplicate REQUEST with the same
                // `(origin_hash, call_id)`. Without this, a retry
                // that arrives while the first attempt's pump is
                // still draining will overwrite the prior
                // semaphore Arc in `flow_control`, leaving the
                // first pump awaiting an orphaned semaphore (the
                // terminal cleanup keys on `key` and removes the
                // *new* entry, so the orphan never gets dropped
                // and the first handler hangs forever).
                //
                // Idempotent for the caller: we emit a terminal
                // `Internal` chunk so the duplicate sender sees a
                // clean refusal rather than waiting on a stream
                // that will never produce output.
                {
                    let in_flight = self.in_flight.lock();
                    if in_flight.contains_key(&key) {
                        drop(in_flight);
                        tracing::warn!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc streaming server fold: duplicate REQUEST for in-flight call_id; refusing",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::Internal,
                            headers: vec![(
                                HEADER_NRPC_STREAMING.to_string(),
                                HEADER_NRPC_STREAMING_END.to_vec(),
                            )],
                            body: b"duplicate REQUEST for already-in-flight call_id".to_vec(),
                        };
                        let emit = self.emit.clone();
                        let caller_origin = meta.origin_hash;
                        let call_id = meta.seq_or_ts;
                        tokio::spawn(async move {
                            emit(caller_origin, call_id, resp).await;
                        });
                        return Ok(());
                    }
                }
                // Cancellation token + in-flight bookkeeping —
                // identical to the unary fold's pattern.
                let cancellation = RpcCancellationToken::new();
                self.in_flight.lock().insert(key, cancellation.clone());
                // Flow-control opt-in: parse the
                // `nrpc-stream-window-initial` header. When
                // present, install a per-call semaphore the pump
                // task will await per chunk; subsequent
                // STREAM_GRANT events refill it. When absent, no
                // entry → pump skips the await (back-compat).
                let flow_sem = parse_stream_window_initial(&payload.headers).map(|n| {
                    let sem = Arc::new(tokio::sync::Semaphore::new(n as usize));
                    self.flow_control.lock().insert(key, sem.clone());
                    sem
                });
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                let flow_control = self.flow_control.clone();
                let caller_origin = meta.origin_hash;
                let call_id = meta.seq_or_ts;
                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let metrics = self.metrics.clone();
                // See unary fold for rationale — clone the
                // cancellation handle so the spawned task can probe
                // it after the handler returns and override the
                // terminal frame with `RpcStatus::Cancelled`.
                let cancel_probe = cancellation.clone();
                tokio::spawn(async move {
                    if let Some(m) = metrics.as_ref() {
                        m.handler_invocations_total
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.handler_in_flight
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let handler_started = std::time::Instant::now();
                    let ctx = RpcContext {
                        caller_origin,
                        call_id,
                        payload,
                        cancellation,
                        trace_context,
                    };
                    // Build the sink + receive end. Spawn a
                    // pump that forwards each chunk to the emit
                    // closure. The handler's `sink.send(...)`
                    // calls show up here as items on the receiver.
                    // **Bounded** at STREAMING_PUMP_CAPACITY: a
                    // runaway handler that produces chunks faster
                    // than the publish path can drain stops
                    // blocking the runtime past this many queued
                    // chunks; additional chunks are dropped and
                    // counted via streaming_chunks_dropped_total.
                    let (tx, mut rx) =
                        tokio::sync::mpsc::channel::<bytes::Bytes>(STREAMING_PUMP_CAPACITY);
                    let sink = RpcResponseSink {
                        inner: tx,
                        metrics: metrics.clone(),
                    };
                    let pump_emit = emit.clone();
                    let pump_metrics = metrics.clone();
                    let pump_flow = flow_sem.clone();
                    let pump = tokio::spawn(async move {
                        while let Some(chunk) = rx.recv().await {
                            // Flow control: when the caller opted
                            // in, await one semaphore permit per
                            // chunk before publishing. The semaphore
                            // starts at the caller's `initial_window`
                            // and refills when the caller sends
                            // STREAM_GRANT events. `forget()`
                            // consumes the slot — each chunk uses
                            // exactly one credit, never returned.
                            // No-op when `pump_flow` is None
                            // (back-compat path).
                            if let Some(sem) = pump_flow.as_ref() {
                                let permit = match sem.clone().acquire_owned().await {
                                    Ok(p) => p,
                                    Err(_) => {
                                        // Semaphore was closed —
                                        // shouldn't happen during
                                        // normal operation; bail.
                                        break;
                                    }
                                };
                                permit.forget();
                            }
                            if let Some(m) = pump_metrics.as_ref() {
                                m.streaming_chunks_emitted_total
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            let resp = RpcResponsePayload {
                                status: RpcStatus::Ok,
                                headers: vec![(
                                    HEADER_NRPC_STREAMING.to_string(),
                                    HEADER_NRPC_STREAMING_CONTINUE.to_vec(),
                                )],
                                body: chunk.to_vec(),
                            };
                            // Await per-chunk publish so chunks for
                            // one call_id reach the network in send
                            // order. Without this, two chunks emitted
                            // in tight succession can race into the
                            // publish path and arrive out of order
                            // (or be eclipsed by the terminal frame
                            // and lost entirely on the caller side).
                            pump_emit(caller_origin, call_id, resp).await;
                        }
                    });
                    // Run the handler. Catch panics so a
                    // misbehaving handler can't take down the
                    // runtime — same shape as the unary fold.
                    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                        handler.call(ctx, sink),
                    ))
                    .await;
                    // The handler dropped the sink (either by
                    // returning or by panicking through the
                    // catch_unwind). Wait for the pump to drain
                    // any final in-flight chunks.
                    let _ = pump.await;
                    if let Some(m) = metrics.as_ref() {
                        m.handler_in_flight
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        m.record_handler_duration(handler_started.elapsed());
                        if outcome.is_err() {
                            m.handler_panics_total
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    // Emit the terminal frame. CANCEL-wins ordering
                    // matches the unary fold: if the cancellation
                    // token fired during execution, override the
                    // handler's terminal with `RpcStatus::Cancelled`.
                    let terminal = if cancel_probe.is_cancelled() {
                        RpcResponsePayload {
                            status: RpcStatus::Cancelled,
                            headers: vec![],
                            body: b"server observed CANCEL during streaming handler execution"
                                .to_vec(),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(())) => RpcResponsePayload {
                                status: RpcStatus::Ok,
                                headers: vec![(
                                    HEADER_NRPC_STREAMING.to_string(),
                                    HEADER_NRPC_STREAMING_END.to_vec(),
                                )],
                                body: vec![],
                            },
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: message.into_bytes(),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: message.into_bytes(),
                            },
                            Err(panic) => {
                                let panic_msg = panic
                                    .downcast_ref::<&'static str>()
                                    .map(|s| s.to_string())
                                    .or_else(|| panic.downcast_ref::<String>().cloned())
                                    .unwrap_or_else(|| "<non-string panic>".into());
                                tracing::error!(
                                    caller_origin = format!("{:#x}", caller_origin),
                                    call_id,
                                    panic = %panic_msg,
                                    "rpc streaming server handler panicked",
                                );
                                RpcResponsePayload {
                                    status: RpcStatus::Internal,
                                    headers: vec![],
                                    body: format!("handler panicked: {panic_msg}").into_bytes(),
                                }
                            }
                        }
                    };
                    in_flight.lock().remove(&key);
                    // Drop the per-call flow-control semaphore
                    // (if any) so a stale GRANT arriving after
                    // termination is silently dropped — the entry
                    // is gone, lookup misses.
                    flow_control.lock().remove(&key);
                    // Await the terminal frame's publish too so it
                    // arrives strictly AFTER the last chunk on the
                    // wire (the pump has already drained, but the
                    // emit itself is still async and we must await
                    // it before the spawned task ends).
                    emit(caller_origin, call_id, terminal).await;
                });
            }
            DISPATCH_RPC_CANCEL => {
                if let Some(token) = self.in_flight.lock().remove(&key) {
                    token.cancel();
                }
                // Also drop the flow-control entry — the spawned
                // task's terminal cleanup will run too, but doing
                // it here makes the CANCEL path immediately stop
                // refilling the pump (the pending `acquire().await`
                // will resolve once the semaphore is dropped or
                // when the task exits).
                self.flow_control.lock().remove(&key);
            }
            DISPATCH_RPC_STREAM_GRANT => {
                // Add credit to the per-call semaphore. Silently
                // drop GRANT events for unknown / non-flow-
                // controlled calls — server can't tell whether
                // the caller is racing a terminal vs. sending a
                // grant for a non-flow-controlled stream, and
                // both are harmless to ignore.
                let amount = match decode_stream_grant(&ev.payload[EVENT_META_SIZE..]) {
                    Some(n) => n,
                    None => {
                        tracing::debug!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc streaming server fold: malformed STREAM_GRANT payload",
                        );
                        return Ok(());
                    }
                };
                if amount == 0 {
                    return Ok(());
                }
                if let Some(sem) = self.flow_control.lock().get(&key).cloned() {
                    // Tokio's `Semaphore::add_permits` is bounded
                    // by `MAX_PERMITS = usize::MAX >> 3`. A
                    // misbehaving caller flooding huge grants
                    // would eventually saturate; cap defensively.
                    let safe = (amount as usize).min(usize::MAX >> 4);
                    sem.add_permits(safe);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// ============================================================================
// Client-side fold.
//
// `RpcClientFold` is the symmetric companion of `RpcServerFold`.
// It sees RESPONSE events on the caller's reply channel
// (`<service>.replies.<self_origin>`) and routes each one to the
// matching call's awaiting `oneshot::Receiver` keyed on `call_id`
// (the `EventMeta::seq_or_ts`).
//
// The fold's mutable state (the pending-senders map) is shared
// with the `Mesh::call` API via a clone of the same Arc — so the
// publisher side can `register(call_id)` to stage a receiver
// before publishing the REQUEST, and the fold side can `deliver`
// when the matching RESPONSE arrives.
// ============================================================================

/// One pending entry — either a unary oneshot or a streaming
/// mpsc. The fold dispatches to the right variant based on
/// what's registered for the `call_id`.
enum PendingEntry {
    /// Unary call — exactly one RESPONSE expected. Completes the
    /// oneshot with the decoded payload.
    Unary(tokio::sync::oneshot::Sender<RpcResponsePayload>),
    /// Streaming call — multiple non-terminal `Continue` chunks
    /// followed by one terminal frame. Each non-terminal chunk
    /// pushes a `StreamItem::Chunk(body)` onto the mpsc; the
    /// terminal frame pushes `StreamItem::End` (Ok) or
    /// `StreamItem::Error(payload)` (non-Ok status) and the
    /// pending entry is removed.
    Streaming(tokio::sync::mpsc::UnboundedSender<StreamItem>),
}

/// One item delivered to a streaming caller. The caller's
/// `RpcStream` translates these into `Stream::Item =
/// Result<Bytes, RpcError>` plus stream termination.
#[derive(Debug, Clone)]
pub enum StreamItem {
    /// Non-terminal chunk — a body slice from the server.
    Chunk(bytes::Bytes),
    /// Terminal frame, server signaled clean stream end.
    End,
    /// Terminal frame with a non-`Ok` status. Body is the
    /// server's diagnostic; status is the wire `RpcStatus` value.
    Error(RpcResponsePayload),
}

/// Shared pending-call state. Held by both the `RpcClientFold`
/// (writer side: completes oneshot senders / pushes streaming
/// chunks on RESPONSE arrival) and the `Mesh::call*` APIs (reader
/// side: registers entries before publishing the REQUEST).
/// Concurrent access is mediated by `DashMap`.
///
/// Multiplexes unary AND streaming calls in a single map keyed
/// on `call_id` — the entry's enum variant tells the fold how
/// to dispatch incoming RESPONSE events.
pub struct RpcClientPending {
    senders: dashmap::DashMap<u64, PendingEntry>,
}

impl RpcClientPending {
    /// Construct an empty pending-call store.
    pub fn new() -> Self {
        Self {
            senders: dashmap::DashMap::new(),
        }
    }

    /// Register a oneshot for a unary `call_id`. Returns the
    /// receiver the caller awaits. The caller MUST publish the
    /// REQUEST after registration (and not before) so the
    /// matching RESPONSE can't arrive while the pending entry is
    /// missing.
    ///
    /// If a sender already exists for `call_id` (improperly reused
    /// id), it is replaced and the old receiver gets a
    /// `RecvError::Closed` — surfacing the misuse as a hard error
    /// at the caller rather than silently delivering the response
    /// to the wrong waiter.
    pub fn register(&self, call_id: u64) -> tokio::sync::oneshot::Receiver<RpcResponsePayload> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.senders.insert(call_id, PendingEntry::Unary(tx));
        rx
    }

    /// Register a streaming entry for `call_id`. Returns the
    /// receive end of an mpsc the fold will push chunks onto.
    /// Same registration ordering rules as `register` —
    /// publisher must call this BEFORE publishing the REQUEST.
    pub fn register_streaming(
        &self,
        call_id: u64,
    ) -> tokio::sync::mpsc::UnboundedReceiver<StreamItem> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.senders.insert(call_id, PendingEntry::Streaming(tx));
        rx
    }

    /// Drop the pending entry for `call_id`. Called by the
    /// caller-side cancellation path (e.g. `Mesh::call`'s future
    /// being dropped, the stream being dropped, or a deadline
    /// timer firing). The matching RESPONSE(s) that may still
    /// arrive afterwards are silently discarded by `deliver`.
    pub fn cancel(&self, call_id: u64) {
        self.senders.remove(&call_id);
    }

    /// Deliver `resp` to the waiter for `call_id`, if any.
    ///
    /// For a unary entry: completes the oneshot and removes the
    /// entry.
    ///
    /// For a streaming entry: examines the response's headers to
    /// decide whether it's a non-terminal chunk (`Continue` —
    /// push `StreamItem::Chunk`, keep the entry) or terminal
    /// (`End` / non-`Ok` — push `StreamItem::End` or `Error`,
    /// remove the entry).
    ///
    /// Idempotent on subsequent deliveries to a removed entry.
    fn deliver(&self, call_id: u64, resp: RpcResponsePayload) {
        // Look up the entry — but DON'T remove it yet, because for
        // streaming we may want to keep it for non-terminal chunks.
        // The remove decision is per-variant.
        let entry = self.senders.get(&call_id);
        let Some(entry) = entry else { return };
        match entry.value() {
            PendingEntry::Unary(_) => {
                drop(entry);
                if let Some((_, PendingEntry::Unary(tx))) = self.senders.remove(&call_id) {
                    let _ = tx.send(resp);
                }
            }
            PendingEntry::Streaming(tx) => {
                let kind = classify_streaming_chunk(&resp);
                match kind {
                    StreamingChunkKind::Continue => {
                        // Non-terminal: push the chunk, keep the
                        // entry for future RESPONSE events.
                        let _ = tx.send(StreamItem::Chunk(bytes::Bytes::from(resp.body)));
                    }
                    StreamingChunkKind::Terminal => {
                        // Terminal: classify Ok-end vs Error-end
                        // and remove the entry.
                        let item = if resp.status.is_ok() {
                            // Ok terminal frame: emit a final
                            // chunk if the body is non-empty,
                            // then End.
                            if !resp.body.is_empty() {
                                let _ = tx.send(StreamItem::Chunk(bytes::Bytes::from(resp.body)));
                            }
                            StreamItem::End
                        } else {
                            StreamItem::Error(resp)
                        };
                        let _ = tx.send(item);
                        drop(entry);
                        self.senders.remove(&call_id);
                    }
                    StreamingChunkKind::Unary => {
                        // Streaming entry but unary-shaped
                        // response (no `nrpc-streaming` header,
                        // status Ok). This usually indicates a
                        // server-side bug — the caller opened a
                        // streaming call but the server replied
                        // through the unary path. Warn so
                        // operators can see the mismatch in logs;
                        // treat as terminal end with body as a
                        // single chunk so the caller still gets
                        // the data instead of hanging.
                        tracing::warn!(
                            call_id,
                            body_len = resp.body.len(),
                            "rpc client: streaming consumer received unary-shaped \
                             response (no nrpc-streaming header); server may have \
                             bridged a unary path. Bridging to single-chunk + EOF.",
                        );
                        if !resp.body.is_empty() {
                            let _ = tx.send(StreamItem::Chunk(bytes::Bytes::from(resp.body)));
                        }
                        let _ = tx.send(StreamItem::End);
                        drop(entry);
                        self.senders.remove(&call_id);
                    }
                }
            }
        }
    }

    /// Test-only: how many pending calls are registered. Used by
    /// integration tests to confirm cleanup after happy-path / cancel.
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.senders.len()
    }
}

impl Default for RpcClientPending {
    fn default() -> Self {
        Self::new()
    }
}

/// Client-side fold. Decodes RESPONSE events and routes them to
/// awaiting oneshots in the shared [`RpcClientPending`].
///
/// `Mesh::call` clones the same `Arc<RpcClientPending>` to register
/// oneshots before publishing REQUESTs.
pub struct RpcClientFold {
    pending: Arc<RpcClientPending>,
}

impl RpcClientFold {
    /// Construct a client fold that delivers responses through
    /// `pending`. Typical pattern:
    ///
    /// ```ignore
    /// let pending = Arc::new(RpcClientPending::new());
    /// let fold = RpcClientFold::new(pending.clone());
    /// let adapter = CortexAdapter::open(..., fold, ())?;
    /// // `pending` is still usable for register / cancel.
    /// ```
    pub fn new(pending: Arc<RpcClientPending>) -> Self {
        Self { pending }
    }
}

impl RedexFold<()> for RpcClientFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc client fold: event payload too short for EventMeta; skipping",
            );
            return Ok(());
        };
        // Only RESPONSE events are routed; the caller's reply
        // channel shouldn't carry REQUEST/CANCEL traffic, but if a
        // misconfigured publisher sent some, ignore them rather
        // than killing the fold.
        if meta.dispatch != DISPATCH_RPC_RESPONSE {
            return Ok(());
        }
        match RpcResponsePayload::decode(&ev.payload[EVENT_META_SIZE..]) {
            Ok(resp) => self.pending.deliver(meta.seq_or_ts, resp),
            Err(e) => {
                // Malformed RESPONSE on the reply channel. We can't
                // fabricate a synthetic response (the call_id might
                // be valid; we just can't tell what it was supposed
                // to mean). Log and leave the pending entry intact
                // — the caller's deadline / cancellation path will
                // eventually clean it up.
                tracing::warn!(
                    error = %e,
                    call_id = meta.seq_or_ts,
                    "rpc client fold: malformed response payload",
                );
            }
        }
        Ok(())
    }
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

    /// Regression: encoder bounds. Encoding a service name longer
    /// than `MAX_RPC_SERVICE_NAME_LEN` panics in debug, catching
    /// the programmer error in tests rather than silently writing
    /// a truncated `as u8` length that the receiver decodes as
    /// garbage. The matching debug_asserts guard body length,
    /// header count, header name length, and header value length.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "service name")]
    fn request_encode_panics_on_oversize_service_name() {
        let p = RpcRequestPayload {
            service: "x".repeat(MAX_RPC_SERVICE_NAME_LEN + 1),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        let _ = p.encode();
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "body length")]
    fn request_encode_panics_on_oversize_body() {
        let p = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![0; MAX_RPC_BODY_LEN + 1],
        };
        let _ = p.encode();
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "header name")]
    fn request_encode_panics_on_oversize_header_name() {
        let p = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![("a".repeat(MAX_RPC_HEADER_NAME_LEN + 1), vec![])],
            body: vec![],
        };
        let _ = p.encode();
    }

    /// `encoded_len()` must agree with `encode().len()` for every
    /// payload shape — pin this so a future codec change can't
    /// silently desynchronize the size-budgeting helper from the
    /// actual wire size.
    #[test]
    fn encoded_len_matches_encode_len_for_request_and_response() {
        let req = RpcRequestPayload {
            service: "echo.v1".to_string(),
            deadline_ns: 1_700_000_000_000_000_000,
            flags: FLAG_RPC_PROPAGATE_TRACE,
            headers: vec![
                header("traceparent", b"00-aabb"),
                header("idempotency-key", &7u64.to_le_bytes()),
            ],
            body: b"{\"hello\":\"world\"}".to_vec(),
        };
        assert_eq!(req.encoded_len(), req.encode().len());

        let resp = RpcResponsePayload {
            status: RpcStatus::Application(0x8001),
            headers: vec![header("content-type", b"application/json")],
            body: b"ok".to_vec(),
        };
        assert_eq!(resp.encoded_len(), resp.encode().len());

        // Empty edge cases.
        let empty_req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        assert_eq!(empty_req.encoded_len(), empty_req.encode().len());
        let empty_resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        };
        assert_eq!(empty_resp.encoded_len(), empty_resp.encode().len());
    }

    /// Bit 0 of `RpcRequestPayload::flags` is reserved (was the
    /// removed `FLAG_RPC_IDEMPOTENT`). Pin: live flag constants
    /// must NOT collide with bit 0, so a future re-add can safely
    /// reuse it without breaking existing senders.
    #[test]
    fn flag_bit_assignments_leave_idempotent_slot_reserved() {
        // Bit 0 (1 << 0) is reserved; the live flags occupy higher bits.
        assert_eq!(FLAG_RPC_STREAMING_RESPONSE, 1 << 1);
        assert_eq!(FLAG_RPC_PROPAGATE_TRACE, 1 << 2);
        assert_eq!(FLAG_RPC_STREAMING_RESPONSE & 1, 0);
        assert_eq!(FLAG_RPC_PROPAGATE_TRACE & 1, 0);
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
            flags: FLAG_RPC_PROPAGATE_TRACE,
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

    // ====================================================================
    // RpcServerFold — server-side dispatch behavior.
    //
    // These tests drive the fold directly with synthetic events
    // and observe the emitter callback. The end-to-end story
    // (Mesh::serve_rpc + bus + cortex adapter) is integration-
    // tested separately once the glue layer lands.
    // ====================================================================

    use super::super::super::redex::{RedexEntry, RedexEvent};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    /// Captured-response store. Test-local typedef so the
    /// `capturing_emitter` signature stays under the `clippy::
    /// type_complexity` lint.
    type CapturedResponses = Arc<Mutex<Vec<(u64, u64, RpcResponsePayload)>>>;

    /// Build a synthetic RedexEvent carrying an RPC request payload.
    /// Tests use this to drive the fold without going through the
    /// real ingest/cortex pipeline.
    fn rpc_request_event(
        caller_origin: u64,
        call_id: u64,
        payload: RpcRequestPayload,
    ) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, caller_origin, call_id, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&payload.encode());
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    fn rpc_cancel_event(caller_origin: u64, call_id: u64) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, caller_origin, call_id, 0);
        let buf = meta.to_bytes().to_vec();
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// Captures responses emitted by the fold for assertion in tests.
    fn capturing_emitter() -> (RpcResponseEmitter, CapturedResponses) {
        let captured: CapturedResponses = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let emit: RpcResponseEmitter = Arc::new(move |origin, call_id, resp| {
            captured_clone.lock().push((origin, call_id, resp));
        });
        (emit, captured)
    }

    /// A handler that just echoes the request body back as the
    /// response body, with `RpcStatus::Ok`.
    struct EchoHandler;
    #[async_trait::async_trait]
    impl RpcHandler for EchoHandler {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: ctx.payload.body,
            })
        }
    }

    /// Wait until `pred` is true, polling at 10ms intervals up to
    /// `timeout`. Used to await spawned-handler completion in tests
    /// without a sleep-and-pray.
    async fn wait_until<F: Fn() -> bool>(pred: F, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        pred()
    }

    /// Happy path: a REQUEST event triggers the handler; the fold
    /// emits a RESPONSE with the handler's payload.
    #[tokio::test]
    async fn server_fold_request_invokes_handler_and_emits_response() {
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(EchoHandler), emit);
        let req = RpcRequestPayload {
            service: "echo".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: b"hello".to_vec(),
        };
        let ev = rpc_request_event(0xCAFE, 7, req);
        fold.apply(&ev, &mut ()).unwrap();

        // Handler runs in tokio::spawn; wait for the emit.
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected one emitted response"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (origin, call_id, resp) = &captured[0];
        assert_eq!(*origin, 0xCAFE);
        assert_eq!(*call_id, 7);
        assert_eq!(resp.status, RpcStatus::Ok);
        assert_eq!(resp.body, b"hello");
        // In-flight set is cleaned up after the handler completes.
        assert!(fold.in_flight_keys().is_empty());
    }

    /// Application error: handler returns
    /// `RpcHandlerError::Application` → fold emits a response with
    /// `RpcStatus::Application(code)` and the message as body.
    #[tokio::test]
    async fn server_fold_application_error_maps_to_application_status() {
        struct AppErrHandler;
        #[async_trait::async_trait]
        impl RpcHandler for AppErrHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                Err(RpcHandlerError::Application {
                    code: 0xBEEF,
                    message: "bad input".to_string(),
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(AppErrHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await);
        let captured = captured.lock();
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Application(0xBEEF));
        assert_eq!(resp.body, b"bad input");
    }

    /// Internal error: handler returns `RpcHandlerError::Internal`
    /// → fold emits `RpcStatus::Internal` with the message body.
    #[tokio::test]
    async fn server_fold_internal_error_maps_to_internal_status() {
        struct IntErrHandler;
        #[async_trait::async_trait]
        impl RpcHandler for IntErrHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                Err(RpcHandlerError::Internal("db timeout".to_string()))
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(IntErrHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await);
        let captured = captured.lock();
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Internal);
        assert_eq!(resp.body, b"db timeout");
    }

    /// Handler panic: caught by the fold's `catch_unwind`; surfaces
    /// as `RpcStatus::Internal` to the caller. Pre-fix the panic
    /// would propagate up the spawned task, log a tokio
    /// uncaught-panic message, and silently leave the caller
    /// waiting forever.
    #[tokio::test]
    async fn server_fold_handler_panic_surfaces_as_internal_status() {
        struct PanicHandler;
        #[async_trait::async_trait]
        impl RpcHandler for PanicHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                panic!("kaboom");
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(PanicHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await);
        let captured = captured.lock();
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Internal);
        assert!(
            String::from_utf8_lossy(&resp.body).contains("kaboom"),
            "panic message must surface in body, got {}",
            String::from_utf8_lossy(&resp.body),
        );
    }

    /// Deadline already passed: server short-circuits with
    /// `Timeout` without invoking the handler. Pinned via the
    /// `with_test_now_ns` clock override so the test doesn't race
    /// wall time.
    #[tokio::test]
    async fn server_fold_deadline_already_passed_short_circuits_to_timeout() {
        let invoked = Arc::new(AtomicBool::new(false));
        struct CountingHandler {
            invoked: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for CountingHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                self.invoked.store(true, Ordering::Release);
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: vec![],
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(CountingHandler {
                invoked: invoked.clone(),
            }),
            emit,
        )
        // Use a clock value > DEADLINE_SKEW_TOLERANCE_NS + 1
        // (10s + 1ns) so the deadline-passed check fires past the
        // skew tolerance window. With now=20s and deadline=1ns,
        // (now - 10s) > 1ns.
        .with_test_now_ns(20_000_000_000);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            // Deadline well in the past — past the skew tolerance.
            deadline_ns: 1_000,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        // Emit happens synchronously in the deadline-passed branch
        // (no handler spawn).
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Timeout);
        assert!(
            !invoked.load(Ordering::Acquire),
            "handler must NOT be invoked when deadline already passed",
        );
    }

    /// Regression: a deadline that has elapsed by less than
    /// `DEADLINE_SKEW_TOLERANCE_NS` does NOT short-circuit. A
    /// peer with a slightly-fast clock would otherwise be
    /// prematurely timed out before the handler ever ran.
    #[tokio::test]
    async fn server_fold_deadline_within_skew_tolerance_invokes_handler() {
        let invoked = Arc::new(AtomicBool::new(false));
        struct CountingHandler {
            invoked: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for CountingHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                self.invoked.store(true, Ordering::Release);
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: vec![],
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(CountingHandler {
                invoked: invoked.clone(),
            }),
            emit,
        )
        // now = 100s, deadline = 95s → elapsed = 5s, within the
        // 10s skew tolerance.
        .with_test_now_ns(100_000_000_000);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 95_000_000_000,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(
            wait_until(|| invoked.load(Ordering::Acquire), Duration::from_secs(1)).await,
            "handler must run when deadline is within skew tolerance",
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].2.status, RpcStatus::Ok);
    }

    /// CANCEL flips the matching in-flight token. The handler that
    /// `select!`s on the cancellation observes the signal and can
    /// short-circuit. The fold removes the in-flight entry on
    /// CANCEL.
    #[tokio::test]
    async fn server_fold_cancel_flips_token_and_clears_in_flight() {
        let resumed_after_cancel = Arc::new(AtomicBool::new(false));
        struct CancelObservingHandler {
            resumed: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for CancelObservingHandler {
            async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => {
                        self.resumed.store(true, Ordering::Release);
                        Err(RpcHandlerError::Internal("cancelled by caller".to_string()))
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        Ok(RpcResponsePayload {
                            status: RpcStatus::Ok,
                            headers: vec![],
                            body: b"slept the full window".to_vec(),
                        })
                    }
                }
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(CancelObservingHandler {
                resumed: resumed_after_cancel.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 42, req), &mut ()).unwrap();
        // Wait until the handler's `select!` is parked; then send
        // CANCEL.
        assert!(
            wait_until(
                || fold.in_flight_keys().contains(&(1, 42)),
                Duration::from_secs(1)
            )
            .await
        );
        fold.apply(&rpc_cancel_event(1, 42), &mut ()).unwrap();
        // The cancellation is observed by the handler. Even though
        // the handler returns `Internal("cancelled by caller")`,
        // the fold's CANCEL-wins ordering overrides the response
        // with `RpcStatus::Cancelled` so the caller sees the
        // documented status code rather than the handler's
        // accidental Internal payload.
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "handler should observe cancellation and emit response"
        );
        assert!(
            resumed_after_cancel.load(Ordering::Acquire),
            "handler must observe cancellation"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (_, _, resp) = &captured[0];
        assert_eq!(
            resp.status,
            RpcStatus::Cancelled,
            "CANCEL must override handler outcome with RpcStatus::Cancelled"
        );
        // CANCEL also removes the in-flight entry directly.
        // Handler completion removes it again (idempotent).
        assert!(fold.in_flight_keys().is_empty());
    }

    /// Regression: a duplicate REQUEST for an already-in-flight
    /// `(origin_hash, call_id)` must be refused with a synthetic
    /// `Internal` response and must NOT spawn a second handler.
    /// Without the refusal, two handlers race under the same key
    /// and CANCEL handling is broken (CANCEL removes the entry
    /// the first handler reinserts, etc.).
    #[tokio::test]
    async fn server_fold_duplicate_request_refuses_without_double_dispatch() {
        let invocations = Arc::new(AtomicUsize::new(0));
        struct CountingHandler {
            invocations: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for CountingHandler {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                self.invocations.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: b"done".to_vec(),
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(CountingHandler {
                invocations: invocations.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        // First REQUEST — handler spawns and parks in sleep.
        fold.apply(&rpc_request_event(1, 99, req.clone()), &mut ())
            .unwrap();
        assert!(
            wait_until(
                || fold.in_flight_keys().contains(&(1, 99)),
                Duration::from_secs(1)
            )
            .await
        );
        // Second REQUEST with same key — must be refused
        // synchronously with a synthetic Internal response.
        fold.apply(&rpc_request_event(1, 99, req), &mut ()).unwrap();
        // The refusal emit happens synchronously in the fold's
        // sync emitter path.
        let after_dup = captured.lock().clone();
        assert_eq!(
            after_dup.len(),
            1,
            "duplicate REQUEST must emit exactly one synthetic refusal",
        );
        assert_eq!(after_dup[0].2.status, RpcStatus::Internal);
        assert!(String::from_utf8_lossy(&after_dup[0].2.body).contains("duplicate"));
        // Wait for the first handler to complete.
        assert!(
            wait_until(|| captured.lock().len() == 2, Duration::from_secs(2)).await,
            "first handler should still complete normally"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 2);
        // The first handler's response is the second emit (Ok).
        assert_eq!(captured[1].2.status, RpcStatus::Ok);
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate REQUEST must NOT spawn a second handler",
        );
    }

    /// Regression: a CANCEL that fires while the handler is mid-
    /// flight must override the handler's outcome with
    /// `RpcStatus::Cancelled` even when the handler ignores
    /// cancellation and returns `Ok(...)`. Without this, a caller
    /// who cancelled would see the handler's accidental success
    /// payload and could not tell whether their CANCEL won.
    #[tokio::test]
    async fn server_fold_cancel_overrides_handler_ok_with_cancelled_status() {
        struct IgnoresCancellation;
        #[async_trait::async_trait]
        impl RpcHandler for IgnoresCancellation {
            async fn call(&self, _ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                // Sleep long enough for the test to send CANCEL,
                // then return Ok regardless. This models a handler
                // that doesn't `select!` on `ctx.cancellation`.
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: b"finished despite cancellation".to_vec(),
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(IgnoresCancellation), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(7, 11, req), &mut ()).unwrap();
        // Wait until the handler is parked, then send CANCEL well
        // before the handler's sleep elapses.
        assert!(
            wait_until(
                || fold.in_flight_keys().contains(&(7, 11)),
                Duration::from_secs(1)
            )
            .await
        );
        fold.apply(&rpc_cancel_event(7, 11), &mut ()).unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "handler should complete and emit response"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (_, _, resp) = &captured[0];
        assert_eq!(
            resp.status,
            RpcStatus::Cancelled,
            "handler that returned Ok despite CANCEL must surface as Cancelled"
        );
        assert!(fold.in_flight_keys().is_empty());
    }

    /// CANCEL for an unknown call_id is a no-op (no panic, no
    /// stray emission). This is the case where a CANCEL races a
    /// handler completion or a duplicate CANCEL arrives.
    #[tokio::test]
    async fn server_fold_cancel_for_unknown_call_id_is_no_op() {
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(EchoHandler), emit);
        // CANCEL with no matching REQUEST.
        fold.apply(&rpc_cancel_event(1, 999), &mut ()).unwrap();
        assert!(captured.lock().is_empty());
        assert!(fold.in_flight_keys().is_empty());
    }

    /// Malformed request payload: fold emits a
    /// `RpcStatus::UnknownVersion` response and continues. A
    /// regression that returned `Err` here would kill the cortex
    /// adapter's tail-and-fold task on the first malformed event,
    /// which is the wrong behavior for an RPC server that needs
    /// to keep serving past garbage.
    #[tokio::test]
    async fn server_fold_malformed_payload_emits_unknown_version_and_keeps_going() {
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(EchoHandler), emit);
        // Build an event with valid meta but a garbage tail (just
        // a single 0x00 byte, which fails the service-len check).
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 7, 1, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.push(0x00); // svc_len = 0 → empty service → Truncated
        let ev = RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        };
        let result = fold.apply(&ev, &mut ());
        assert!(
            result.is_ok(),
            "fold must NOT return Err on malformed payload (would kill the adapter); got {result:?}"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::UnknownVersion);
    }

    /// Cancellation token roundtrip: `cancel()` sets `is_cancelled`
    /// and wakes a parked `cancelled().await`.
    #[tokio::test]
    async fn cancellation_token_signals_waiters() {
        let token = RpcCancellationToken::new();
        assert!(!token.is_cancelled());
        let token2 = token.clone();
        let waiter = tokio::spawn(async move {
            token2.cancelled().await;
        });
        // Give the waiter a chance to park.
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel();
        // Waiter wakes.
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter must wake within 1s")
            .expect("waiter task must not panic");
        assert!(token.is_cancelled());
    }

    // ====================================================================
    // W3C Trace Context propagation.
    // ====================================================================

    /// `build_trace_headers` + `extract_trace_context` round-trip
    /// a typical W3C trace context through the request headers.
    #[test]
    fn trace_context_round_trips_through_headers() {
        let tc = TraceContext {
            traceparent: "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
            tracestate: "vendor1=opaque-value,vendor2=other".to_string(),
        };
        let headers = build_trace_headers(&tc);
        assert_eq!(headers.len(), 2, "non-empty tracestate emits both headers");
        let extracted = extract_trace_context(&headers).expect("must extract");
        assert_eq!(extracted, tc);
    }

    /// Regression for M21: `extract_trace_context` does
    /// case-INsensitive matching on the header names, matching the
    /// W3C and HTTP conventions. A peer that emits capitalized
    /// `Traceparent` or `TRACESTATE` must still be picked up — the
    /// previous implementation used `name.as_str() == "traceparent"`
    /// and silently dropped any non-lowercase variant.
    #[test]
    fn extract_trace_context_is_case_insensitive_on_header_names() {
        // Capital-T traceparent + uppercase TRACESTATE — both must
        // be picked up by the extractor.
        let headers = vec![
            (
                "Traceparent".to_string(),
                b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_vec(),
            ),
            ("TRACESTATE".to_string(), b"vendor=value".to_vec()),
        ];
        let extracted =
            extract_trace_context(&headers).expect("capital-T traceparent must be recognized");
        assert_eq!(
            extracted.traceparent,
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        );
        assert_eq!(extracted.tracestate, "vendor=value");

        // Mixed-case still works.
        let headers = vec![
            ("traceParent".to_string(), b"00-aa-bb-01".to_vec()),
            ("TraceState".to_string(), b"v=1".to_vec()),
        ];
        let extracted =
            extract_trace_context(&headers).expect("mixed-case traceparent must be recognized");
        assert_eq!(extracted.traceparent, "00-aa-bb-01");
        assert_eq!(extracted.tracestate, "v=1");
    }

    /// Empty `tracestate` is omitted on the wire (W3C convention)
    /// but extracted as empty on the receive side.
    #[test]
    fn trace_context_empty_tracestate_omitted_from_wire() {
        let tc = TraceContext {
            traceparent: "00-aa-bb-01".to_string(),
            tracestate: String::new(),
        };
        let headers = build_trace_headers(&tc);
        assert_eq!(
            headers.len(),
            1,
            "empty tracestate must NOT be emitted on the wire",
        );
        assert_eq!(headers[0].0, "traceparent");
        let extracted = extract_trace_context(&headers).expect("must extract");
        assert_eq!(extracted.traceparent, "00-aa-bb-01");
        assert_eq!(extracted.tracestate, "");
    }

    /// Headers without `traceparent` decode as `None`. Useful for
    /// the FLAG_RPC_PROPAGATE_TRACE-set-but-no-headers misuse
    /// case — the server gets `None` rather than a bogus context.
    #[test]
    fn trace_context_missing_traceparent_returns_none() {
        let headers = vec![
            ("content-type".to_string(), b"application/json".to_vec()),
            ("idempotency-key".to_string(), b"abc".to_vec()),
        ];
        assert!(extract_trace_context(&headers).is_none());
    }

    /// Server fold populates `RpcContext::trace_context` only when
    /// the caller signals `FLAG_RPC_PROPAGATE_TRACE`. End-to-end
    /// through the fold's apply path.
    #[tokio::test]
    async fn server_fold_propagates_trace_context_via_flag() {
        struct CapturingHandler {
            captured: Arc<Mutex<Option<Option<TraceContext>>>>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for CapturingHandler {
            async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                *self.captured.lock() = Some(ctx.trace_context.clone());
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: vec![],
                })
            }
        }

        // Helper: run one request through a fresh fold and return
        // what the handler captured for trace_context.
        async fn run(req: RpcRequestPayload) -> Option<TraceContext> {
            let captured: Arc<Mutex<Option<Option<TraceContext>>>> = Arc::new(Mutex::new(None));
            let (emit, _captured_responses) = capturing_emitter();
            let handler = Arc::new(CapturingHandler {
                captured: captured.clone(),
            });
            let mut fold = RpcServerFold::new(handler, emit);
            fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
            // Wait for the spawned handler to finish.
            assert!(
                wait_until(|| captured.lock().is_some(), Duration::from_secs(2)).await,
                "handler must run"
            );
            let observed = captured.lock().take().unwrap();
            observed
        }

        // Case 1: FLAG_RPC_PROPAGATE_TRACE NOT set → trace_context is None.
        let req_no_flag = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![("traceparent".to_string(), b"00-aa-bb-01".to_vec())],
            body: vec![],
        };
        assert!(
            run(req_no_flag).await.is_none(),
            "without the flag, server must NOT extract trace_context"
        );

        // Case 2: FLAG set + headers present → server gets the context.
        let tc = TraceContext {
            traceparent: "00-trace-span-01".to_string(),
            tracestate: "vendor=value".to_string(),
        };
        let req_with_flag = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_PROPAGATE_TRACE,
            headers: build_trace_headers(&tc),
            body: vec![],
        };
        let observed = run(req_with_flag).await.expect("flag set → should be Some");
        assert_eq!(observed, tc);

        // Case 3: FLAG set but headers missing → None (defensive).
        let req_flag_no_headers = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_PROPAGATE_TRACE,
            headers: vec![],
            body: vec![],
        };
        assert!(
            run(req_flag_no_headers).await.is_none(),
            "flag set but no headers → server gets None (no synthesis)"
        );
    }

    /// Race: cancel fires AFTER the future is registered but
    /// BEFORE the await actually parks. The token's
    /// `notified()`-then-check ordering must catch this case
    /// without sleeping past the cancellation.
    #[tokio::test]
    async fn cancellation_token_does_not_miss_cancel_racing_register() {
        for _ in 0..50 {
            let token = RpcCancellationToken::new();
            let token2 = token.clone();
            let waiter = tokio::spawn(async move {
                token2.cancelled().await;
            });
            // No sleep — fire cancel as fast as possible against
            // the just-spawned waiter. In the worst case the
            // waiter has not yet reached `notified()`; it will see
            // `is_cancelled() == true` on its first check and
            // return immediately. In the other case it parks and
            // gets woken by `notify_waiters`.
            token.cancel();
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("waiter must complete within 1s")
                .expect("waiter task must not panic");
        }
    }

    // ====================================================================
    // RpcClientFold — caller-side response routing.
    // ====================================================================

    fn rpc_response_event(
        caller_origin: u64,
        call_id: u64,
        payload: RpcResponsePayload,
    ) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, caller_origin, call_id, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&payload.encode());
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// Happy path: register a call, drive the matching RESPONSE
    /// through the fold, the awaiting receiver gets the payload.
    #[tokio::test]
    async fn client_fold_routes_response_to_registered_waiter() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let rx = pending.register(42);
        assert_eq!(pending.pending_count(), 1);

        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: b"hello back".to_vec(),
        };
        fold.apply(&rpc_response_event(0xCAFE, 42, resp.clone()), &mut ())
            .unwrap();

        // Receiver is completed.
        let got = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("receiver must complete within 1s")
            .expect("sender must not be dropped");
        assert_eq!(got, resp);
        // Pending entry cleared after delivery.
        assert_eq!(pending.pending_count(), 0);
    }

    /// RESPONSE for an unknown call_id is a no-op (no panic, no
    /// stray side effect). This is the case where a stale RESPONSE
    /// arrives after the caller has cancelled or timed out.
    #[tokio::test]
    async fn client_fold_response_for_unknown_call_id_is_no_op() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_response_event(1, 999, resp), &mut ())
            .unwrap();
        assert_eq!(pending.pending_count(), 0);
    }

    /// REQUEST / CANCEL events on the reply channel are ignored
    /// rather than producing a stray decode-error or affecting
    /// pending state. The reply channel shouldn't carry these in
    /// practice (they belong on `<service>.requests`), but a
    /// misconfigured publisher must not break the fold.
    #[tokio::test]
    async fn client_fold_ignores_non_response_dispatches() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let _rx = pending.register(7);

        // REQUEST event landing on the caller's reply channel is
        // ignored.
        let req = RpcRequestPayload {
            service: "stray".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 7, req), &mut ()).unwrap();
        // Pending entry untouched.
        assert_eq!(pending.pending_count(), 1);

        // CANCEL on the reply channel: also ignored.
        fold.apply(&rpc_cancel_event(1, 7), &mut ()).unwrap();
        assert_eq!(pending.pending_count(), 1);
    }

    /// `cancel(call_id)` removes the pending entry; a subsequent
    /// RESPONSE for that call_id is dropped silently.
    #[tokio::test]
    async fn client_pending_cancel_drops_subsequent_response() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let rx = pending.register(5);
        pending.cancel(5);
        assert_eq!(pending.pending_count(), 0);

        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_response_event(1, 5, resp), &mut ())
            .unwrap();

        // Receiver was dropped along with the cancel. The previously-
        // returned `rx` errors with `Closed`.
        let result = tokio::time::timeout(Duration::from_secs(1), rx).await;
        let inner = result.expect("must complete within 1s");
        assert!(
            inner.is_err(),
            "receiver after cancel must error (sender dropped)",
        );
    }

    /// Malformed RESPONSE payload: fold returns Ok (does not kill
    /// the cortex adapter) and leaves the pending entry intact for
    /// the caller's deadline / cancellation path to clean up. Pre-
    /// fix a bad payload could either kill the fold or fabricate a
    /// synthetic response — both wrong.
    #[tokio::test]
    async fn client_fold_malformed_response_is_logged_not_fatal() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let rx = pending.register(11);

        // Build a malformed RESPONSE: valid meta, garbage tail
        // (just `[0xFF]`, which is shorter than the required 2-byte
        // status + 1-byte headers count + 4-byte body length).
        let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 1, 11, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.push(0xFF);
        let ev = RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        };
        let result = fold.apply(&ev, &mut ());
        assert!(
            result.is_ok(),
            "fold must not return Err on malformed response"
        );
        // Pending entry NOT cleared — the caller's cancellation
        // path will eventually clean it up via `cancel(call_id)`.
        assert_eq!(pending.pending_count(), 1);
        // Receiver is still pending (not delivered, not closed).
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rx)
                .await
                .is_err(),
            "receiver should still be parked (no delivery, no drop)",
        );
    }

    /// Re-registering the same call_id replaces the prior sender;
    /// the prior `Receiver` errors with `RecvError::Closed`. This
    /// is the misuse-detection path — call_ids should be unique
    /// per (caller, target) for the lifetime of the call, and a
    /// clash surfaces as a hard error rather than silently
    /// delivering the response to the wrong waiter.
    #[tokio::test]
    async fn client_pending_re_register_closes_prior_receiver() {
        let pending = Arc::new(RpcClientPending::new());
        let rx_a = pending.register(99);
        let _rx_b = pending.register(99);
        // The first receiver is now closed (sender dropped on
        // re-insert).
        let result = tokio::time::timeout(Duration::from_secs(1), rx_a).await;
        let inner = result.expect("must complete within 1s");
        assert!(inner.is_err(), "re-register must close prior receiver");
        assert_eq!(pending.pending_count(), 1);
    }

    // ====================================================================
    // RpcServerStreamingFold — coverage for the multi-fire emit path.
    //
    // The streaming fold is the most complex code in this file:
    //   - Per-call cancellation token (same as unary)
    //   - Pump task that drains an mpsc and awaits each emit to
    //     enforce per-call ordering
    //   - Optional flow-control semaphore (caller-set window +
    //     STREAM_GRANT credit refills)
    //   - Terminal-frame emission with CANCEL-wins override
    //
    // These tests pin each branch: ordered chunks + clean EOF;
    // application error after partial stream; panic surfacing as
    // Internal; CANCEL flipping the cancellation token AND being
    // surfaced as the terminal status; STREAM_GRANT permits;
    // duplicate-REQUEST refusal.
    // ====================================================================

    /// Build an async `RpcAsyncResponseEmitter` that captures every
    /// emit into a shared Vec. Streaming fold tests use this to
    /// inspect the multi-frame emit pattern.
    fn capturing_async_emitter() -> (RpcAsyncResponseEmitter, CapturedResponses) {
        let captured: CapturedResponses = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let emit: RpcAsyncResponseEmitter = Arc::new(move |origin, call_id, resp| {
            let captured_clone = captured_clone.clone();
            Box::pin(async move {
                captured_clone.lock().push((origin, call_id, resp));
            })
        });
        (emit, captured)
    }

    /// Synthesize a STREAM_GRANT event for a `(caller_origin, call_id)`
    /// asking for `n` additional credits.
    fn rpc_stream_grant_event(caller_origin: u64, call_id: u64, n: u32) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_STREAM_GRANT, 0, caller_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + 4);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&encode_stream_grant(n));
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// Streaming handler that emits N chunks and returns Ok. The
    /// caller-side test asserts (a) all N chunks arrive in order
    /// with the `nrpc-streaming: continue` header, (b) a final
    /// terminal frame with `nrpc-streaming: end` follows.
    #[tokio::test]
    async fn streaming_fold_emits_chunks_in_order_and_clean_terminal() {
        struct CountingHandler {
            n: usize,
        }
        #[async_trait::async_trait]
        impl RpcStreamingHandler for CountingHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                for i in 0..self.n {
                    sink.send(format!("chunk-{i}").into_bytes());
                }
                Ok(())
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(CountingHandler { n: 5 }), emit);
        let req = RpcRequestPayload {
            service: "stream".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(11, 22, req), &mut ())
            .unwrap();
        // 5 continue chunks + 1 terminal end frame.
        assert!(
            wait_until(|| captured.lock().len() == 6, Duration::from_secs(2)).await,
            "expected 6 frames (5 chunks + terminal end), got {}",
            captured.lock().len(),
        );
        let captured = captured.lock();
        for (i, (_, _, resp)) in captured.iter().take(5).enumerate() {
            assert_eq!(resp.status, RpcStatus::Ok);
            // continue header on every non-terminal chunk
            let hdr = resp
                .headers
                .iter()
                .find(|(n, _)| n == HEADER_NRPC_STREAMING)
                .expect("streaming header present");
            assert_eq!(hdr.1.as_slice(), HEADER_NRPC_STREAMING_CONTINUE);
            assert_eq!(resp.body, format!("chunk-{i}").into_bytes());
        }
        // Terminal frame
        let (_, _, term) = captured.last().unwrap();
        assert_eq!(term.status, RpcStatus::Ok);
        let hdr = term
            .headers
            .iter()
            .find(|(n, _)| n == HEADER_NRPC_STREAMING)
            .expect("terminal must have streaming header");
        assert_eq!(hdr.1.as_slice(), HEADER_NRPC_STREAMING_END);
        assert!(term.body.is_empty());
    }

    /// Handler returns `Err(Internal)` after sending 2 chunks. Caller
    /// must see (a) both chunks with the continue header, (b) a
    /// terminal frame carrying `RpcStatus::Internal` (NOT the end
    /// marker — the terminal-error path drops the header).
    #[tokio::test]
    async fn streaming_fold_terminal_error_after_partial_stream() {
        struct PartialErrHandler;
        #[async_trait::async_trait]
        impl RpcStreamingHandler for PartialErrHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                sink.send(b"first".to_vec());
                sink.send(b"second".to_vec());
                Err(RpcHandlerError::Internal("ran out of fuel".into()))
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(PartialErrHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(
            wait_until(|| captured.lock().len() == 3, Duration::from_secs(2)).await,
            "expected 2 chunks + 1 terminal error",
        );
        let captured = captured.lock();
        assert_eq!(captured[0].2.body, b"first");
        assert_eq!(captured[1].2.body, b"second");
        let (_, _, term) = &captured[2];
        assert_eq!(term.status, RpcStatus::Internal);
        assert!(
            String::from_utf8_lossy(&term.body).contains("ran out of fuel"),
            "diagnostic must round-trip, got {:?}",
            String::from_utf8_lossy(&term.body),
        );
    }

    /// Handler panics. The fold's `catch_unwind` surfaces it as a
    /// terminal `RpcStatus::Internal` rather than killing the
    /// runtime.
    #[tokio::test]
    async fn streaming_fold_handler_panic_surfaces_as_internal_terminal() {
        struct PanicHandler;
        #[async_trait::async_trait]
        impl RpcStreamingHandler for PanicHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                _sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                panic!("kaboom in streaming handler");
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(PanicHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 2, req), &mut ()).unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "panic must surface as a terminal frame",
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Internal);
        assert!(
            String::from_utf8_lossy(&resp.body).contains("kaboom"),
            "panic message must surface, got {:?}",
            String::from_utf8_lossy(&resp.body),
        );
    }

    /// CANCEL during a streaming call overrides the terminal frame
    /// with `RpcStatus::Cancelled` — same CANCEL-wins ordering as
    /// the unary fold.
    #[tokio::test]
    async fn streaming_fold_cancel_overrides_terminal_with_cancelled() {
        struct CooperativeHandler;
        #[async_trait::async_trait]
        impl RpcStreamingHandler for CooperativeHandler {
            async fn call(
                &self,
                ctx: RpcContext,
                sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                sink.send(b"chunk-0".to_vec());
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(5)) => Ok(()),
                }
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(CooperativeHandler), emit);
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(7, 13, req), &mut ()).unwrap();
        // Wait until at least the first chunk is captured AND the
        // handler is parked (in_flight key present), then CANCEL.
        assert!(
            wait_until(
                || !captured.lock().is_empty() && fold.in_flight_keys().contains(&(7, 13)),
                Duration::from_secs(2)
            )
            .await
        );
        fold.apply(&rpc_cancel_event(7, 13), &mut ()).unwrap();
        // Wait for the terminal frame.
        assert!(
            wait_until(|| captured.lock().len() >= 2, Duration::from_secs(2)).await,
            "expected first chunk + terminal frame",
        );
        let captured = captured.lock();
        // First emit was the chunk; the LAST should be the
        // Cancelled terminal.
        assert_eq!(
            captured.last().unwrap().2.status,
            RpcStatus::Cancelled,
            "CANCEL must override terminal status",
        );
    }

    /// Duplicate REQUEST with the same `(origin, call_id)` is
    /// refused with a synthetic Internal terminal frame and does
    /// NOT spawn a second handler. Mirror of the unary fold's
    /// regression at server_fold_duplicate_request_refuses_*.
    #[tokio::test]
    async fn streaming_fold_duplicate_request_refuses_without_double_dispatch() {
        let invocations = Arc::new(AtomicUsize::new(0));
        struct CountingHandler {
            invocations: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl RpcStreamingHandler for CountingHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                self.invocations.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(80)).await;
                sink.send(b"chunk".to_vec());
                Ok(())
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(
            Arc::new(CountingHandler {
                invocations: invocations.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: vec![],
        };
        fold.apply(&rpc_request_event(1, 99, req.clone()), &mut ())
            .unwrap();
        assert!(
            wait_until(
                || fold.in_flight_keys().contains(&(1, 99)),
                Duration::from_secs(1)
            )
            .await
        );
        // Duplicate REQUEST — must emit a synthetic Internal
        // terminal and not invoke the handler a second time.
        fold.apply(&rpc_request_event(1, 99, req), &mut ()).unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(1)).await,
            "synthetic refusal should be emitted",
        );
        // First emit (chronologically) is the synthetic refusal.
        let refusal = captured.lock()[0].clone();
        assert_eq!(refusal.2.status, RpcStatus::Internal);
        assert!(String::from_utf8_lossy(&refusal.2.body).contains("duplicate"));
        // Wait for the original handler to complete (chunk + terminal).
        assert!(
            wait_until(|| captured.lock().len() >= 3, Duration::from_secs(2)).await,
            "first handler should still complete normally",
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate REQUEST must NOT spawn a second handler",
        );
    }

    /// STREAM_GRANT for an unknown call_id is silently dropped
    /// (no panic, no tracing event escalation). Pin the
    /// always-safe behavior so a misbehaving caller (or a CANCEL/
    /// GRANT race) can't crash the fold.
    #[tokio::test]
    async fn streaming_fold_grant_for_unknown_call_id_is_no_op() {
        struct NoopHandler;
        #[async_trait::async_trait]
        impl RpcStreamingHandler for NoopHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                _sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                Ok(())
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(NoopHandler), emit);
        let result = fold.apply(&rpc_stream_grant_event(99, 42, 5), &mut ());
        assert!(result.is_ok(), "GRANT for unknown call_id must be Ok");
        assert!(captured.lock().is_empty(), "no emit for unknown GRANT");
    }

    /// Regression for M20: the streaming pump's mpsc is bounded
    /// at `STREAMING_PUMP_CAPACITY`. A handler that produces
    /// chunks faster than the pump drains gets its excess
    /// `sink.send(...)` calls silently dropped (matching the
    /// "caller cancelled" semantic) — and the metric counter
    /// `streaming_chunks_dropped_total` increments.
    ///
    /// We construct the sink directly with a tiny bounded mpsc
    /// (capacity 2) and a metrics handle, then call `send` 5
    /// times without a receiver. The first 2 fit in the channel;
    /// the next 3 are dropped and counted.
    #[tokio::test]
    async fn streaming_sink_drops_on_full_and_increments_metric() {
        use crate::adapter::net::mesh_rpc_metrics::{RpcMetricsRegistry, ServiceMetricsAtomic};
        // Tiny channel to make overflow easy to observe.
        let (tx, _rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(2);
        let registry = RpcMetricsRegistry::new();
        let metrics: Arc<ServiceMetricsAtomic> = registry.for_service("drop_test");
        let sink = RpcResponseSink {
            inner: tx,
            metrics: Some(metrics.clone()),
        };
        // 5 sends; first 2 buffer, next 3 drop.
        for i in 0..5u8 {
            sink.send(vec![i]);
        }
        assert_eq!(
            metrics
                .streaming_chunks_dropped_total
                .load(Ordering::Relaxed),
            3,
            "expected 3 dropped chunks (capacity=2, sent 5)",
        );
    }

    /// Malformed REQUEST payload on the streaming fold: emits one
    /// terminal `UnknownVersion` frame and continues — same
    /// keep-the-adapter-alive contract as the unary fold.
    #[tokio::test]
    async fn streaming_fold_malformed_payload_emits_unknown_version_terminal() {
        struct NoopHandler;
        #[async_trait::async_trait]
        impl RpcStreamingHandler for NoopHandler {
            async fn call(
                &self,
                _ctx: RpcContext,
                _sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                Ok(())
            }
        }
        let (emit, captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(Arc::new(NoopHandler), emit);
        // Garbage tail: valid meta + 0x00 svc_len → Truncated.
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 1, 1, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.push(0x00);
        let ev = RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        };
        let result = fold.apply(&ev, &mut ());
        assert!(
            result.is_ok(),
            "malformed payload must NOT kill the adapter",
        );
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "synthetic UnknownVersion terminal must arrive",
        );
        let captured = captured.lock();
        assert_eq!(captured[0].2.status, RpcStatus::UnknownVersion);
        let hdr = captured[0]
            .2
            .headers
            .iter()
            .find(|(n, _)| n == HEADER_NRPC_STREAMING);
        assert!(hdr.is_some(), "malformed terminal must include end marker");
    }
}
