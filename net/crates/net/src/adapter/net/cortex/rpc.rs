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

use bytes::{Buf, BufMut, Bytes};
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

/// Caller → server. Continuation chunk of a client-streaming or
/// duplex REQUEST. Carries an [`RpcRequestChunkPayload`] after the
/// `EventMeta` prefix. `EventMeta::seq_or_ts` matches the initial
/// REQUEST's `call_id`. Non-terminal chunks have
/// `flags & FLAG_RPC_REQUEST_END == 0`; the terminal upload chunk
/// sets [`FLAG_RPC_REQUEST_END`].
///
/// Only meaningful for calls whose initial REQUEST set
/// [`FLAG_RPC_CLIENT_STREAMING_REQUEST`]; otherwise the server
/// silently drops the chunk (caller bug; no observable effect).
pub const DISPATCH_RPC_REQUEST_CHUNK: u8 = 0x15;

/// Server → caller. Request-direction stream-credit grant. Mirror
/// of [`DISPATCH_RPC_STREAM_GRANT`] for the upload direction.
/// Carries an [`RpcRequestGrantPayload`] after `EventMeta`: a
/// `call_id` plus a `u32` credit count. `EventMeta::seq_or_ts`
/// matches the call's `call_id` (redundant with the payload, but
/// kept symmetric with the rest of the dispatch family).
///
/// Only meaningful when the caller opted into request-direction
/// flow control via the `nrpc-request-window-initial` header
/// ([`HEADER_NRPC_REQUEST_WINDOW_INITIAL`]). Caller's sink
/// awaits one credit per `REQUEST_CHUNK`; absent header →
/// unbounded credit (sink emits as fast as the publish path can
/// take it).
pub const DISPATCH_RPC_REQUEST_GRANT: u8 = 0x16;

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

// Bit `1 << 3` reserved — symmetric to the reserved bit 0 above,
// kept as breathing room for a future protocol-level flag without
// pushing every existing live bit.

/// Set on the initial REQUEST if the caller will follow up with
/// one or more [`DISPATCH_RPC_REQUEST_CHUNK`] events. Distinguishes
/// client-streaming / duplex calls from unary at the very first
/// frame so the server's fold knows to open a request-side stream
/// instead of treating the REQUEST as complete.
///
/// Combined with [`FLAG_RPC_STREAMING_RESPONSE`] on the same
/// REQUEST: full duplex.
///
/// Bidi streaming plan (Phase A).
pub const FLAG_RPC_CLIENT_STREAMING_REQUEST: u16 = 1 << 4;

/// Set on a [`DISPATCH_RPC_REQUEST_CHUNK`] (or on the initial
/// REQUEST itself) to signal the terminal upload frame for a
/// client-streaming or duplex call. After receiving this, the
/// server's request-side stream yields `None` and the handler
/// proceeds to its terminal response.
///
/// Setting this on the initial REQUEST is the degenerate "client-
/// streaming with exactly one item" path — saves a round-trip
/// for the trivial case.
///
/// Bidi streaming plan (Phase A).
pub const FLAG_RPC_REQUEST_END: u16 = 1 << 5;

// Bits `6..=15` reserved; producers MUST write zero, consumers MUST
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
    /// v0.4 capability-auth: the target's `CapabilityAnnouncement`
    /// either does not list the requested `nrpc:<service>` tag, or
    /// lists it with allow-lists the caller does not match. See
    /// `docs/plans/CAPABILITY_AUTH_PLAN.md` §3. Distinct from
    /// `Unauthorized` (channel-auth / token-scope failures) so
    /// operators can tell the two enforcement surfaces apart in
    /// audit logs.
    /// gRPC equivalent: `PERMISSION_DENIED` (7) — same outward
    /// shape as `Unauthorized` but a separate substrate code.
    CapabilityDenied = 0x0008,
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
            Self::CapabilityDenied => 0x0008,
            Self::Application(v) => v,
        }
    }

    /// Decode from the wire `u16`. Reserved values
    /// (`0x0009..=0x7FFF`) decode as `Application(v)` rather than
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
            0x0008 => Self::CapabilityDenied,
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
    ///
    /// Held as [`Bytes`] so [`Self::decode`] can zero-copy `slice_ref`
    /// the body out of the inbound event's `Bytes` payload — pre-fix
    /// perf #84 in `docs/performance/net-perf-analysis.md` this was
    /// `Vec<u8>` and every decode did a `data[body_start..body_end].to_vec()`
    /// (a memcpy per frame). For high-RPS systems doing 100K+ RPCs/sec
    /// with 1 KB+ bodies that was 100+ MB/sec of pure memcpy.
    pub body: Bytes,
}

/// Continuation chunk for a client-streaming or duplex REQUEST.
/// Lives after the 24-byte `EventMeta` prefix in a
/// [`DISPATCH_RPC_REQUEST_CHUNK`] event.
///
/// Unlike the initial [`RpcRequestPayload`] there is no
/// `service` field (server already routed by service at the
/// initial REQUEST) and no `deadline_ns` (the initial REQUEST's
/// deadline applies to the whole call). The `call_id` field is
/// redundant with `EventMeta::seq_or_ts` but kept on the
/// payload so the codec is self-contained — a reader handed a
/// chunk's bytes without the meta header can still recover its
/// correlation id.
///
/// Bidi streaming plan (Phase A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequestChunkPayload {
    /// Matches `EventMeta::seq_or_ts` and the original REQUEST's
    /// `call_id`. Kept on the payload so the codec round-trips
    /// in isolation.
    pub call_id: u64,
    /// Bitfield of `FLAG_RPC_*` constants. The only flag that
    /// makes sense on a chunk today is [`FLAG_RPC_REQUEST_END`];
    /// other flags MUST be zero on the wire so future protocol
    /// extensions can claim them without colliding with
    /// existing chunks.
    pub flags: u16,
    /// Per-chunk metadata. Typically empty; reserved for
    /// trace-span continuity across long uploads, content-type
    /// changes mid-stream, or other rare per-chunk concerns.
    /// Capped at `MAX_RPC_HEADERS` entries with the same
    /// per-field caps as `RpcRequestPayload::headers`.
    pub headers: Vec<RpcHeader>,
    /// Application-defined chunk body. Cap is `MAX_RPC_BODY_LEN`
    /// (4 MiB), same as the initial REQUEST body — clients that
    /// need >4 MiB total payload chunk their upload across
    /// multiple `REQUEST_CHUNK` events.
    ///
    /// See [`RpcRequestPayload::body`] for the `Bytes`-vs-`Vec<u8>`
    /// rationale.
    pub body: Bytes,
}

/// Request-direction credit grant. Lives after the 24-byte
/// `EventMeta` prefix in a [`DISPATCH_RPC_REQUEST_GRANT`] event.
/// Mirror of the response-direction [`encode_stream_grant`] /
/// [`decode_stream_grant`] pair, but with an explicit `call_id`
/// in the payload (instead of relying solely on
/// `EventMeta::seq_or_ts`) so the codec is self-contained — same
/// rationale as [`RpcRequestChunkPayload::call_id`].
///
/// Bidi streaming plan (Phase A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcRequestGrantPayload {
    /// Matches the call's `call_id`.
    pub call_id: u64,
    /// Additional REQUEST_CHUNK frames the server is willing to
    /// admit beyond the current credit. Server's incoming-credit
    /// counter is capped defensively (see PHASE-B server fold)
    /// so a misbehaving grant can't overflow.
    pub credits: u32,
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
    ///
    /// See [`RpcRequestPayload::body`] for the `Bytes`-vs-`Vec<u8>`
    /// rationale.
    pub body: Bytes,
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
        let mut buf = Vec::with_capacity(self.encoded_len());
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
    ///
    /// Takes [`Bytes`] (not `&[u8]`) so the decoded `body` field
    /// can be a zero-copy `data.slice(..)` instead of an owned
    /// `to_vec` — see perf #84.
    pub fn decode(data: Bytes) -> Result<Self, RpcCodecError> {
        let mut cur = std::io::Cursor::new(data.as_ref());
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
        let headers = decode_headers(&mut cur, &data)?;
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
        // Zero-copy slice over the input — refcount bump only.
        let body = data.slice(body_start..body_end);
        Ok(Self {
            service,
            deadline_ns,
            flags,
            headers,
            body,
        })
    }
}

impl RpcRequestChunkPayload {
    /// Compute the encoded byte length WITHOUT actually encoding.
    /// See [`RpcRequestPayload::encoded_len`] for the rationale.
    pub fn encoded_len(&self) -> usize {
        // call_id: u64
        8
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

    /// Encode to the wire bytes that follow the 24-byte `EventMeta`
    /// prefix in a [`DISPATCH_RPC_REQUEST_CHUNK`] event. Same
    /// encoder-bounds policy as [`RpcRequestPayload::encode`]:
    /// oversize fields panic in debug, the decoder enforces in
    /// release.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.encoded_len());
        // call_id
        buf.put_u64_le(self.call_id);
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
    /// Bounded by the same `MAX_RPC_*` caps as the initial REQUEST.
    /// Takes [`Bytes`] for zero-copy `body` slicing — see perf #84.
    pub fn decode(data: Bytes) -> Result<Self, RpcCodecError> {
        let mut cur = std::io::Cursor::new(data.as_ref());
        // call_id
        if cur.remaining() < 8 {
            return Err(RpcCodecError::Truncated("call_id"));
        }
        let call_id = cur.get_u64_le();
        // flags
        if cur.remaining() < 2 {
            return Err(RpcCodecError::Truncated("flags"));
        }
        let flags = cur.get_u16_le();
        // headers
        let headers = decode_headers(&mut cur, &data)?;
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
        let body = data.slice(body_start..body_end);
        Ok(Self {
            call_id,
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
        let mut buf = Vec::with_capacity(self.encoded_len());
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
    /// Takes [`Bytes`] for zero-copy `body` slicing — see perf #84.
    pub fn decode(data: Bytes) -> Result<Self, RpcCodecError> {
        let mut cur = std::io::Cursor::new(data.as_ref());
        if cur.remaining() < 2 {
            return Err(RpcCodecError::Truncated("status"));
        }
        let status = RpcStatus::from_wire(cur.get_u16_le());
        let headers = decode_headers(&mut cur, &data)?;
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
        let body = data.slice(body_start..body_end);
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
    /// Canonical [`ChannelHash`](crate::adapter::net::channel::ChannelHash)
    /// (u32) of the channel this event arrived on — widened from the
    /// per-packet wire `u16` `NetHeader::channel_hash` via the
    /// registered-dispatcher table at receive time.
    /// Collision-resistant at realistic scale; the wire `u16` may
    /// bucket-collide but the canonical hash uniquely identifies the
    /// registered dispatcher target.
    pub channel_hash: super::super::channel::ChannelHash,
    /// Caller's `origin_hash` from the packet header (32-bit
    /// routing projection of the AEAD-verified peer's full
    /// `EntityKeypair::origin_hash()` — see `OriginStamp` doc).
    /// The dispatcher should treat this as routing metadata, not
    /// identity authentication.
    pub origin_hash: u32,
    /// Wire-session peer's `NodeId` resolved at packet receive
    /// time from the AEAD-verified session_id. Distinct from
    /// `origin_hash`: this is the full 64-bit network identity
    /// of the peer that delivered the packet, not a 32-bit
    /// routing projection. Used by `RpcClientPending::deliver`
    /// to reject spoofed RESPONSE frames whose call_id happens
    /// to match an in-flight request but whose session peer
    /// isn't the recorded target.
    ///
    /// Set to `0` on test / loopback paths that don't have a
    /// session to resolve against; callers that register
    /// pending entries with `target_node = 0` opt out of the
    /// binding gate (and trust the call_id randomness alone).
    ///
    /// **Production wire-path invariant**: real over-the-wire
    /// inbound delivery MUST NOT produce `from_node = 0`. The
    /// dispatcher in `mesh.rs` (`handle_inbound_user_payload`)
    /// drops the event when the wire session has no resolvable
    /// `NodeId`, rather than forwarding under the sentinel — see
    /// the explicit drop + warn at the
    /// `dropping cortex-RPC event: wire session has no resolvable NodeId`
    /// log site. The v0.4 capability-auth callee-side gate in
    /// `MeshNode::serve_rpc`'s bridge relies on this: it skips
    /// permissively when `from_node == 0` (loopback compat), so
    /// a wire-path leak of the sentinel would silently re-open
    /// the gate. If you change the dispatcher to fall back to 0
    /// instead of dropping, you ALSO have to teach the bridge
    /// to deny on the sentinel.
    pub from_node: super::super::behavior::placement::NodeId,
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

/// Header on the initial REQUEST of a client-streaming or duplex
/// call that opts the upload direction into flow control with the
/// given initial credit window. Value is the ASCII decimal
/// representation of a `u32`. When present, the server's
/// streaming-request fold creates a per-call semaphore and the
/// caller's sink awaits one credit per `REQUEST_CHUNK`. The server
/// refills via [`DISPATCH_RPC_REQUEST_GRANT`] events.
///
/// Absent → unbounded credit (caller's sink emits as fast as the
/// publish path can take it). Long client-streaming calls that
/// could outpace a slow handler SHOULD opt into flow control —
/// without it, the server's chunk mpsc grows unbounded under a
/// stalled handler.
///
/// Bidi streaming plan (Phase A).
pub const HEADER_NRPC_REQUEST_WINDOW_INITIAL: &str = "nrpc-request-window-initial";

/// Encode a request-grant payload — `call_id` (u64 little-endian)
/// followed by additional credit (u32 big-endian). Big-endian on
/// the credit field matches [`encode_stream_grant`]; little-endian
/// on `call_id` matches the rest of the RPC codec's u64 fields.
///
/// Pair with [`decode_request_grant`] on the caller side.
pub fn encode_request_grant(call_id: u64, credits: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    buf.put_u64_le(call_id);
    buf.extend_from_slice(&credits.to_be_bytes());
    buf
}

/// Decode a request-grant payload. Returns `None` if the slice is
/// not exactly 12 bytes — defends the caller's fold against
/// malformed grants without killing the cortex adapter.
pub fn decode_request_grant(payload: &[u8]) -> Option<RpcRequestGrantPayload> {
    if payload.len() != 12 {
        return None;
    }
    let mut cid = [0u8; 8];
    cid.copy_from_slice(&payload[..8]);
    let call_id = u64::from_le_bytes(cid);
    let mut credits = [0u8; 4];
    credits.copy_from_slice(&payload[8..]);
    Some(RpcRequestGrantPayload {
        call_id,
        credits: u32::from_be_bytes(credits),
    })
}

/// Parse the `nrpc-request-window-initial` header from a request's
/// header list. Same semantics as [`parse_stream_window_initial`]
/// but for the upload direction.
pub fn parse_request_window_initial(headers: &[RpcHeader]) -> Option<u32> {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(HEADER_NRPC_REQUEST_WINDOW_INITIAL) {
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
                let payload = match RpcRequestPayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
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
                            body: Bytes::from(format!("malformed request: {e}")),
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
                        body: Bytes::from_static(b"deadline already passed when request landed"),
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
                            body: Bytes::from_static(
                                b"duplicate REQUEST for already-in-flight call_id",
                            ),
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
                            body: Bytes::from_static(
                                b"server observed CANCEL during handler execution",
                            ),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(payload)) => payload,
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: Bytes::from(message),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: Bytes::from(message),
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
                                    body: Bytes::from(format!("handler panicked: {panic_msg}")),
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

/// Bounded capacity for the client-streaming server fold's
/// per-call request mpsc. Mirror of [`STREAMING_PUMP_CAPACITY`]
/// for the upload direction. A runaway caller that emits
/// REQUEST_CHUNKs faster than the handler can drain stops
/// queueing past this many chunks — additional chunks are
/// dropped (and counted via `streaming_request_chunks_dropped_total`
/// when metrics are wired). Opt-in flow control via the
/// `nrpc-request-window-initial` header is the right primitive
/// for strict throttling on the upload side.
///
/// Bidi streaming plan (Phase B).
pub const STREAMING_REQUEST_PUMP_CAPACITY: usize = 1024;

// ============================================================================
// Phase B — server-side primitives for client-streaming.
// ============================================================================

/// Context handed to an [`RpcClientStreamingHandler::call`]. Same
/// shape as [`RpcContext`] minus the eager `payload` (the request
/// stream delivers chunk bodies on the fly) and plus the
/// per-call `deadline_ns` (which would otherwise have ridden on
/// the eager payload).
///
/// Bidi streaming plan (Phase B).
pub struct RpcStreamingContext {
    /// AEAD-verified caller `origin_hash`. Same source as
    /// [`RpcContext::caller_origin`].
    pub caller_origin: u64,
    /// Caller-generated correlation id. Matches the initial
    /// REQUEST's `call_id` and every subsequent REQUEST_CHUNK /
    /// CANCEL / REQUEST_GRANT for this call.
    pub call_id: u64,
    /// Absolute deadline (unix nanos) from the initial REQUEST.
    /// `0` means no deadline; the fold does NOT auto-cancel on
    /// deadline (handlers self-supervise via tokio timers, same
    /// contract as the unary fold).
    pub deadline_ns: u64,
    /// Per-chunk metadata headers from the initial REQUEST.
    /// Per-REQUEST_CHUNK headers are NOT surfaced at the substrate
    /// layer — the typed SDK veneer (Phase E) is where header
    /// inspection across chunks lives (if it lands at all; the
    /// plan defers per-chunk-headers as opt-in raw-path access).
    pub headers: Vec<RpcHeader>,
    /// Cancellation signal. Flipped by the fold when a
    /// `DISPATCH_RPC_CANCEL` arrives for this call's `call_id`.
    /// Long-running handlers should `select!` on
    /// `cancellation.cancelled()`; the request stream also
    /// terminates on cancellation, but the token is the
    /// authoritative signal (the stream's terminator is shared
    /// with REQUEST_END).
    pub cancellation: RpcCancellationToken,
    /// W3C Trace Context propagated from the caller's initial
    /// REQUEST. Same semantics as [`RpcContext::trace_context`].
    pub trace_context: Option<TraceContext>,
}

/// Callback the fold invokes to publish a [`DISPATCH_RPC_REQUEST_GRANT`]
/// event back to the caller. Wired up by the `Mesh` glue (Phase C)
/// to publish on the caller's reply channel. Type-erased so the
/// fold doesn't depend on the mesh layer directly.
///
/// Arguments: `(caller_origin, call_id, credits)`. Synchronous —
/// the publish itself is non-blocking (the underlying transport
/// has its own internal queueing); the fold fires-and-forgets
/// every grant, so dropped grants are at worst a latency wobble,
/// not a correctness issue (the caller's send sink will retry
/// when its credit budget refills via the next grant or via the
/// initial window).
///
/// Bidi streaming plan (Phase B).
pub type RpcRequestGrantEmitter = Arc<dyn Fn(u64, u64, u32) + Send + Sync + 'static>;

/// Server-side stream of inbound request chunk bodies for one
/// client-streaming (or duplex) call. Yields one `Bytes` per
/// `DISPATCH_RPC_REQUEST` / `DISPATCH_RPC_REQUEST_CHUNK` frame
/// (including empty bodies — the semantics of "empty bytes" are
/// the application's concern, not the substrate's). Closes on
/// `FLAG_RPC_REQUEST_END` or on CANCEL.
///
/// **Stream item ordering convention**: the first item this
/// stream yields corresponds to the initial REQUEST body; every
/// subsequent item corresponds to a REQUEST_CHUNK body, in the
/// order the chunks were received from the wire. The substrate
/// does not tag items with their frame kind — the SDK veneer
/// (Phase E) is responsible for the Init / Data classification
/// via its `Chunk<T>` enum.
///
/// **Auto-grant behavior**: when the caller opted into
/// request-direction flow control via
/// [`HEADER_NRPC_REQUEST_WINDOW_INITIAL`], every successful
/// `poll_next()` fires one credit back to the caller via the
/// captured `grant_emitter`. This keeps the in-flight window
/// at the caller's initial value as the handler drains the
/// stream. When the caller did NOT opt in (no header), the
/// `grant_emitter` is `None` and the auto-grant path is a no-op
/// (caller is on the unbounded-credit fast path).
///
/// Bidi streaming plan (Phase B).
pub struct RequestStream {
    inner: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    grant_emitter: Option<RpcRequestGrantEmitter>,
    caller_origin: u64,
    call_id: u64,
}

impl RequestStream {
    /// Visible to the fold (and only the fold) for constructing
    /// a stream tied to a specific receiver + caller. The
    /// `grant_emitter` is `None` when the caller didn't opt into
    /// flow control; `Some(...)` when they did.
    pub(crate) fn new(
        inner: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        grant_emitter: Option<RpcRequestGrantEmitter>,
        caller_origin: u64,
        call_id: u64,
    ) -> Self {
        Self {
            inner,
            grant_emitter,
            caller_origin,
            call_id,
        }
    }
}

impl futures::Stream for RequestStream {
    type Item = bytes::Bytes;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.inner.poll_recv(cx) {
            std::task::Poll::Ready(Some(bytes)) => {
                // Auto-grant fires on every successful pull when
                // flow control was opted into. Cheap and
                // fire-and-forget; missed grants are recovered
                // by subsequent pulls.
                if let Some(emit) = self.grant_emitter.as_ref() {
                    emit(self.caller_origin, self.call_id, 1);
                }
                std::task::Poll::Ready(Some(bytes))
            }
            other => other,
        }
    }
}

/// User-supplied handler for a client-streaming RPC. Receives an
/// [`RpcStreamingContext`] (caller identity, deadline, cancellation,
/// trace context, initial REQUEST headers) plus a [`RequestStream`]
/// of chunk bodies. Returns one terminal [`RpcResponsePayload`] —
/// the fold publishes it as the call's single RESPONSE frame.
///
/// **Cancellation contract.** Long-running handlers should
/// `select!` on `ctx.cancellation.cancelled()` so a caller-side
/// drop / deadline correctly stops the handler. The request
/// stream also terminates on cancellation (yields `None`), but
/// the token is the authoritative signal — the stream's `None`
/// is shared with the clean REQUEST_END path, so handlers can't
/// distinguish "caller finished cleanly" from "caller cancelled"
/// without consulting the token.
///
/// **Auto-grant.** When the caller opted into request-direction
/// flow control via [`HEADER_NRPC_REQUEST_WINDOW_INITIAL`], every
/// `stream.next().await` that yields `Some` fires one
/// REQUEST_GRANT back to the caller, maintaining the in-flight
/// window at the caller's initial value. Handlers don't need to
/// think about credit management for the common case.
///
/// Bidi streaming plan (Phase B).
#[async_trait::async_trait]
pub trait RpcClientStreamingHandler: Send + Sync + 'static {
    /// Process a client-streaming call. Drain the request stream,
    /// produce one terminal response payload (or an
    /// [`RpcHandlerError`] for failure mapping).
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError>;
}

/// User-supplied handler for a duplex RPC — many requests in,
/// many responses out, interleaved. Receives an [`RpcStreamingContext`]
/// plus a [`RequestStream`] of chunk bodies plus an
/// [`RpcResponseSink`] for emitting response chunks. The handler's
/// return value is its terminal status, NOT a final payload:
/// `Ok(())` closes the response stream cleanly with a terminal
/// `Ok` frame, `Err(RpcHandlerError)` closes with the matching
/// error status.
///
/// **Composition.** A duplex handler is a hybrid of an
/// [`RpcClientStreamingHandler`] (drains request chunks) and an
/// [`RpcStreamingHandler`] (emits response chunks). The two
/// directions are independent — a handler can finish emitting
/// responses before reading all requests, or vice versa. The
/// server fold serializes RESPONSE chunk publishes per call_id
/// so wire order matches handler order.
///
/// **Cancellation contract.** Identical to
/// [`RpcClientStreamingHandler`]: long-running work should
/// `select!` on `ctx.cancellation.cancelled()`.
///
/// **Auto-grant.** Identical to [`RpcClientStreamingHandler`]:
/// every successful `requests.next().await` emits one
/// REQUEST_GRANT back to the caller (when the caller opted in).
///
/// Bidi streaming plan (Phase D).
#[async_trait::async_trait]
pub trait RpcDuplexHandler: Send + Sync + 'static {
    /// Process one duplex call. Drain inbound chunks via
    /// `requests.next().await`; emit outbound chunks via
    /// `responses.send(...)`. Return `Ok(())` for clean close,
    /// `Err(RpcHandlerError)` for failure mapping.
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError>;
}

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
                let payload = match RpcRequestPayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
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
                            body: Bytes::from(format!("malformed request: {e}")),
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
                            body: Bytes::from_static(
                                b"duplicate REQUEST for already-in-flight call_id",
                            ),
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
                                body: chunk.clone(),
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
                            body: Bytes::from_static(
                                b"server observed CANCEL during streaming handler execution",
                            ),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(())) => RpcResponsePayload {
                                status: RpcStatus::Ok,
                                headers: vec![(
                                    HEADER_NRPC_STREAMING.to_string(),
                                    HEADER_NRPC_STREAMING_END.to_vec(),
                                )],
                                body: Bytes::new(),
                            },
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: Bytes::from(message),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: Bytes::from(message),
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
                                    body: Bytes::from(format!("handler panicked: {panic_msg}")),
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
// Phase B — server-side fold for client-streaming.
//
// `RpcStreamingRequestFold` mirrors `RpcServerStreamingFold` but
// flipped on the data-direction axis: the SERVER consumes a
// stream of REQUEST_CHUNK events and the handler produces ONE
// terminal RESPONSE (vs. the response-side fold where one REQUEST
// drives many RESPONSE chunks).
//
// Wire shape it handles:
//   DISPATCH_RPC_REQUEST       (FLAG_RPC_CLIENT_STREAMING_REQUEST)
//   DISPATCH_RPC_REQUEST_CHUNK (zero or more)
//   DISPATCH_RPC_REQUEST_CHUNK (FLAG_RPC_REQUEST_END)
//   DISPATCH_RPC_CANCEL        (any time; flips token + closes stream)
//
// Wire shape it EMITS (via callbacks):
//   DISPATCH_RPC_RESPONSE        (one terminal frame; via RpcResponseEmitter)
//   DISPATCH_RPC_REQUEST_GRANT   (one per consumed chunk when flow
//                                 control is opted in; via
//                                 RpcRequestGrantEmitter)
//
// Each service binds to exactly one fold shape (unary, server-
// streaming, or client-streaming) at `serve_rpc*` registration.
// A REQUEST without FLAG_RPC_CLIENT_STREAMING_REQUEST that lands
// on the client-streaming fold is a caller bug — the fold emits a
// terminal `Internal` and drops the call.
// ============================================================================

/// Per-call request-direction sender map type. Keyed on
/// `(caller_origin_hash, call_id)`; value is the bounded mpsc
/// sender the fold's `apply()` pushes REQUEST_CHUNK bodies into.
/// The matching receiver lives inside the handler's
/// [`RequestStream`]; dropping the sender (on REQUEST_END or
/// CANCEL) closes the stream.
type RequestChunkSenders = Arc<Mutex<HashMap<(u64, u64), tokio::sync::mpsc::Sender<bytes::Bytes>>>>;

/// Shared REQUEST_CHUNK handling used by both
/// [`RpcStreamingRequestFold`] and [`RpcDuplexFold`]. Decodes the
/// payload, validates the call_id agreement, looks up the per-call
/// sender, pushes the body (skipping the empty-body FLAG_END
/// terminator), and removes the sender on FLAG_END so the
/// handler's stream observes EOF.
///
/// `diag_tag` selects the log prefix ("client-streaming" or
/// "duplex") so the two call sites surface identically-shaped
/// diagnostics with the correct fold name. The behavior is
/// otherwise identical — both folds carry the same wire format
/// and the same per-call mpsc + sender-map contract.
fn apply_request_chunk_to_senders(
    payload_bytes: Bytes,
    meta: &EventMeta,
    senders: &RequestChunkSenders,
    diag_tag: &'static str,
) {
    let payload = match RpcRequestChunkPayload::decode(payload_bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = %e,
                caller_origin = format!("{:#x}", meta.origin_hash),
                call_id = meta.seq_or_ts,
                tag = diag_tag,
                "rpc server fold: malformed REQUEST_CHUNK payload",
            );
            return;
        }
    };
    if payload.call_id != meta.seq_or_ts {
        tracing::warn!(
            caller_origin = format!("{:#x}", meta.origin_hash),
            meta_call_id = meta.seq_or_ts,
            payload_call_id = payload.call_id,
            tag = diag_tag,
            "rpc server fold: REQUEST_CHUNK payload call_id does not match EventMeta",
        );
        return;
    }
    let key = (meta.origin_hash, meta.seq_or_ts);
    let is_end = payload.flags & FLAG_RPC_REQUEST_END != 0;
    let sender = senders.lock().get(&key).cloned();
    let Some(sender) = sender else {
        // Unknown call — either the initial REQUEST hasn't
        // arrived yet (out-of-order delivery is possible on the
        // bus) or the handler already completed and the entry is
        // gone. Drop silently.
        tracing::debug!(
            caller_origin = format!("{:#x}", meta.origin_hash),
            call_id = meta.seq_or_ts,
            tag = diag_tag,
            "rpc server fold: REQUEST_CHUNK for unknown call_id; dropping",
        );
        return;
    };
    let is_pure_terminator = is_end && payload.body.is_empty();
    if !is_pure_terminator && sender.try_send(payload.body).is_err() {
        tracing::debug!(
            caller_origin = format!("{:#x}", meta.origin_hash),
            call_id = meta.seq_or_ts,
            tag = diag_tag,
            "rpc server fold: request-chunk mpsc full or closed; dropping",
        );
    }
    if is_end {
        // Drop the sender from the map → its clone here goes out
        // of scope at end of function → the receiver in the
        // handler's RequestStream sees EOF on the next poll.
        senders.lock().remove(&key);
    }
}

/// Server-side fold for client-streaming RPC. Parallel to
/// [`RpcServerStreamingFold`] but consumes REQUEST_CHUNK on the
/// input side and produces one terminal RESPONSE on the output
/// side (vs. one REQUEST in / many RESPONSE chunks out).
///
/// State `()` — like the other folds, application state lives in
/// the handler's captured `Arc<Mutex<S>>`. The fold's own state
/// (in-flight cancellation tokens + per-call request-chunk
/// senders) lives on `&mut self` via `Arc<Mutex<...>>` so spawned
/// handler tasks can self-clean on completion.
///
/// Bidi streaming plan (Phase B).
pub struct RpcStreamingRequestFold {
    handler: Arc<dyn RpcClientStreamingHandler>,
    emit: RpcResponseEmitter,
    /// Optional request-direction grant emitter. `Some(...)`
    /// when the surrounding mesh glue is wired to publish
    /// REQUEST_GRANT events; `None` in unit tests / contexts
    /// without a real publish path. When `None`, the auto-grant
    /// path on every `RequestStream::poll_next` becomes a no-op
    /// (callers that opted into flow control will see no
    /// refill and stall once their initial window is exhausted —
    /// honest behavior for a fold not wired up for grants).
    grant_emit: Option<RpcRequestGrantEmitter>,
    in_flight: Arc<Mutex<HashMap<(u64, u64), RpcCancellationToken>>>,
    senders: RequestChunkSenders,
    /// Optional per-service metrics handle. Same shape as the
    /// other folds. Reuses the response-side counters where they
    /// apply (handler_invocations / handler_panics / etc.) and
    /// would gain request-side counters (e.g.
    /// `streaming_request_chunks_dropped_total`) in a follow-up.
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
}

impl RpcStreamingRequestFold {
    /// Construct a client-streaming server fold. `emit` publishes
    /// the terminal RESPONSE on the caller's reply channel.
    ///
    /// Use the sync [`RpcResponseEmitter`] here — there's only
    /// one RESPONSE per call (the terminal frame), so the
    /// per-call serialization the async emitter buys for the
    /// response-side fold is not needed here.
    pub fn new(handler: Arc<dyn RpcClientStreamingHandler>, emit: RpcResponseEmitter) -> Self {
        Self {
            handler,
            emit,
            grant_emit: None,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            metrics: None,
        }
    }

    /// Attach the request-direction grant emitter. Hands every
    /// `RequestStream::poll_next` a hook to fire one REQUEST_GRANT
    /// back to the caller after a chunk is consumed. Optional —
    /// folds constructed without it still work, callers that
    /// opted into flow control just won't be refilled.
    pub fn with_grant_emitter(mut self, grant_emit: RpcRequestGrantEmitter) -> Self {
        self.grant_emit = Some(grant_emit);
        self
    }

    /// Attach a per-service metrics handle. Hooks the spawned
    /// handler task to bump `handler_invocations_total` /
    /// `handler_in_flight` / `handler_panics_total` /
    /// `handler_duration_*`. Symmetric with `RpcServerFold` and
    /// `RpcServerStreamingFold`.
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

    /// Test-only: snapshot of the in-flight per-call senders.
    /// Useful for tests that need to assert a call's sender has
    /// been dropped after REQUEST_END / CANCEL.
    #[cfg(test)]
    pub fn sender_keys(&self) -> Vec<(u64, u64)> {
        self.senders.lock().keys().copied().collect()
    }
}

impl RedexFold<()> for RpcStreamingRequestFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc client-streaming server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc client-streaming server fold: malformed request payload",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::UnknownVersion,
                            headers: vec![],
                            body: Bytes::from(format!("malformed request: {e}")),
                        };
                        (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                        return Ok(());
                    }
                };
                // A REQUEST without the client-streaming flag on
                // this fold is a caller bug — the service was
                // registered as client-streaming. Refuse cleanly.
                if payload.flags & FLAG_RPC_CLIENT_STREAMING_REQUEST == 0 {
                    tracing::warn!(
                        caller_origin = format!("{:#x}", meta.origin_hash),
                        call_id = meta.seq_or_ts,
                        flags = format!("{:#06x}", payload.flags),
                        "rpc client-streaming server fold: REQUEST missing FLAG_RPC_CLIENT_STREAMING_REQUEST",
                    );
                    let resp = RpcResponsePayload {
                        status: RpcStatus::Internal,
                        headers: vec![],
                        body: Bytes::from_static(
                            b"REQUEST on a client-streaming service must set FLAG_RPC_CLIENT_STREAMING_REQUEST",
                        ),
                    };
                    (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                    return Ok(());
                }
                // Refuse a duplicate REQUEST with the same
                // `(origin_hash, call_id)` — same rationale as
                // the response-side fold: a retry that arrives
                // while the first attempt is still in-flight
                // would overwrite the prior sender and orphan the
                // existing handler.
                {
                    let in_flight = self.in_flight.lock();
                    if in_flight.contains_key(&key) {
                        drop(in_flight);
                        tracing::warn!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc client-streaming server fold: duplicate REQUEST for in-flight call_id; refusing",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::Internal,
                            headers: vec![],
                            body: Bytes::from_static(
                                b"duplicate REQUEST for already-in-flight call_id",
                            ),
                        };
                        (self.emit)(meta.origin_hash, meta.seq_or_ts, resp);
                        return Ok(());
                    }
                }
                let cancellation = RpcCancellationToken::new();
                self.in_flight.lock().insert(key, cancellation.clone());
                // Build the per-call request-chunk mpsc. Bounded
                // capacity — overflow on the sender side drops the
                // chunk (caller can re-send or, if flow-control is
                // wired, will naturally not push past the credit
                // window).
                let (tx, rx) =
                    tokio::sync::mpsc::channel::<bytes::Bytes>(STREAMING_REQUEST_PUMP_CAPACITY);
                // Terminator-semantics rule: an empty body
                // combined with FLAG_REQUEST_END is a pure
                // terminator — the caller's `finish()` emits it
                // to close the stream without yielding a phantom
                // empty item to the handler. A non-empty body on
                // a FLAG_END frame IS a final item (used by the
                // "single-item degenerate path": initial REQUEST
                // with FLAG_END + a real body sends one item +
                // closes in a single frame).
                let end_on_initial = payload.flags & FLAG_RPC_REQUEST_END != 0;
                let is_pure_terminator = end_on_initial && payload.body.is_empty();
                if !is_pure_terminator {
                    // Fresh `mpsc::channel(STREAMING_REQUEST_PUMP_CAPACITY)`
                    // with a live receiver — try_send cannot fail.
                    // debug_assert surfaces the invariant break in
                    // tests; release logs at error level rather than
                    // silently swallowing the first request body.
                    if tx.try_send(payload.body).is_err() {
                        debug_assert!(
                            false,
                            "fresh client-streaming request mpsc rejected initial body"
                        );
                        tracing::error!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc client-streaming server fold: fresh mpsc rejected initial REQUEST body (invariant break)",
                        );
                    }
                }
                // If the initial REQUEST also set FLAG_REQUEST_END,
                // close the stream immediately — degenerate case of
                // "one-item upload" where the caller didn't bother
                // with a trailing REQUEST_CHUNK. Don't even insert
                // the sender into the map; just drop it here.
                if !end_on_initial {
                    self.senders.lock().insert(key, tx);
                }
                // Build the handler's context + stream. Auto-grant
                // is opted into when the caller set the request
                // window header AND the fold was wired with a
                // grant emitter; both must be present for grants
                // to actually fly.
                let grant_emitter = if parse_request_window_initial(&payload.headers).is_some() {
                    self.grant_emit.clone()
                } else {
                    None
                };
                let request_stream =
                    RequestStream::new(rx, grant_emitter, meta.origin_hash, meta.seq_or_ts);
                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let deadline_ns = payload.deadline_ns;
                let ctx = RpcStreamingContext {
                    caller_origin: meta.origin_hash,
                    call_id: meta.seq_or_ts,
                    deadline_ns,
                    headers: payload.headers,
                    cancellation: cancellation.clone(),
                    trace_context,
                };
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                let senders = self.senders.clone();
                let caller_origin = meta.origin_hash;
                let call_id = meta.seq_or_ts;
                let cancel_probe = cancellation.clone();
                let cancel_for_deadline = cancellation.clone();
                let metrics = self.metrics.clone();
                tokio::spawn(async move {
                    if let Some(m) = metrics.as_ref() {
                        m.handler_invocations_total
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.handler_in_flight
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let handler_started = std::time::Instant::now();
                    // Deadline guard: if the caller declared
                    // `deadline_ns`, force-drop the handler future
                    // after it elapses so an orphaned request stream
                    // (caller-side network partition before
                    // REQUEST_END arrives) can never hang the call
                    // indefinitely. `deadline_ns = 0` means "no
                    // deadline" — caller's responsibility.
                    let call_fut = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                        handler.call(ctx, request_stream),
                    ));
                    let outcome = if deadline_ns > 0 {
                        let now_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);
                        let remaining = deadline_ns.saturating_sub(now_ns);
                        if remaining == 0 {
                            cancel_for_deadline.cancel();
                            Ok(Err(RpcHandlerError::Internal(
                                "handler deadline_ns already expired at spawn".to_string(),
                            )))
                        } else {
                            match tokio::time::timeout(
                                std::time::Duration::from_nanos(remaining),
                                call_fut,
                            )
                            .await
                            {
                                Ok(o) => o,
                                Err(_) => {
                                    cancel_for_deadline.cancel();
                                    Ok(Err(RpcHandlerError::Internal(
                                        "handler deadline_ns exceeded".to_string(),
                                    )))
                                }
                            }
                        }
                    } else {
                        call_fut.await
                    };
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
                    // token fired during execution, override the
                    // handler's terminal with Cancelled.
                    let terminal = if cancel_probe.is_cancelled() {
                        RpcResponsePayload {
                            status: RpcStatus::Cancelled,
                            headers: vec![],
                            body: Bytes::from_static(
                                b"server observed CANCEL during client-streaming handler execution",
                            ),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(resp)) => resp,
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: Bytes::from(message),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: Bytes::from(message),
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
                                    "rpc client-streaming server handler panicked",
                                );
                                RpcResponsePayload {
                                    status: RpcStatus::Internal,
                                    headers: vec![],
                                    body: Bytes::from(format!("handler panicked: {panic_msg}")),
                                }
                            }
                        }
                    };
                    in_flight.lock().remove(&key);
                    // Drop the per-call request-chunk sender too
                    // (idempotent — already gone if REQUEST_END
                    // arrived; defensive otherwise so a handler
                    // that returned without consuming all chunks
                    // doesn't leak the entry).
                    senders.lock().remove(&key);
                    (emit)(caller_origin, call_id, terminal);
                });
            }
            DISPATCH_RPC_REQUEST_CHUNK => {
                apply_request_chunk_to_senders(
                    ev.payload.slice(EVENT_META_SIZE..),
                    &meta,
                    &self.senders,
                    "client-streaming",
                );
            }
            DISPATCH_RPC_CANCEL => {
                if let Some(token) = self.in_flight.lock().remove(&key) {
                    token.cancel();
                }
                // Drop the per-call sender so the handler's
                // RequestStream yields None on the next poll
                // (handler observes cancel via the token OR via
                // the stream's EOF; the cancel_probe in the
                // spawned task ensures the terminal RESPONSE is
                // Cancelled regardless of which the handler
                // checks first).
                self.senders.lock().remove(&key);
            }
            _ => {}
        }
        Ok(())
    }
}

// ============================================================================
// Phase D — server-side fold for full duplex.
//
// `RpcDuplexFold` is the hybrid of `RpcStreamingRequestFold`
// (Phase B — request side) and `RpcServerStreamingFold` (existing
// — response side). The handler trait takes BOTH a `RequestStream`
// AND an `RpcResponseSink`; the fold spawns one handler task per
// REQUEST and one pump task per call_id, then emits a terminal
// RESPONSE on handler return.
//
// Wire shape it consumes:
//   DISPATCH_RPC_REQUEST       (FLAG_CLIENT_STREAMING_REQUEST + FLAG_STREAMING_RESPONSE)
//   DISPATCH_RPC_REQUEST_CHUNK (zero or more, with FLAG_REQUEST_END on the last)
//   DISPATCH_RPC_CANCEL        (flips token + closes both directions)
//
// Wire shape it produces:
//   DISPATCH_RPC_RESPONSE        (multi-fire; nrpc-streaming: continue / end)
//   DISPATCH_RPC_REQUEST_GRANT   (one per consumed request-chunk when flow
//                                 control is opted in)
//
// Bidi streaming plan (Phase D).
// ============================================================================

/// Server-side fold for duplex RPC. Composes Phase B's request
/// stream + per-call request-chunk senders with the existing
/// response-side pump + multi-fire RESPONSE emit.
///
/// State `()` — same as the sibling folds.
///
/// Bidi streaming plan (Phase D).
pub struct RpcDuplexFold {
    handler: Arc<dyn RpcDuplexHandler>,
    /// Async emitter for response chunks (per-call ordering via
    /// awaited emits — same rationale as `RpcServerStreamingFold`).
    emit: RpcAsyncResponseEmitter,
    /// Optional request-direction grant emitter.
    grant_emit: Option<RpcRequestGrantEmitter>,
    in_flight: Arc<Mutex<HashMap<(u64, u64), RpcCancellationToken>>>,
    senders: RequestChunkSenders,
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
}

impl RpcDuplexFold {
    /// Construct a duplex server fold. `emit` publishes individual
    /// response chunks AND the terminal frame on the caller's
    /// reply channel (uses the async emitter for per-call
    /// ordering, same as `RpcServerStreamingFold`).
    pub fn new(handler: Arc<dyn RpcDuplexHandler>, emit: RpcAsyncResponseEmitter) -> Self {
        Self {
            handler,
            emit,
            grant_emit: None,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            metrics: None,
        }
    }

    /// Attach the request-direction grant emitter. See
    /// [`RpcStreamingRequestFold::with_grant_emitter`] for the
    /// auto-grant behavior. When unset, callers that opted into
    /// flow control simply won't get refilled.
    pub fn with_grant_emitter(mut self, grant_emit: RpcRequestGrantEmitter) -> Self {
        self.grant_emit = Some(grant_emit);
        self
    }

    /// Attach a per-service metrics handle. Bumps
    /// handler_invocations / handler_in_flight / handler_panics /
    /// handler_duration_* + the response pump's
    /// streaming_chunks_emitted_total per emitted chunk.
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

    /// Test-only: snapshot of the in-flight per-call senders.
    #[cfg(test)]
    pub fn sender_keys(&self) -> Vec<(u64, u64)> {
        self.senders.lock().keys().copied().collect()
    }
}

impl RedexFold<()> for RpcDuplexFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc duplex server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc duplex server fold: malformed request payload",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::UnknownVersion,
                            headers: vec![(
                                HEADER_NRPC_STREAMING.to_string(),
                                HEADER_NRPC_STREAMING_END.to_vec(),
                            )],
                            body: Bytes::from(format!("malformed request: {e}")),
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
                // Caller-bug guard: a duplex REQUEST must set
                // BOTH the client-streaming flag (we'll receive
                // request chunks) AND the streaming-response flag
                // (we'll emit response chunks). Missing flags →
                // refuse cleanly.
                let required = FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE;
                if payload.flags & required != required {
                    tracing::warn!(
                        caller_origin = format!("{:#x}", meta.origin_hash),
                        call_id = meta.seq_or_ts,
                        flags = format!("{:#06x}", payload.flags),
                        "rpc duplex server fold: REQUEST missing required flags",
                    );
                    let resp = RpcResponsePayload {
                        status: RpcStatus::Internal,
                        headers: vec![(
                            HEADER_NRPC_STREAMING.to_string(),
                            HEADER_NRPC_STREAMING_END.to_vec(),
                        )],
                        body: Bytes::from_static(
                            b"REQUEST on a duplex service must set FLAG_RPC_CLIENT_STREAMING_REQUEST and FLAG_RPC_STREAMING_RESPONSE",
                        ),
                    };
                    let emit = self.emit.clone();
                    let caller_origin = meta.origin_hash;
                    let call_id = meta.seq_or_ts;
                    tokio::spawn(async move {
                        emit(caller_origin, call_id, resp).await;
                    });
                    return Ok(());
                }
                // Duplicate-REQUEST refusal.
                {
                    let in_flight = self.in_flight.lock();
                    if in_flight.contains_key(&key) {
                        drop(in_flight);
                        tracing::warn!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc duplex server fold: duplicate REQUEST for in-flight call_id; refusing",
                        );
                        let resp = RpcResponsePayload {
                            status: RpcStatus::Internal,
                            headers: vec![(
                                HEADER_NRPC_STREAMING.to_string(),
                                HEADER_NRPC_STREAMING_END.to_vec(),
                            )],
                            body: Bytes::from_static(
                                b"duplicate REQUEST for already-in-flight call_id",
                            ),
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
                let cancellation = RpcCancellationToken::new();
                self.in_flight.lock().insert(key, cancellation.clone());

                // Build per-call request-side mpsc (Phase B
                // pattern).
                let (req_tx, req_rx) =
                    tokio::sync::mpsc::channel::<bytes::Bytes>(STREAMING_REQUEST_PUMP_CAPACITY);
                let end_on_initial = payload.flags & FLAG_RPC_REQUEST_END != 0;
                let is_pure_terminator = end_on_initial && payload.body.is_empty();
                if !is_pure_terminator {
                    // Same invariant as the client-streaming fold:
                    // fresh bounded mpsc with a live receiver cannot
                    // reject the first send.
                    if req_tx.try_send(payload.body).is_err() {
                        debug_assert!(false, "fresh duplex request mpsc rejected initial body");
                        tracing::error!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc duplex server fold: fresh mpsc rejected initial REQUEST body (invariant break)",
                        );
                    }
                }
                if !end_on_initial {
                    self.senders.lock().insert(key, req_tx);
                }
                // Hand the handler an auto-granting RequestStream
                // when the caller opted into request-direction
                // flow control AND the fold was wired with a
                // grant emitter.
                let grant_emitter = if parse_request_window_initial(&payload.headers).is_some() {
                    self.grant_emit.clone()
                } else {
                    None
                };
                let request_stream =
                    RequestStream::new(req_rx, grant_emitter, meta.origin_hash, meta.seq_or_ts);

                // Build the per-call response-side mpsc (existing
                // server-streaming-response pattern). The handler
                // writes chunks to the sink; the pump task drains
                // the receiver and publishes RESPONSE events.
                let (resp_tx, mut resp_rx) =
                    tokio::sync::mpsc::channel::<bytes::Bytes>(STREAMING_PUMP_CAPACITY);
                let response_sink = RpcResponseSink {
                    inner: resp_tx,
                    metrics: self.metrics.clone(),
                };

                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let deadline_ns = payload.deadline_ns;
                let ctx = RpcStreamingContext {
                    caller_origin: meta.origin_hash,
                    call_id: meta.seq_or_ts,
                    deadline_ns,
                    headers: payload.headers,
                    cancellation: cancellation.clone(),
                    trace_context,
                };
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                let senders = self.senders.clone();
                let caller_origin = meta.origin_hash;
                let call_id = meta.seq_or_ts;
                let cancel_probe = cancellation.clone();
                let cancel_for_deadline = cancellation.clone();
                let metrics = self.metrics.clone();

                // Pump: drains resp_rx, emits per-chunk RESPONSE
                // events with `nrpc-streaming: continue`.
                let pump_emit = emit.clone();
                let pump_metrics = metrics.clone();
                let pump = tokio::spawn(async move {
                    while let Some(chunk) = resp_rx.recv().await {
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
                            body: chunk.clone(),
                        };
                        pump_emit(caller_origin, call_id, resp).await;
                    }
                });

                tokio::spawn(async move {
                    if let Some(m) = metrics.as_ref() {
                        m.handler_invocations_total
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        m.handler_in_flight
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    let handler_started = std::time::Instant::now();
                    // Same deadline guard as the client-streaming
                    // fold: force-drop the handler future at
                    // deadline_ns so an orphaned request stream
                    // can't hang the call. `0` means no deadline.
                    let call_fut = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                        handler.call(ctx, request_stream, response_sink),
                    ));
                    let outcome = if deadline_ns > 0 {
                        let now_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);
                        let remaining = deadline_ns.saturating_sub(now_ns);
                        if remaining == 0 {
                            cancel_for_deadline.cancel();
                            Ok(Err(RpcHandlerError::Internal(
                                "duplex handler deadline_ns already expired at spawn".to_string(),
                            )))
                        } else {
                            match tokio::time::timeout(
                                std::time::Duration::from_nanos(remaining),
                                call_fut,
                            )
                            .await
                            {
                                Ok(o) => o,
                                Err(_) => {
                                    cancel_for_deadline.cancel();
                                    Ok(Err(RpcHandlerError::Internal(
                                        "duplex handler deadline_ns exceeded".to_string(),
                                    )))
                                }
                            }
                        }
                    } else {
                        call_fut.await
                    };
                    // Handler dropped the sink — let the pump
                    // drain any final in-flight chunks before we
                    // emit the terminal frame.
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
                    let terminal = if cancel_probe.is_cancelled() {
                        RpcResponsePayload {
                            status: RpcStatus::Cancelled,
                            headers: vec![],
                            body: Bytes::from_static(
                                b"server observed CANCEL during duplex handler execution",
                            ),
                        }
                    } else {
                        match outcome {
                            Ok(Ok(())) => RpcResponsePayload {
                                status: RpcStatus::Ok,
                                headers: vec![(
                                    HEADER_NRPC_STREAMING.to_string(),
                                    HEADER_NRPC_STREAMING_END.to_vec(),
                                )],
                                body: Bytes::new(),
                            },
                            Ok(Err(RpcHandlerError::Application { code, message })) => {
                                RpcResponsePayload {
                                    status: RpcStatus::Application(code),
                                    headers: vec![],
                                    body: Bytes::from(message),
                                }
                            }
                            Ok(Err(RpcHandlerError::Internal(message))) => RpcResponsePayload {
                                status: RpcStatus::Internal,
                                headers: vec![],
                                body: Bytes::from(message),
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
                                    "rpc duplex server handler panicked",
                                );
                                RpcResponsePayload {
                                    status: RpcStatus::Internal,
                                    headers: vec![],
                                    body: Bytes::from(format!("handler panicked: {panic_msg}")),
                                }
                            }
                        }
                    };
                    in_flight.lock().remove(&key);
                    senders.lock().remove(&key);
                    emit(caller_origin, call_id, terminal).await;
                });
            }
            DISPATCH_RPC_REQUEST_CHUNK => {
                apply_request_chunk_to_senders(
                    ev.payload.slice(EVENT_META_SIZE..),
                    &meta,
                    &self.senders,
                    "duplex",
                );
            }
            DISPATCH_RPC_CANCEL => {
                if let Some(token) = self.in_flight.lock().remove(&key) {
                    token.cancel();
                }
                self.senders.lock().remove(&key);
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

/// One pending entry — unary oneshot, server-streaming mpsc, or
/// client-streaming (one terminal oneshot + a separate grant
/// mpsc). The fold dispatches to the right variant based on
/// what's registered for the `call_id`.
enum PendingEntry {
    /// Unary call — exactly one RESPONSE expected. Completes the
    /// oneshot with the decoded payload.
    Unary(tokio::sync::oneshot::Sender<RpcResponsePayload>),
    /// Server-streaming call — multiple non-terminal `Continue`
    /// chunks followed by one terminal frame. Each non-terminal
    /// chunk pushes a `StreamItem::Chunk(body)` onto the mpsc;
    /// the terminal frame pushes `StreamItem::End` (Ok) or
    /// `StreamItem::Error(payload)` (non-Ok status) and the
    /// pending entry is removed.
    Streaming(tokio::sync::mpsc::UnboundedSender<StreamItem>),
    /// Client-streaming or duplex call. Two sender halves:
    ///
    /// - `terminal_tx`: oneshot that completes when the server's
    ///   single terminal RESPONSE arrives. Response shape and
    ///   delivery semantics are identical to the unary variant —
    ///   the caller awaits one payload, success or failure status.
    /// - `grant_tx`: mpsc that ferries REQUEST_GRANT credit values
    ///   from the client fold to the caller's send sink. Each
    ///   `DISPATCH_RPC_REQUEST_GRANT` event for this call_id
    ///   pushes one `u32` credit onto the mpsc; the caller's send
    ///   sink consumes credits to gate `send(...).await`.
    ///
    /// Bidi streaming plan (Phase C). Used for pure client-
    /// streaming (one terminal RESPONSE closes the call). Duplex
    /// calls use the [`PendingEntry::Duplex`] variant instead,
    /// since they receive many response chunks rather than one
    /// terminal payload.
    ClientStreaming {
        terminal_tx: tokio::sync::oneshot::Sender<RpcResponsePayload>,
        grant_tx: tokio::sync::mpsc::UnboundedSender<u32>,
    },
    /// Duplex call — many request chunks out, many response
    /// chunks in. Two senders, same shape as `ClientStreaming`
    /// except the terminal slot is an mpsc instead of a oneshot
    /// because the response side is multi-chunk (terminator is
    /// implicit in `StreamItem::End` / `StreamItem::Error` on
    /// the chunks_tx mpsc, same as `PendingEntry::Streaming`).
    ///
    /// - `chunks_tx`: response-chunk mpsc — fed by `deliver`
    ///   when RESPONSE events arrive on the reply channel.
    ///   `StreamItem::Chunk` for non-terminal, `StreamItem::End`
    ///   / `StreamItem::Error` terminates and removes the entry.
    /// - `grant_tx`: request-direction credit mpsc — fed by
    ///   `deliver_grant` when REQUEST_GRANT events arrive.
    ///
    /// Bidi streaming plan (Phase D).
    Duplex {
        chunks_tx: tokio::sync::mpsc::UnboundedSender<StreamItem>,
        grant_tx: tokio::sync::mpsc::UnboundedSender<u32>,
    },
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
    /// Map keyed on `call_id`, value carries `(expected_target,
    /// PendingEntry)`. `expected_target` is the `NodeId` of the
    /// peer the request was dispatched to; `deliver` rejects
    /// frames whose wire `from_node` doesn't match. A
    /// `expected_target == 0` entry opts out of the binding
    /// (loopback tests + paths with no session).
    senders: dashmap::DashMap<u64, (super::super::behavior::placement::NodeId, PendingEntry)>,
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
    /// `target_node` is the wire-session peer the request will
    /// be sent to; `deliver` rejects RESPONSE frames whose
    /// `from_node` doesn't match. Pass `0` for loopback / no-
    /// session test paths to opt out of the binding gate.
    ///
    /// If a sender already exists for `call_id` (improperly reused
    /// id), it is replaced and the old receiver gets a
    /// `RecvError::Closed` — surfacing the misuse as a hard error
    /// at the caller rather than silently delivering the response
    /// to the wrong waiter.
    pub fn register(
        &self,
        call_id: u64,
        target_node: super::super::behavior::placement::NodeId,
    ) -> tokio::sync::oneshot::Receiver<RpcResponsePayload> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.senders
            .insert(call_id, (target_node, PendingEntry::Unary(tx)));
        rx
    }

    /// Register a streaming entry for `call_id`. Returns the
    /// receive end of an mpsc the fold will push chunks onto.
    /// Same registration ordering rules as `register` —
    /// publisher must call this BEFORE publishing the REQUEST.
    pub fn register_streaming(
        &self,
        call_id: u64,
        target_node: super::super::behavior::placement::NodeId,
    ) -> tokio::sync::mpsc::UnboundedReceiver<StreamItem> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.senders
            .insert(call_id, (target_node, PendingEntry::Streaming(tx)));
        rx
    }

    /// Register a client-streaming (or duplex) entry for
    /// `call_id`. Returns BOTH the terminal-response receiver
    /// (the caller awaits on this for the single terminal
    /// RESPONSE that ends the call) AND a grant receiver (the
    /// caller's send sink consumes this to gate `send().await`
    /// when the caller opted into request-direction flow
    /// control).
    ///
    /// Same registration ordering rules as `register` /
    /// `register_streaming` — publisher must call this BEFORE
    /// publishing the REQUEST so a fast server's RESPONSE /
    /// REQUEST_GRANT can't arrive while no pending entry exists.
    ///
    /// Bidi streaming plan (Phase C).
    pub fn register_client_streaming(
        &self,
        call_id: u64,
        target_node: super::super::behavior::placement::NodeId,
    ) -> (
        tokio::sync::oneshot::Receiver<RpcResponsePayload>,
        tokio::sync::mpsc::UnboundedReceiver<u32>,
    ) {
        let (terminal_tx, terminal_rx) = tokio::sync::oneshot::channel();
        let (grant_tx, grant_rx) = tokio::sync::mpsc::unbounded_channel();
        self.senders.insert(
            call_id,
            (
                target_node,
                PendingEntry::ClientStreaming {
                    terminal_tx,
                    grant_tx,
                },
            ),
        );
        (terminal_rx, grant_rx)
    }

    /// Register a duplex entry for `call_id`. Returns BOTH a
    /// response-chunk receiver (yields `StreamItem` per inbound
    /// RESPONSE chunk; terminator is `End` / `Error`) AND a
    /// grant receiver (yields `u32` credits per inbound
    /// REQUEST_GRANT).
    ///
    /// Same registration ordering rules as the other `register_*`
    /// methods: publisher must call this BEFORE publishing the
    /// REQUEST so the server's response chunks / grants can't
    /// arrive while no pending entry exists.
    ///
    /// Bidi streaming plan (Phase D).
    pub fn register_duplex(
        &self,
        call_id: u64,
        target_node: super::super::behavior::placement::NodeId,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<StreamItem>,
        tokio::sync::mpsc::UnboundedReceiver<u32>,
    ) {
        let (chunks_tx, chunks_rx) = tokio::sync::mpsc::unbounded_channel();
        let (grant_tx, grant_rx) = tokio::sync::mpsc::unbounded_channel();
        self.senders.insert(
            call_id,
            (
                target_node,
                PendingEntry::Duplex {
                    chunks_tx,
                    grant_tx,
                },
            ),
        );
        (chunks_rx, grant_rx)
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
    /// `from_node` is the wire-session peer of the inbound
    /// RESPONSE. If the pending entry's recorded `target_node`
    /// is non-zero and does not match `from_node`, the frame is
    /// dropped with a trace log and the pending entry stays
    /// intact — a forged response on a shared reply channel
    /// can't resolve a victim's call. A recorded `target_node
    /// == 0` opts the call out of the binding (loopback paths).
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
    fn deliver(
        &self,
        call_id: u64,
        from_node: super::super::behavior::placement::NodeId,
        resp: RpcResponsePayload,
    ) {
        // Look up the entry — but DON'T remove it yet, because for
        // streaming we may want to keep it for non-terminal chunks.
        // The remove decision is per-variant.
        let entry = self.senders.get(&call_id);
        let Some(entry) = entry else { return };
        // S-4 part 2 gate. The pending registry binds each call
        // to the AEAD-verified `target_node` the request was
        // dispatched to; any other session peer publishing on the
        // shared reply channel with a guessed call_id is dropped
        // here without touching the waiter. `0` opts out — used
        // by loopback paths that have no session peer.
        let (target_node, _entry_value) = entry.value();
        if *target_node != 0 && *target_node != from_node {
            tracing::trace!(
                call_id,
                from_node,
                expected = *target_node,
                "rpc client: dropping RESPONSE from non-target session peer"
            );
            return;
        }
        match entry.value() {
            (_, PendingEntry::Unary(_)) => {
                drop(entry);
                if let Some((_, (_, PendingEntry::Unary(tx)))) = self.senders.remove(&call_id) {
                    let _ = tx.send(resp);
                }
            }
            (_, PendingEntry::ClientStreaming { .. }) => {
                // Terminal RESPONSE for a client-streaming /
                // duplex call. Same delivery shape as Unary —
                // complete the oneshot, remove the entry. The
                // grant_tx half drops with the entry, which is
                // fine (no more grants will arrive after the
                // terminal frame).
                drop(entry);
                if let Some((
                    _,
                    (
                        _,
                        PendingEntry::ClientStreaming {
                            terminal_tx,
                            grant_tx: _,
                        },
                    ),
                )) = self.senders.remove(&call_id)
                {
                    let _ = terminal_tx.send(resp);
                }
            }
            (_, PendingEntry::Streaming(tx)) => {
                let tx = tx.clone();
                drop(entry);
                self.dispatch_streaming_chunk(&tx, resp, call_id);
            }
            (_, PendingEntry::Duplex { chunks_tx, .. }) => {
                // Same dispatch logic as Streaming — duplex
                // response side IS a multi-chunk stream.
                let tx = chunks_tx.clone();
                drop(entry);
                self.dispatch_streaming_chunk(&tx, resp, call_id);
            }
        }
    }

    /// Shared response-chunk dispatch used by both
    /// `PendingEntry::Streaming` and `PendingEntry::Duplex`. The
    /// caller has already verified the target-binding gate and
    /// dropped its `entry` ref; this helper does the classify-
    /// and-push and removes the entry from the senders map on
    /// terminal frames.
    fn dispatch_streaming_chunk(
        &self,
        tx: &tokio::sync::mpsc::UnboundedSender<StreamItem>,
        resp: RpcResponsePayload,
        call_id: u64,
    ) {
        let kind = classify_streaming_chunk(&resp);
        match kind {
            StreamingChunkKind::Continue => {
                let _ = tx.send(StreamItem::Chunk(resp.body));
            }
            StreamingChunkKind::Terminal => {
                let item = if resp.status.is_ok() {
                    if !resp.body.is_empty() {
                        let _ = tx.send(StreamItem::Chunk(resp.body));
                    }
                    StreamItem::End
                } else {
                    StreamItem::Error(resp)
                };
                let _ = tx.send(item);
                self.senders.remove(&call_id);
            }
            StreamingChunkKind::Unary => {
                tracing::warn!(
                    call_id,
                    body_len = resp.body.len(),
                    "rpc client: streaming / duplex consumer received unary-shaped \
                     response (no nrpc-streaming header); server may have bridged a \
                     unary path. Bridging to single-chunk + EOF.",
                );
                if !resp.body.is_empty() {
                    let _ = tx.send(StreamItem::Chunk(resp.body));
                }
                let _ = tx.send(StreamItem::End);
                self.senders.remove(&call_id);
            }
        }
    }

    /// Deliver a request-direction grant credit to the waiter
    /// for `call_id`, if it's a client-streaming / duplex entry.
    /// Silently no-op for unknown call_ids, for unary entries
    /// (caller bug — grant for a unary call makes no sense),
    /// and for server-streaming entries (grants apply only to
    /// the upload direction).
    ///
    /// `from_node` is gated by the same target-binding check
    /// as `deliver`: a grant from a non-target session peer is
    /// dropped (a forged grant on a shared reply channel can't
    /// inject credit into a victim's call).
    ///
    /// Bidi streaming plan (Phase C).
    fn deliver_grant(
        &self,
        call_id: u64,
        from_node: super::super::behavior::placement::NodeId,
        credits: u32,
    ) {
        let entry = self.senders.get(&call_id);
        let Some(entry) = entry else { return };
        let (target_node, _entry_value) = entry.value();
        if *target_node != 0 && *target_node != from_node {
            tracing::trace!(
                call_id,
                from_node,
                expected = *target_node,
                "rpc client: dropping REQUEST_GRANT from non-target session peer"
            );
            return;
        }
        match entry.value() {
            (_, PendingEntry::ClientStreaming { grant_tx, .. })
            | (_, PendingEntry::Duplex { grant_tx, .. }) => {
                let _ = grant_tx.send(credits);
            }
            // Unary / Streaming entries silently ignore — see
            // method docs for the rationale.
            _ => {}
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

    /// Production-path entry point. Mesh dispatch calls this with
    /// the AEAD-verified session peer's `NodeId` in
    /// `ev.from_node`; the pending registry's S-4 binding gate
    /// uses it to reject responses from the wrong target.
    pub fn apply_inbound(&mut self, ev: &RpcInboundEvent) {
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = ev.payload.len(),
                "rpc client fold: event payload too short for EventMeta; skipping",
            );
            return;
        };
        match meta.dispatch {
            DISPATCH_RPC_RESPONSE => {
                match RpcResponsePayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
                    Ok(resp) => self.pending.deliver(meta.seq_or_ts, ev.from_node, resp),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            call_id = meta.seq_or_ts,
                            "rpc client fold: malformed response payload",
                        );
                    }
                }
            }
            DISPATCH_RPC_REQUEST_GRANT => {
                // Server granted upload credit for a
                // client-streaming / duplex call. Route it to the
                // matching pending entry's grant mpsc; non-client-
                // streaming entries silently ignore (see
                // RpcClientPending::deliver_grant docs).
                match decode_request_grant(&ev.payload[EVENT_META_SIZE..]) {
                    Some(grant) => {
                        // The payload's `call_id` MUST agree with
                        // the EventMeta's `seq_or_ts`: producer
                        // encodes both to the same value (see
                        // `RpcRequestGrantPayload::call_id` docs).
                        // If they disagree, the frame is malformed
                        // or forged — drop it. Otherwise a peer
                        // could publish a GRANT whose meta names
                        // one call but whose payload credits a
                        // different in-flight call_id.
                        if grant.call_id != meta.seq_or_ts {
                            tracing::debug!(
                                meta_call_id = meta.seq_or_ts,
                                payload_call_id = grant.call_id,
                                "rpc client fold: REQUEST_GRANT meta/payload call_id mismatch; dropping",
                            );
                            return;
                        }
                        if grant.credits == 0 {
                            return;
                        }
                        self.pending
                            .deliver_grant(grant.call_id, ev.from_node, grant.credits);
                    }
                    None => {
                        tracing::debug!(
                            call_id = meta.seq_or_ts,
                            "rpc client fold: malformed REQUEST_GRANT payload"
                        );
                    }
                }
            }
            _ => {
                // Unknown / unexpected dispatch on the reply
                // channel — ignore (a misconfigured publisher
                // shouldn't take down the fold).
            }
        }
    }
}

impl RedexFold<()> for RpcClientFold {
    /// Legacy entry point used by loopback / test paths that
    /// don't have a session peer to resolve. Calls `deliver`
    /// with `from_node = 0`, which the pending registry treats
    /// as "no binding" — callers that registered with
    /// `target_node = 0` accept it, callers that registered
    /// with a real target reject it.
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
        // Route RESPONSE and REQUEST_GRANT events; ignore other
        // dispatches a misconfigured publisher might send. The
        // loopback path uses `from_node = 0` which the pending
        // registry treats as "no binding" — see the apply_inbound
        // production-path counterpart above for the AEAD-verified
        // peer routing.
        match meta.dispatch {
            DISPATCH_RPC_RESPONSE => {
                match RpcResponsePayload::decode(ev.payload.slice(EVENT_META_SIZE..)) {
                    Ok(resp) => self.pending.deliver(meta.seq_or_ts, 0, resp),
                    Err(e) => {
                        // Malformed RESPONSE on the reply channel.
                        // We can't fabricate a synthetic response
                        // (the call_id might be valid; we just
                        // can't tell what it was supposed to
                        // mean). Log and leave the pending entry
                        // intact — the caller's deadline /
                        // cancellation path will eventually clean
                        // it up.
                        tracing::warn!(
                            error = %e,
                            call_id = meta.seq_or_ts,
                            "rpc client fold: malformed response payload",
                        );
                    }
                }
            }
            DISPATCH_RPC_REQUEST_GRANT => {
                match decode_request_grant(&ev.payload[EVENT_META_SIZE..]) {
                    Some(grant) => {
                        // See `apply_inbound` REQUEST_GRANT arm for
                        // the meta/payload call_id invariant.
                        if grant.call_id != meta.seq_or_ts {
                            tracing::debug!(
                                meta_call_id = meta.seq_or_ts,
                                payload_call_id = grant.call_id,
                                "rpc client fold: REQUEST_GRANT meta/payload call_id mismatch; dropping",
                            );
                            return Ok(());
                        }
                        if grant.credits == 0 {
                            return Ok(());
                        }
                        self.pending.deliver_grant(grant.call_id, 0, grant.credits);
                    }
                    None => {
                        tracing::debug!(
                            call_id = meta.seq_or_ts,
                            "rpc client fold: malformed REQUEST_GRANT payload"
                        );
                    }
                }
            }
            _ => {}
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
            (RpcStatus::CapabilityDenied, 0x0008),
        ] {
            assert_eq!(status.to_wire(), expected, "{status:?}");
            assert_eq!(RpcStatus::from_wire(expected), status);
        }
    }

    /// Reserved numeric range (`0x0009..=0x7FFF`) decodes as
    /// `Application(v)` for forward-compat with future canonical
    /// assignments. A future status numbered `0x0009` would round-
    /// trip via `from_wire(0x0009)` until that variant is added,
    /// at which point the variant takes precedence.
    #[test]
    fn reserved_status_range_decodes_as_application_for_forward_compat() {
        let decoded = RpcStatus::from_wire(0x0009);
        assert_eq!(decoded, RpcStatus::Application(0x0009));
        assert_eq!(decoded.to_wire(), 0x0009);
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
        assert_eq!(DISPATCH_RPC_STREAM_GRANT, 0x14);
        assert_eq!(DISPATCH_RPC_REQUEST_CHUNK, 0x15);
        assert_eq!(DISPATCH_RPC_REQUEST_GRANT, 0x16);
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
            body: Bytes::new(),
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
            body: Bytes::from(vec![0; MAX_RPC_BODY_LEN + 1]),
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
            body: Bytes::new(),
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
            body: Bytes::from_static(b"{\"hello\":\"world\"}"),
        };
        assert_eq!(req.encoded_len(), req.encode().len());

        let resp = RpcResponsePayload {
            status: RpcStatus::Application(0x8001),
            headers: vec![header("content-type", b"application/json")],
            body: Bytes::from_static(b"ok"),
        };
        assert_eq!(resp.encoded_len(), resp.encode().len());

        // Empty edge cases.
        let empty_req = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: Bytes::new(),
        };
        assert_eq!(empty_req.encoded_len(), empty_req.encode().len());
        let empty_resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
        };
        assert_eq!(empty_resp.encoded_len(), empty_resp.encode().len());
    }

    /// Bit 0 of `RpcRequestPayload::flags` is reserved (was the
    /// removed `FLAG_RPC_IDEMPOTENT`). Pin: live flag constants
    /// must NOT collide with bit 0, so a future re-add can safely
    /// reuse it without breaking existing senders.
    #[test]
    fn flag_bit_assignments_leave_idempotent_slot_reserved() {
        // Bit 0 (1 << 0) and bit 3 (1 << 3) are reserved; live flags
        // occupy other bits. Pinning the exact assignments here so
        // a renumber that collides with bit 0 (future `IDEMPOTENT`
        // re-add) or bit 3 (held in reserve for a future protocol
        // flag) surfaces in the test suite before it ships.
        assert_eq!(FLAG_RPC_STREAMING_RESPONSE, 1 << 1);
        assert_eq!(FLAG_RPC_PROPAGATE_TRACE, 1 << 2);
        assert_eq!(FLAG_RPC_CLIENT_STREAMING_REQUEST, 1 << 4);
        assert_eq!(FLAG_RPC_REQUEST_END, 1 << 5);
        for flag in [
            FLAG_RPC_STREAMING_RESPONSE,
            FLAG_RPC_PROPAGATE_TRACE,
            FLAG_RPC_CLIENT_STREAMING_REQUEST,
            FLAG_RPC_REQUEST_END,
        ] {
            assert_eq!(
                flag & (1 << 0),
                0,
                "flag {flag:#06x} collides with reserved bit 0"
            );
            assert_eq!(
                flag & (1 << 3),
                0,
                "flag {flag:#06x} collides with reserved bit 3"
            );
        }
    }

    // --------------------------------------------------------------------
    // Bidi streaming (Phase A) — RpcRequestChunkPayload and
    // RpcRequestGrantPayload wire-stability tests.
    // --------------------------------------------------------------------

    /// 1/5 — RequestChunk round-trip with realistic header set and
    /// 1 KiB body. Pins the encode/decode loop on the full shape.
    #[test]
    fn request_chunk_roundtrip_with_headers_and_body() {
        let mut headers = Vec::new();
        for i in 0..10u8 {
            headers.push(header(&format!("x-chunk-meta-{i}"), &[0xAA, 0xBB, i, !i]));
        }
        let body: Vec<u8> = (0..1024u32).map(|n| (n & 0xFF) as u8).collect();
        let p = RpcRequestChunkPayload {
            call_id: 0xCAFE_F00D_DEAD_BEEF,
            flags: FLAG_RPC_REQUEST_END | FLAG_RPC_PROPAGATE_TRACE,
            headers,
            body: Bytes::from(body),
        };
        let bytes = p.encode();
        assert_eq!(
            p.encoded_len(),
            bytes.len(),
            "encoded_len must agree with encode().len()"
        );
        let decoded = RpcRequestChunkPayload::decode(Bytes::from(bytes)).expect("decode");
        assert_eq!(decoded, p);
    }

    /// 2/5 — truncation rejection at every field boundary. The
    /// codec must error rather than panic / allocate-unbounded on
    /// any short slice.
    #[test]
    fn request_chunk_decode_rejects_truncation_at_every_boundary() {
        let p = RpcRequestChunkPayload {
            call_id: 0x1234,
            flags: 0,
            headers: vec![header("x", b"y")],
            body: Bytes::from_static(b"hello"),
        };
        let full = p.encode();
        // Walk every prefix shorter than the full encoding; every
        // one must produce a Truncated / TooLarge / InvalidUtf8
        // error, not panic.
        for n in 0..full.len() {
            let prefix = &full[..n];
            let result = RpcRequestChunkPayload::decode(Bytes::copy_from_slice(prefix));
            assert!(result.is_err(), "n={n}: expected Err, got Ok({:?})", result);
        }
        // Full length must decode cleanly.
        assert!(RpcRequestChunkPayload::decode(Bytes::from(full)).is_ok());
    }

    /// 3/5 — body length cap rejection. A wire-claimed body length
    /// over `MAX_RPC_BODY_LEN` must error rather than try to
    /// allocate 4+ MiB of garbage.
    #[test]
    fn request_chunk_decode_rejects_oversized_body_length() {
        // Build a synthetic encoding by hand: small valid prefix
        // up to body_len, then claim body_len = MAX_RPC_BODY_LEN + 1.
        let mut buf = Vec::new();
        buf.put_u64_le(0x42); // call_id
        buf.put_u16_le(0); // flags
        buf.put_u8(0); // headers count = 0
        buf.put_u32_le((MAX_RPC_BODY_LEN + 1) as u32);
        // (no body bytes follow — we want the decoder to reject at
        // the length check before it even tries to read body bytes)
        let err = RpcRequestChunkPayload::decode(Bytes::from(buf))
            .expect_err("oversized body length must reject");
        match err {
            RpcCodecError::TooLarge {
                field,
                actual,
                limit,
            } => {
                assert_eq!(field, "body");
                assert_eq!(actual, MAX_RPC_BODY_LEN + 1);
                assert_eq!(limit, MAX_RPC_BODY_LEN);
            }
            other => panic!("expected TooLarge {{ field=body }}, got {other:?}"),
        }
    }

    /// 4/5 — header count cap rejection. A header count over
    /// `MAX_RPC_HEADERS` must error before the per-header decode
    /// loop even starts.
    #[test]
    fn request_chunk_decode_rejects_oversized_header_count() {
        let mut buf = Vec::new();
        buf.put_u64_le(0x42); // call_id
        buf.put_u16_le(0); // flags
        buf.put_u8((MAX_RPC_HEADERS + 1) as u8); // over the cap
        let err = RpcRequestChunkPayload::decode(Bytes::from(buf))
            .expect_err("oversized header count must reject");
        match err {
            RpcCodecError::TooLarge {
                field,
                actual,
                limit,
            } => {
                // The shared `decode_headers` helper reports this
                // field as "headers".
                assert_eq!(field, "headers");
                assert_eq!(actual, MAX_RPC_HEADERS + 1);
                assert_eq!(limit, MAX_RPC_HEADERS);
            }
            other => panic!("expected TooLarge {{ field=headers }}, got {other:?}"),
        }
    }

    /// 5/5 — RequestGrant round-trip + truncation rejection. The
    /// payload is fixed-size (12 bytes), so the test surface is
    /// "exactly 12 bytes decodes" + "any other length errors".
    #[test]
    fn request_grant_roundtrip_and_truncation_rejection() {
        // Round-trip across the full u32 range corners + an
        // arbitrary mid-value.
        for (call_id, credits) in [
            (0u64, 0u32),
            (1, 1),
            (0xFFFF_FFFF_FFFF_FFFF, 0xFFFF_FFFF),
            (0xCAFE_F00D, 0x10203040),
        ] {
            let bytes = encode_request_grant(call_id, credits);
            assert_eq!(bytes.len(), 12, "request grant is always 12 bytes");
            let decoded = decode_request_grant(&bytes).expect("decode");
            assert_eq!(decoded.call_id, call_id);
            assert_eq!(decoded.credits, credits);
        }
        // Wrong-length payloads must reject (return None), not
        // panic. Empty, short, long, off-by-one each get covered.
        assert!(decode_request_grant(&[]).is_none());
        assert!(decode_request_grant(&[0u8; 11]).is_none());
        assert!(decode_request_grant(&[0u8; 13]).is_none());
    }

    /// Bonus pin: `parse_request_window_initial` extracts a valid
    /// u32 ASCII-decimal header and rejects everything else.
    /// Same coverage shape as `parse_stream_window_initial`'s
    /// implicit contract, made explicit here so the request-side
    /// helper doesn't drift away from the response-side one.
    #[test]
    fn parse_request_window_initial_matches_response_side_semantics() {
        // Happy path.
        let headers = vec![header(HEADER_NRPC_REQUEST_WINDOW_INITIAL, b"32")];
        assert_eq!(parse_request_window_initial(&headers), Some(32));
        // Case-insensitive on header name.
        let headers = vec![header("Nrpc-Request-Window-Initial", b"7")];
        assert_eq!(parse_request_window_initial(&headers), Some(7));
        // Absent.
        assert_eq!(parse_request_window_initial(&[]), None);
        // Malformed value (non-numeric).
        let headers = vec![header(HEADER_NRPC_REQUEST_WINDOW_INITIAL, b"twelve")];
        assert_eq!(parse_request_window_initial(&headers), None);
        // Malformed value (non-utf8 bytes).
        let headers = vec![header(HEADER_NRPC_REQUEST_WINDOW_INITIAL, &[0xFF, 0xFE])];
        assert_eq!(parse_request_window_initial(&headers), None);
        // Empty value.
        let headers = vec![header(HEADER_NRPC_REQUEST_WINDOW_INITIAL, b"")];
        assert_eq!(parse_request_window_initial(&headers), None);
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
            body: Bytes::new(),
        };
        let bytes = p.encode();
        let decoded = RpcRequestPayload::decode(Bytes::from(bytes)).unwrap();
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
            body: Bytes::from_static(b"{\"hello\":\"world\"}"),
        };
        let bytes = p.encode();
        let decoded = RpcRequestPayload::decode(Bytes::from(bytes)).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn request_decode_rejects_empty_service() {
        let bytes = vec![0x00];
        let err = RpcRequestPayload::decode(Bytes::from(bytes)).unwrap_err();
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
        let err = RpcRequestPayload::decode(Bytes::from(bytes)).unwrap_err();
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
        let err = RpcRequestPayload::decode(Bytes::from(bytes)).unwrap_err();
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
            body: Bytes::from_static(b"body"),
        };
        let bytes = p.encode();
        // Try each prefix length up to but not including the full
        // length — every one must be a decode error.
        for trim_to in 0..bytes.len() {
            let truncated = &bytes[..trim_to];
            let result = RpcRequestPayload::decode(Bytes::copy_from_slice(truncated));
            assert!(
                result.is_err(),
                "trim_to={trim_to} of {} must error, got {:?}",
                bytes.len(),
                result,
            );
        }
        // Full length must succeed.
        assert!(RpcRequestPayload::decode(Bytes::from(bytes)).is_ok());
    }

    // --------------------------------------------------------------------
    // RpcResponsePayload codec.
    // --------------------------------------------------------------------

    #[test]
    fn response_roundtrip_ok_with_body() {
        let p = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![header("content-type", b"application/json")],
            body: Bytes::from_static(b"{\"answer\":42}"),
        };
        let bytes = p.encode();
        let decoded = RpcResponsePayload::decode(Bytes::from(bytes)).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn response_roundtrip_application_status() {
        let p = RpcResponsePayload {
            status: RpcStatus::Application(0xBEEF),
            headers: vec![],
            body: Bytes::from_static(b"app-specific diagnostic"),
        };
        let bytes = p.encode();
        let decoded = RpcResponsePayload::decode(Bytes::from(bytes)).unwrap();
        assert_eq!(decoded.status, RpcStatus::Application(0xBEEF));
        assert_eq!(decoded.body, p.body);
    }

    #[test]
    fn response_decode_rejects_empty_buffer() {
        let err = RpcResponsePayload::decode(Bytes::new()).unwrap_err();
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
            body: Bytes::new(),
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
            body: Bytes::new(),
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
            body: Bytes::from_static(b"hello"),
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
        assert_eq!(resp.body.as_ref(), b"hello");
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
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await);
        let captured = captured.lock();
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Application(0xBEEF));
        assert_eq!(resp.body.as_ref(), b"bad input");
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
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await);
        let captured = captured.lock();
        let (_, _, resp) = &captured[0];
        assert_eq!(resp.status, RpcStatus::Internal);
        assert_eq!(resp.body.as_ref(), b"db timeout");
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
            body: Bytes::new(),
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
                    body: Bytes::new(),
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
            body: Bytes::new(),
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
                    body: Bytes::new(),
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
            body: Bytes::new(),
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
                            body: Bytes::from_static(b"slept the full window"),
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
            body: Bytes::new(),
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
                    body: Bytes::from_static(b"done"),
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
            body: Bytes::new(),
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
                    body: Bytes::from_static(b"finished despite cancellation"),
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
            body: Bytes::new(),
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
                    body: Bytes::new(),
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
            body: Bytes::new(),
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
            body: Bytes::new(),
        };
        let observed = run(req_with_flag).await.expect("flag set → should be Some");
        assert_eq!(observed, tc);

        // Case 3: FLAG set but headers missing → None (defensive).
        let req_flag_no_headers = RpcRequestPayload {
            service: "x".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_PROPAGATE_TRACE,
            headers: vec![],
            body: Bytes::new(),
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
        let rx = pending.register(42, 0);
        assert_eq!(pending.pending_count(), 1);

        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"hello back"),
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
            body: Bytes::new(),
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
        let _rx = pending.register(7, 0);

        // REQUEST event landing on the caller's reply channel is
        // ignored.
        let req = RpcRequestPayload {
            service: "stray".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: Bytes::new(),
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
        let rx = pending.register(5, 0);
        pending.cancel(5);
        assert_eq!(pending.pending_count(), 0);

        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::new(),
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
        let rx = pending.register(11, 0);

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
        let rx_a = pending.register(99, 0);
        let _rx_b = pending.register(99, 0);
        // The first receiver is now closed (sender dropped on
        // re-insert).
        let result = tokio::time::timeout(Duration::from_secs(1), rx_a).await;
        let inner = result.expect("must complete within 1s");
        assert!(inner.is_err(), "re-register must close prior receiver");
        assert_eq!(pending.pending_count(), 1);
    }

    /// S-4 part 2 regression: a RESPONSE whose wire `from_node`
    /// doesn't match the recorded `target_node` must not resolve
    /// the call. Without the gate, any peer with publish access
    /// to the caller's reply channel could ship a spoofed
    /// response (random call_ids from S-4 part 1 narrow the
    /// attack surface, but this gate closes the residual case
    /// of an attacker who has observed the victim's call_id via
    /// some side channel).
    #[tokio::test]
    async fn client_pending_drops_response_from_wrong_target() {
        let pending = Arc::new(RpcClientPending::new());
        let rx = pending.register(0xDEAD_BEEF, 0x42);
        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from_static(b"forged"),
        };
        // Forged from a different session peer — must drop.
        pending.deliver(0xDEAD_BEEF, 0x99, resp.clone());
        // Receiver is still parked; pending entry is intact.
        let parked = tokio::time::timeout(Duration::from_millis(50), rx).await;
        assert!(
            parked.is_err(),
            "forged RESPONSE from wrong target must not resolve the call"
        );
        assert_eq!(pending.pending_count(), 1);

        // Legitimate RESPONSE from the recorded target resolves.
        let rx2 = pending.register(0xCAFE, 0x42);
        let ok_resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: Vec::new(),
            body: Bytes::from_static(b"ok"),
        };
        pending.deliver(0xCAFE, 0x42, ok_resp);
        let delivered = tokio::time::timeout(Duration::from_millis(50), rx2)
            .await
            .expect("must complete")
            .expect("must receive");
        assert_eq!(delivered.body.as_ref(), b"ok");
    }

    // ====================================================================
    // Phase C — RpcClientPending + RpcClientFold for client-streaming.
    // ====================================================================

    /// Build a REQUEST_GRANT event for tests. Mirror of
    /// `rpc_stream_grant_event` for the request direction.
    fn rpc_request_grant_event(caller_origin: u64, call_id: u64, credits: u32) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST_GRANT, 0, caller_origin, call_id, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + 12);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&encode_request_grant(call_id, credits));
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// `register_client_streaming` returns two halves: a terminal
    /// oneshot and a grant mpsc. A terminal RESPONSE resolves the
    /// oneshot (same shape as unary delivery); a REQUEST_GRANT
    /// for the same call_id pushes its credit onto the mpsc.
    #[tokio::test]
    async fn client_pending_client_streaming_routes_terminal_and_grants() {
        let pending = Arc::new(RpcClientPending::new());
        let (terminal_rx, mut grant_rx) = pending.register_client_streaming(0xCAFE_F00D, 0);
        // Push two grants — both should land on the mpsc.
        pending.deliver_grant(0xCAFE_F00D, 0, 3);
        pending.deliver_grant(0xCAFE_F00D, 0, 7);
        assert_eq!(grant_rx.recv().await, Some(3));
        assert_eq!(grant_rx.recv().await, Some(7));
        // Terminal RESPONSE resolves the oneshot and removes the
        // entry. Grant mpsc closes too (its sender drops with
        // the entry).
        let resp = RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"done"),
        };
        pending.deliver(0xCAFE_F00D, 0, resp.clone());
        let delivered = tokio::time::timeout(Duration::from_millis(50), terminal_rx)
            .await
            .expect("terminal must complete")
            .expect("terminal must receive");
        assert_eq!(delivered.body.as_ref(), b"done");
        // Grant mpsc now closed.
        assert_eq!(grant_rx.recv().await, None);
        assert_eq!(pending.pending_count(), 0);
    }

    /// REQUEST_GRANT from a non-target session peer is dropped
    /// without injecting credit. Same S-4-style binding gate as
    /// the RESPONSE delivery path — a forged grant on a shared
    /// reply channel can't inflate a victim's credit budget.
    #[tokio::test]
    async fn client_pending_grant_from_wrong_target_is_dropped() {
        let pending = Arc::new(RpcClientPending::new());
        let (_terminal_rx, mut grant_rx) = pending.register_client_streaming(0xCAFE_F00D, 0x42);
        // Forged grant from a different session peer — must drop.
        pending.deliver_grant(0xCAFE_F00D, 0x99, 100);
        let parked = tokio::time::timeout(Duration::from_millis(50), grant_rx.recv()).await;
        assert!(
            parked.is_err(),
            "forged REQUEST_GRANT from wrong target must not inject credit"
        );
        // Legitimate grant from the recorded target lands.
        pending.deliver_grant(0xCAFE_F00D, 0x42, 5);
        let delivered = tokio::time::timeout(Duration::from_millis(50), grant_rx.recv())
            .await
            .expect("must complete")
            .expect("must receive");
        assert_eq!(delivered, 5);
    }

    /// `deliver_grant` for an unknown call_id is a silent no-op.
    /// Same harmless-drop semantics as a STREAM_GRANT for an
    /// unknown / non-flow-controlled call (CANCEL/GRANT race is
    /// always possible).
    #[tokio::test]
    async fn client_pending_grant_for_unknown_call_id_is_no_op() {
        let pending = Arc::new(RpcClientPending::new());
        // No entry registered for this call_id.
        pending.deliver_grant(0xDEAD, 0, 42);
        // No panics, no entries created.
        assert_eq!(pending.pending_count(), 0);
    }

    /// `deliver_grant` for a unary entry is silently dropped
    /// (grants only apply to client-streaming / duplex calls).
    #[tokio::test]
    async fn client_pending_grant_for_unary_entry_is_no_op() {
        let pending = Arc::new(RpcClientPending::new());
        let _rx = pending.register(0xDEAD, 0);
        pending.deliver_grant(0xDEAD, 0, 42);
        // No state changes — entry still pending, no leak.
        assert_eq!(pending.pending_count(), 1);
    }

    /// `RpcClientFold::apply` (legacy / loopback path) routes
    /// DISPATCH_RPC_REQUEST_GRANT events through to the matching
    /// ClientStreaming entry's grant mpsc. Pins the second
    /// dispatch arm the fold gained for Phase C.
    #[tokio::test]
    async fn client_fold_routes_request_grant_to_registered_waiter() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let (_terminal_rx, mut grant_rx) = pending.register_client_streaming(0xC0DE, 0);
        let ev = rpc_request_grant_event(0xCAFE, 0xC0DE, 9);
        fold.apply(&ev, &mut ()).unwrap();
        let delivered = tokio::time::timeout(Duration::from_millis(50), grant_rx.recv())
            .await
            .expect("must complete")
            .expect("must receive");
        assert_eq!(delivered, 9);
    }

    /// `RpcClientFold::apply` ignores REQUEST_GRANT events whose
    /// payload is malformed (wrong length): no panic, no entry
    /// state change, fold returns Ok and keeps going. Mirror of
    /// the response-side malformed-payload regression.
    #[tokio::test]
    async fn client_fold_malformed_request_grant_is_logged_not_fatal() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let (_terminal_rx, mut grant_rx) = pending.register_client_streaming(0xC0DE, 0);
        // Build a GRANT event whose payload is only 4 bytes
        // (truncated — codec needs 12).
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST_GRANT, 0, 0xCAFE, 0xC0DE, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let ev = RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        };
        let result = fold.apply(&ev, &mut ());
        assert!(
            result.is_ok(),
            "malformed REQUEST_GRANT must NOT kill the fold"
        );
        // No credit landed on the mpsc.
        let parked = tokio::time::timeout(Duration::from_millis(30), grant_rx.recv()).await;
        assert!(
            parked.is_err(),
            "malformed REQUEST_GRANT must not inject credit"
        );
    }

    /// REQUEST_GRANT frames where the payload `call_id` does NOT
    /// agree with `EventMeta::seq_or_ts` must be dropped: the
    /// producer is contracted to encode both fields to the same
    /// value (see `RpcRequestGrantPayload::call_id` doc), so a
    /// mismatch is either a malformed frame or an attempted
    /// cross-call credit-injection. Without this check, a peer
    /// could publish a GRANT whose meta names one call but whose
    /// payload credits a different in-flight call_id.
    ///
    /// Regression: cubic-dev-ai bot P2 review comment on the
    /// `nrpc-streaming` branch.
    #[tokio::test]
    async fn client_fold_drops_request_grant_with_mismatched_call_ids() {
        let pending = Arc::new(RpcClientPending::new());
        let mut fold = RpcClientFold::new(pending.clone());
        let (_terminal_rx_victim, mut grant_rx_victim) =
            pending.register_client_streaming(0xC0DE, 0);
        let (_terminal_rx_other, mut grant_rx_other) = pending.register_client_streaming(0xBEEF, 0);

        // Build a hand-rolled frame: meta names call 0xC0DE,
        // payload encodes credit for call 0xBEEF. Either the
        // producer is broken or this is a forged frame; the
        // consumer must drop, not deliver.
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST_GRANT, 0, 0xCAFE, 0xC0DE, 0);
        let mut buf = Vec::with_capacity(EVENT_META_SIZE + 12);
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&encode_request_grant(0xBEEF, 5));
        let ev = RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        };
        fold.apply(&ev, &mut ()).unwrap();

        let parked_victim =
            tokio::time::timeout(Duration::from_millis(30), grant_rx_victim.recv()).await;
        assert!(
            parked_victim.is_err(),
            "mismatched REQUEST_GRANT must not credit the call named in meta",
        );
        let parked_other =
            tokio::time::timeout(Duration::from_millis(30), grant_rx_other.recv()).await;
        assert!(
            parked_other.is_err(),
            "mismatched REQUEST_GRANT must not credit the call named in payload either",
        );
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
            body: Bytes::new(),
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
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(1, 1, req), &mut ()).unwrap();
        assert!(
            wait_until(|| captured.lock().len() == 3, Duration::from_secs(2)).await,
            "expected 2 chunks + 1 terminal error",
        );
        let captured = captured.lock();
        assert_eq!(captured[0].2.body.as_ref(), b"first");
        assert_eq!(captured[1].2.body.as_ref(), b"second");
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
            body: Bytes::new(),
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
            body: Bytes::new(),
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
            body: Bytes::new(),
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

    // ====================================================================
    // Phase B — RpcStreamingRequestFold (server-side client-streaming)
    // ====================================================================

    /// Build a REQUEST_CHUNK event for tests. Mirrors
    /// `rpc_request_event` / `rpc_stream_grant_event` shape.
    fn rpc_request_chunk_event(
        caller_origin: u64,
        call_id: u64,
        flags: u16,
        body: Vec<u8>,
    ) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, caller_origin, call_id, 0);
        let payload = RpcRequestChunkPayload {
            call_id,
            flags,
            headers: vec![],
            body: body.into(),
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        buf.extend_from_slice(&payload.encode());
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// Collecting client-streaming handler: drains the stream into
    /// a Vec, returns an Ok response whose body is the count of
    /// chunks seen (8-byte LE). Captured chunk bodies are exposed
    /// via the `Arc<Mutex<Vec<Bytes>>>` so tests can assert
    /// ordering and content.
    struct CollectingClientStreamHandler {
        seen: Arc<Mutex<Vec<bytes::Bytes>>>,
        observed_cancel: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl RpcClientStreamingHandler for CollectingClientStreamHandler {
        async fn call(
            &self,
            ctx: RpcStreamingContext,
            mut requests: RequestStream,
        ) -> Result<RpcResponsePayload, RpcHandlerError> {
            use futures::StreamExt;
            while let Some(chunk) = requests.next().await {
                self.seen.lock().push(chunk);
            }
            // Re-check cancellation after EOF so the test can
            // distinguish "clean REQUEST_END" from "CANCEL closed
            // the stream early".
            if ctx.cancellation.is_cancelled() {
                self.observed_cancel
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let count = self.seen.lock().len() as u64;
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: Bytes::copy_from_slice(&count.to_le_bytes()),
            })
        }
    }

    /// 1/6 — happy path: REQUEST + 3 REQUEST_CHUNKs (last has
    /// FLAG_END) delivers 4 bodies to the handler in order; the
    /// fold emits exactly one terminal RESPONSE carrying the
    /// handler's reply.
    #[tokio::test]
    async fn streaming_request_fold_collects_all_chunks_and_emits_terminal_response() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(
            Arc::new(CollectingClientStreamHandler {
                seen: seen.clone(),
                observed_cancel: observed_cancel.clone(),
            }),
            emit,
        );
        // REQUEST with the client-streaming flag, body = "a".
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
            headers: vec![],
            body: Bytes::from_static(b"a"),
        };
        fold.apply(&rpc_request_event(0xCAFE, 7, req), &mut ())
            .unwrap();
        // Wait until the sender is registered (handler task has
        // picked up the request and the apply path completed).
        assert!(
            wait_until(
                || fold.sender_keys().contains(&(0xCAFE, 7)),
                Duration::from_secs(1)
            )
            .await
        );
        // Three more chunks; last sets FLAG_REQUEST_END.
        fold.apply(
            &rpc_request_chunk_event(0xCAFE, 7, 0, b"b".to_vec()),
            &mut (),
        )
        .unwrap();
        fold.apply(
            &rpc_request_chunk_event(0xCAFE, 7, 0, b"c".to_vec()),
            &mut (),
        )
        .unwrap();
        fold.apply(
            &rpc_request_chunk_event(0xCAFE, 7, FLAG_RPC_REQUEST_END, b"d".to_vec()),
            &mut (),
        )
        .unwrap();
        // Handler should observe 4 bodies in order and emit one
        // terminal RESPONSE whose body encodes the count.
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1, "exactly one terminal RESPONSE");
        let (origin, call_id, resp) = &captured[0];
        assert_eq!(*origin, 0xCAFE);
        assert_eq!(*call_id, 7);
        assert_eq!(resp.status, RpcStatus::Ok);
        assert_eq!(resp.body.as_ref(), 4u64.to_le_bytes());
        // And the chunks landed in order.
        let seen = seen.lock();
        let collected: Vec<&[u8]> = seen.iter().map(|b| b.as_ref()).collect();
        assert_eq!(collected, vec![b"a", b"b", b"c", b"d"]);
        assert!(
            !observed_cancel.load(std::sync::atomic::Ordering::SeqCst),
            "clean REQUEST_END must NOT register as a cancellation"
        );
    }

    /// 2/6 — degenerate case: initial REQUEST with both the
    /// client-streaming AND request-end flags set. Handler sees
    /// exactly one body (the REQUEST's own body) and EOF — the
    /// "one-item upload" fast path that saves a trailing CHUNK
    /// event.
    #[tokio::test]
    async fn streaming_request_fold_initial_request_with_end_flag_yields_single_item() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(
            Arc::new(CollectingClientStreamHandler {
                seen: seen.clone(),
                observed_cancel,
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_REQUEST_END,
            headers: vec![],
            body: Bytes::from_static(b"only"),
        };
        fold.apply(&rpc_request_event(1, 42, req), &mut ()).unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].2.status, RpcStatus::Ok);
        assert_eq!(captured[0].2.body.as_ref(), 1u64.to_le_bytes());
        assert_eq!(
            seen.lock()
                .iter()
                .map(|b| b.as_ref())
                .collect::<Vec<&[u8]>>(),
            vec![b"only" as &[u8]]
        );
        // Sender must NOT have been registered (initial-REQUEST-
        // with-END skips the map insert).
        assert!(fold.sender_keys().is_empty());
    }

    /// 3/6 — CANCEL closes the request stream early, flips the
    /// cancellation token, and the spawned task overrides the
    /// handler's terminal with `RpcStatus::Cancelled`.
    #[tokio::test]
    async fn streaming_request_fold_cancel_closes_stream_and_overrides_terminal() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(
            Arc::new(CollectingClientStreamHandler {
                seen: seen.clone(),
                observed_cancel: observed_cancel.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
            headers: vec![],
            body: Bytes::from_static(b"first"),
        };
        fold.apply(&rpc_request_event(2, 17, req), &mut ()).unwrap();
        // Wait for the handler to register, then one in-flight
        // CHUNK, then CANCEL before the handler ever finishes
        // draining.
        assert!(
            wait_until(
                || fold.sender_keys().contains(&(2, 17)),
                Duration::from_secs(1)
            )
            .await
        );
        fold.apply(
            &rpc_request_chunk_event(2, 17, 0, b"second".to_vec()),
            &mut (),
        )
        .unwrap();
        fold.apply(&rpc_cancel_event(2, 17), &mut ()).unwrap();
        // Terminal must arrive and must be Cancelled (CANCEL-wins
        // ordering, same as the response-side fold).
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].2.status,
            RpcStatus::Cancelled,
            "CANCEL must override terminal status"
        );
        assert!(
            observed_cancel.load(std::sync::atomic::Ordering::SeqCst),
            "handler must observe cancellation token after stream EOF"
        );
        // Both maps must be clean post-cancel.
        assert!(fold.in_flight_keys().is_empty());
        assert!(fold.sender_keys().is_empty());
    }

    /// 4/6 — handler returns `Err(RpcHandlerError::Application)`
    /// → terminal RESPONSE carries the application status code +
    /// message body.
    #[tokio::test]
    async fn streaming_request_fold_application_error_round_trips() {
        struct AppErrHandler;
        #[async_trait::async_trait]
        impl RpcClientStreamingHandler for AppErrHandler {
            async fn call(
                &self,
                _ctx: RpcStreamingContext,
                mut requests: RequestStream,
            ) -> Result<RpcResponsePayload, RpcHandlerError> {
                use futures::StreamExt;
                // Drain so the stream's EOF doesn't race the
                // error return.
                while requests.next().await.is_some() {}
                Err(RpcHandlerError::Application {
                    code: 0xBEEF,
                    message: "bad input".to_string(),
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(Arc::new(AppErrHandler), emit);
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_REQUEST_END,
            headers: vec![],
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(3, 100, req), &mut ())
            .unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].2.status, RpcStatus::Application(0xBEEF));
        assert_eq!(captured[0].2.body.as_ref(), b"bad input");
    }

    /// 5/6 — handler panic is caught by `catch_unwind`; terminal
    /// surfaces as `Internal` carrying the panic message. Same
    /// contract as the existing folds — a misbehaving handler
    /// can't take down the cortex adapter.
    #[tokio::test]
    async fn streaming_request_fold_handler_panic_surfaces_as_internal() {
        struct PanickyHandler;
        #[async_trait::async_trait]
        impl RpcClientStreamingHandler for PanickyHandler {
            async fn call(
                &self,
                _ctx: RpcStreamingContext,
                _requests: RequestStream,
            ) -> Result<RpcResponsePayload, RpcHandlerError> {
                panic!("intentional test panic");
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(Arc::new(PanickyHandler), emit);
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_REQUEST_END,
            headers: vec![],
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(4, 200, req), &mut ())
            .unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE"
        );
        let captured = captured.lock();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].2.status, RpcStatus::Internal);
        assert!(
            String::from_utf8_lossy(&captured[0].2.body).contains("intentional test panic"),
            "panic body should carry the panic message"
        );
    }

    /// 6/6 — duplicate REQUEST with the same `(origin, call_id)`
    /// is refused with a synthetic `Internal` terminal frame and
    /// does NOT spawn a second handler. Mirror of the regression
    /// pinned in the unary + response-streaming folds.
    #[tokio::test]
    async fn streaming_request_fold_duplicate_request_refuses_without_double_dispatch() {
        let invocations = Arc::new(AtomicUsize::new(0));
        struct CountingHandler {
            invocations: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl RpcClientStreamingHandler for CountingHandler {
            async fn call(
                &self,
                _ctx: RpcStreamingContext,
                mut requests: RequestStream,
            ) -> Result<RpcResponsePayload, RpcHandlerError> {
                use futures::StreamExt;
                self.invocations.fetch_add(1, Ordering::SeqCst);
                // Slow handler to keep the call in-flight while
                // the duplicate REQUEST arrives.
                tokio::time::sleep(Duration::from_millis(80)).await;
                while requests.next().await.is_some() {}
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: Bytes::new(),
                })
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcStreamingRequestFold::new(
            Arc::new(CountingHandler {
                invocations: invocations.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
            headers: vec![],
            body: Bytes::new(),
        };
        fold.apply(&rpc_request_event(5, 99, req.clone()), &mut ())
            .unwrap();
        assert!(
            wait_until(
                || fold.in_flight_keys().contains(&(5, 99)),
                Duration::from_secs(1)
            )
            .await
        );
        // Duplicate REQUEST: synthetic Internal terminal emitted,
        // handler invocation count must stay at 1.
        fold.apply(&rpc_request_event(5, 99, req), &mut ()).unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(1)).await,
            "synthetic refusal terminal expected"
        );
        let refusal = captured.lock()[0].clone();
        assert_eq!(refusal.2.status, RpcStatus::Internal);
        assert!(String::from_utf8_lossy(&refusal.2.body).contains("duplicate"));
        // Finish the first handler so its terminal lands too.
        fold.apply(
            &rpc_request_chunk_event(5, 99, FLAG_RPC_REQUEST_END, vec![]),
            &mut (),
        )
        .unwrap();
        assert!(
            wait_until(|| captured.lock().len() >= 2, Duration::from_secs(2)).await,
            "first handler should still complete normally"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate REQUEST must NOT spawn a second handler",
        );
    }
}
