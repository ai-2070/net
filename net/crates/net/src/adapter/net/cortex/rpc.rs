//! nRPC — request/response on top of CortEX folds.
//!
//! See `docs/internal/misc/NRPC_DESIGN.md` for the full architectural framing.
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
// OA2-E0.2 — RpcRouteV1 frame discriminator.
//
// Every nRPC frame is laid out `EventMeta ‖ RpcRouteV1 ‖ payload`,
// where RpcRouteV1 is the CANONICAL u64 ChannelHash of the physical
// channel the frame rides (caller → provider: `<service>.requests`;
// provider → caller: the reply channel). The wire packet header
// carries only a `u16` bucket, which can collide; this discriminator
// lets mesh ingress select EXACTLY ONE registered canonical
// dispatcher instead of fanning the frame to every candidate and
// asking each fold to self-filter (the removed ambiguity).
//
// This is a coordinated nRPC frame-version change — every nRPC
// sender writes it and mesh ingress requires it for RPC event types.
// ============================================================================

/// Size of the [`RpcRouteV1`](encode_rpc_route) discriminator: one
/// canonical u64 `ChannelHash`.
pub const RPC_ROUTE_V1_SIZE: usize = 8;

/// Byte offset of the frame-specific payload inside an nRPC frame:
/// past the `EventMeta` prefix AND the RpcRouteV1 discriminator.
pub const RPC_FRAME_BODY_OFFSET: usize = EVENT_META_SIZE + RPC_ROUTE_V1_SIZE;

/// Append the RpcRouteV1 discriminator (`canonical` channel hash)
/// to a frame buffer that already holds the `EventMeta` prefix
/// (OA2-E0.2). Every nRPC frame builder calls this immediately
/// after `meta.to_bytes()`.
#[inline]
pub fn encode_rpc_route(buf: &mut Vec<u8>, canonical: crate::adapter::net::channel::ChannelHash) {
    buf.extend_from_slice(&canonical.to_le_bytes());
}

/// Insert the RpcRouteV1 discriminator into an already-assembled
/// `EventMeta ‖ payload` frame, producing `EventMeta ‖ route ‖
/// payload` (OA2-E0.2). Used at the centralized response publish
/// choke point (`publish_response_to_caller`), where the frame is
/// built before the reply-channel hash is threaded in — one small
/// copy per response frame. Direct request-direction builders use
/// [`encode_rpc_route`] inline instead (no copy). Frames shorter
/// than `EVENT_META_SIZE` are returned unchanged (never a real
/// nRPC frame).
pub fn insert_rpc_route(
    frame: bytes::Bytes,
    canonical: crate::adapter::net::channel::ChannelHash,
) -> bytes::Bytes {
    if frame.len() < EVENT_META_SIZE {
        return frame;
    }
    let mut out = Vec::with_capacity(frame.len() + RPC_ROUTE_V1_SIZE);
    out.extend_from_slice(&frame[..EVENT_META_SIZE]);
    out.extend_from_slice(&canonical.to_le_bytes());
    out.extend_from_slice(&frame[EVENT_META_SIZE..]);
    bytes::Bytes::from(out)
}

/// Read the RpcRouteV1 discriminator from a full nRPC frame
/// (`EventMeta ‖ route ‖ payload`). `None` if the frame is too
/// short to carry it — a malformed/legacy frame that ingress drops.
#[inline]
pub fn decode_rpc_route(frame: &[u8]) -> Option<crate::adapter::net::channel::ChannelHash> {
    let bytes = frame.get(EVENT_META_SIZE..EVENT_META_SIZE + RPC_ROUTE_V1_SIZE)?;
    let arr: [u8; RPC_ROUTE_V1_SIZE] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(arr))
}

/// `true` iff `event_type` is an nRPC dispatch frame that carries
/// the RpcRouteV1 discriminator (OA2-E0.2). Mesh ingress uses this
/// to apply route-based single-select to RPC frames while leaving
/// non-RPC dispatcher registrations (e.g. the sensing intake) on
/// the legacy fan-out path.
#[inline]
pub fn is_rpc_dispatch_frame(event_type: u8) -> bool {
    matches!(
        event_type,
        DISPATCH_RPC_REQUEST
            | DISPATCH_RPC_RESPONSE
            | DISPATCH_RPC_CANCEL
            | DISPATCH_RPC_DEADLINE_EXCEEDED
            | DISPATCH_RPC_STREAM_GRANT
            | DISPATCH_RPC_REQUEST_CHUNK
            | DISPATCH_RPC_REQUEST_GRANT
    )
}

/// Peek the self-declared `service` field of an initial-REQUEST
/// frame (`EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload`) WITHOUT a
/// full payload decode (OA2-E0.2 P0).
///
/// The route discriminator (E0.2) already selected a dispatcher by
/// *canonical channel hash*, but `RpcRequestPayload` also carries
/// its OWN `service` string — the first body field (`u8` length ‖
/// bytes, see [`RpcRequestPayload::encode_into`]). The serve bridge
/// uses this to enforce `payload.service == captured_service` before
/// the capability gate or any fold state, so a frame routed to
/// `admin.requests` whose payload names `echo` never reaches the
/// admin handler.
///
/// Returns `None` when the service field is unreadable — frame too
/// short (missing the route or the length byte), an empty or
/// over-cap length, or non-UTF-8 bytes. Those exactly mirror the
/// `Err` arms of [`RpcRequestPayload::decode`], so an unreadable
/// service falls through to the fold's full decode, which rejects it
/// (`UnknownVersion`) — no handler runs either way. A `Some(svc)`
/// return borrows the service bytes straight out of `frame`.
///
/// Only meaningful for `DISPATCH_RPC_REQUEST` frames; control frames
/// (CANCEL / CHUNK / GRANT) carry no service and inherit the
/// route-selected active call, so callers gate on the dispatch type
/// before calling this.
#[inline]
pub fn peek_request_service(frame: &[u8]) -> Option<&str> {
    let body = frame.get(RPC_FRAME_BODY_OFFSET..)?;
    let (&svc_len, rest) = body.split_first()?;
    let svc_len = svc_len as usize;
    if svc_len == 0 || svc_len > MAX_RPC_SERVICE_NAME_LEN {
        return None;
    }
    let svc = rest.get(..svc_len)?;
    std::str::from_utf8(svc).ok()
}

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
    /// `docs/internal/plans/CAPABILITY_AUTH_PLAN.md` §3. Distinct from
    /// `Unauthorized` (channel-auth / token-scope failures) so
    /// operators can tell the two enforcement surfaces apart in
    /// audit logs.
    /// gRPC equivalent: `PERMISSION_DENIED` (7) — same outward
    /// shape as `Unauthorized` but a separate substrate code.
    CapabilityDenied = 0x0008,
    /// OA-2 org admission (E2.2): a PROTECTED service denied this call —
    /// the caller's `net-org-admission` proof failed verification, the
    /// provider cannot admit right now, or the call shape is unsupported.
    /// A COARSE reason (denied / not-supported / unavailable) rides the
    /// response; the DETAILED `AdmissionDenied` variant stays provider-
    /// side audit only, so denial is not a credential oracle. Distinct
    /// from `CapabilityDenied` (v0.4 allow-list) and `Unauthorized`
    /// (channel-auth / token-scope) so operators can tell the
    /// enforcement surfaces apart.
    /// gRPC equivalent: `PERMISSION_DENIED` (7).
    AdmissionDenied = 0x0009,
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
            Self::AdmissionDenied => 0x0009,
            Self::Application(v) => v,
        }
    }

    /// Decode from the wire `u16`. Reserved values
    /// (`0x000A..=0x7FFF`) decode as `Application(v)` rather than
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
            0x0009 => Self::AdmissionDenied,
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
    /// perf #84 in `docs/internal/performance/net-perf-analysis.md` this was
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
    /// Validate every length field against the wire ceilings the codec's
    /// `u8` / `u16` / `u32` length prefixes assume, BEFORE any encode.
    /// In release builds [`Self::encode_into`] truncates an over-cap
    /// length via its `as u8` / `as u16` / `as u32` casts (debug builds
    /// `debug_assert`), which would let an oversized, publicly-
    /// constructed request produce an ambiguous / non-round-tripping
    /// encoding. Any caller that must NOT silently truncate — the org
    /// request digest (AV-7 item 7), and any future checked send path —
    /// validates first and refuses rather than hashing a collision-prone
    /// encoding.
    pub fn validate_wire_bounds(&self) -> Result<(), RpcCodecError> {
        if self.service.len() > MAX_RPC_SERVICE_NAME_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "service",
                actual: self.service.len(),
                limit: MAX_RPC_SERVICE_NAME_LEN,
            });
        }
        if self.headers.len() > MAX_RPC_HEADERS {
            return Err(RpcCodecError::TooLarge {
                field: "headers",
                actual: self.headers.len(),
                limit: MAX_RPC_HEADERS,
            });
        }
        for (name, value) in &self.headers {
            if name.len() > MAX_RPC_HEADER_NAME_LEN {
                return Err(RpcCodecError::TooLarge {
                    field: "header name",
                    actual: name.len(),
                    limit: MAX_RPC_HEADER_NAME_LEN,
                });
            }
            if value.len() > MAX_RPC_HEADER_VALUE_LEN {
                return Err(RpcCodecError::TooLarge {
                    field: "header value",
                    actual: value.len(),
                    limit: MAX_RPC_HEADER_VALUE_LEN,
                });
            }
        }
        if self.body.len() > MAX_RPC_BODY_LEN {
            return Err(RpcCodecError::TooLarge {
                field: "body",
                actual: self.body.len(),
                limit: MAX_RPC_BODY_LEN,
            });
        }
        Ok(())
    }

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
        self.encode_into(&mut buf);
        buf
    }

    /// Encode directly into `buf`, appending the wire bytes (audit T2.2).
    /// Callers that already hold an `EventMeta`-prefixed buffer use this to
    /// skip `encode()`'s intermediate `Vec` allocation + copy. Produces the
    /// identical bytes `encode()` returns. Pre-`reserve(encoded_len())` for an
    /// exact-fit single allocation when starting from an empty buffer.
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
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
        encode_headers(&self.headers, buf);
        // body
        debug_assert!(
            self.body.len() <= MAX_RPC_BODY_LEN,
            "body length {} exceeds MAX_RPC_BODY_LEN ({})",
            self.body.len(),
            MAX_RPC_BODY_LEN,
        );
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
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
        self.encode_into(&mut buf);
        buf
    }

    /// Encode directly into `buf`, appending the wire bytes (audit T2.2).
    /// See [`RpcRequestPayload::encode_into`].
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        // call_id
        buf.put_u64_le(self.call_id);
        // flags
        buf.put_u16_le(self.flags);
        // headers
        encode_headers(&self.headers, buf);
        // body
        debug_assert!(
            self.body.len() <= MAX_RPC_BODY_LEN,
            "body length {} exceeds MAX_RPC_BODY_LEN ({})",
            self.body.len(),
            MAX_RPC_BODY_LEN,
        );
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
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
        self.encode_into(&mut buf);
        buf
    }

    /// Encode directly into `buf`, appending the wire bytes (audit T2.2).
    /// See [`RpcRequestPayload::encode_into`].
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        buf.put_u16_le(self.status.to_wire());
        encode_headers(&self.headers, buf);
        debug_assert!(
            self.body.len() <= MAX_RPC_BODY_LEN,
            "body length {} exceeds MAX_RPC_BODY_LEN ({})",
            self.body.len(),
            MAX_RPC_BODY_LEN,
        );
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
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
    // OA2-E0.2: EventMeta + RpcRouteV1 discriminator + payload.
    RPC_FRAME_BODY_OFFSET + payload.encoded_len()
}

/// Same for `RpcResponsePayload` after the `EventMeta` prefix in a
/// `DISPATCH_RPC_RESPONSE` event.
pub fn response_wire_size(payload: &RpcResponsePayload) -> usize {
    RPC_FRAME_BODY_OFFSET + payload.encoded_len()
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
    /// The **receiving session's** id (R2), resolved from the
    /// AEAD-verified packet at ingress.
    ///
    /// Authorization and enrollment ownership are properties of the
    /// incarnation that actually carried the request. The event
    /// used to carry only `from_node`, so a request queued in the
    /// bridge and drained after a reconnection captured the
    /// *current* session instead of its own. `0` on loopback/test
    /// paths that have no session, like `from_node`.
    pub session_id: u64,
    /// Canonical [`ChannelHash`](crate::adapter::net::channel::ChannelHash)
    /// (u32) of the channel this event arrived on — widened from the
    /// per-packet wire `u16` `NetHeader::channel_hash` via the
    /// registered-dispatcher table at receive time.
    /// Collision-resistant at realistic scale; the wire `u16` may
    /// bucket-collide but the canonical hash uniquely identifies the
    /// registered dispatcher target.
    pub channel_hash: super::super::channel::ChannelHash,
    /// Caller's `origin_hash` from the packet header — the full
    /// 64-bit `EntityKeypair::origin_hash()` mirroring the wire
    /// field's width post-`WIRE_ORIGIN_HASH_64BIT`. The dispatcher
    /// should treat this as routing metadata, not identity
    /// authentication (the AEAD-verified `session_node` field
    /// below carries that).
    pub origin_hash: u64,
    /// Wire-session peer's `NodeId` resolved at packet receive
    /// time from the AEAD-verified session_id. Distinct from
    /// `origin_hash`: this is the full 64-bit network identity
    /// of the peer that delivered the packet. Used by
    /// `RpcClientPending::deliver`
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

/// Context handed to a `RpcHandler::call`. Carries what the handler
/// needs to fulfill the request: caller routing attribution, the
/// request payload, and a cancellation token.
///
/// **This context does not carry an authenticated end-to-end caller
/// identity.** See [`RpcContext::caller_origin`]; handlers that need
/// to authorize a caller must either run behind the PROTECTED-service
/// admission gate (and read [`RpcContext::org_admission`]) or carry
/// their own application-level signature.
///
/// # Construction
///
/// `#[non_exhaustive]`: handlers *receive* this type, they do not build
/// it, and the 0.35 addition of [`session_peer`](Self::session_peer)
/// showed what the alternative costs — a new field is a compile break
/// for every downstream literal. Nothing outside this crate constructs
/// one today, so the attribute takes nothing away and means the next
/// field this gains is additive.
#[non_exhaustive]
pub struct RpcContext {
    /// Caller's `origin_hash`, copied verbatim from the inbound
    /// packet header ([`RpcInboundEvent::origin_hash`]).
    ///
    /// **Routing metadata, not identity authentication — do not
    /// authorize on this.** It is a value carried on the wire, so a
    /// peer chooses what it says. Comparing it against an identity
    /// claimed elsewhere in the request compares two claims from the
    /// same untrusted source and proves nothing.
    ///
    /// The authenticated fields, and what they actually mean:
    ///
    /// - the AEAD-verified *wire-session peer* is the node that
    ///   delivered the packet — the last hop, not necessarily the
    ///   originator (it is `RpcInboundEvent::from_node`, the wire-session
    ///   peer's `NodeId`);
    /// - [`RpcContext::org_admission`] carries a verified four-party
    ///   identity, but only for calls admitted through the
    ///   PROTECTED-service gate; it is `None` for public calls.
    ///
    /// For anything stronger on a public service, the handler needs
    /// an application-level signature over a transcript that binds
    /// the destination and carries its own freshness.
    pub caller_origin: u64,
    /// The AEAD-authenticated session peer that delivered this
    /// request — `RpcInboundEvent::from_node`.
    ///
    /// **This is the only authenticated subject a public handler
    /// gets.** Unlike [`caller_origin`](Self::caller_origin), a peer
    /// cannot choose what this says: the packet decrypted under that
    /// session's keys, so it is the node that actually sent it.
    ///
    /// What it does **not** prove: that this node *originated* the
    /// request. It is the last hop. If a deployment relays nRPC
    /// through an intermediary, requests arrive bearing the relay's
    /// node id, so an allowlist built on this field authorizes the
    /// relay and everything the relay chooses to forward. Handlers
    /// that need end-to-end provenance want a PROTECTED service and
    /// [`org_admission`](Self::org_admission), or an application-level
    /// signature over a transcript that binds the destination.
    ///
    /// Within those limits it is the same basis the RPC layer already
    /// uses for its own security decisions: response routing and the
    /// in-flight cancellation map are both keyed on `from_node`
    /// precisely so a peer that copies another's `caller_origin`
    /// cannot steer or cancel the victim's call.
    pub session_peer: u64,
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
    /// OA-2 org-admission attribution (E1.6). `Some(Admitted)` for a
    /// call that passed the PROTECTED-service admission gate — the
    /// four-party verified identity (caller, acting org, provider org,
    /// exact provider, capability). The raw `net-org-admission` proof
    /// header is STRIPPED from `payload.headers` before the handler
    /// sees it, so application code receives verified attribution, never
    /// raw credential material. `None` for public
    /// (`PublicAuthenticated`) calls, which keep existing header
    /// behavior.
    pub org_admission: Option<crate::adapter::net::behavior::org_admission::Admitted>,
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
    ///
    /// **Ordering, and where it lives.** The transport orders
    /// *delivery*; the fold preserves that for the part that is
    /// the fold's to preserve. A `Reliable` stream delivers one
    /// source's REQUESTs in sequence order, the serve bridge
    /// drains its inbound receiver from a single task, and
    /// `apply_frame` disposes of each frame — deadline refusal,
    /// duplicate refusal, in-flight registration, dispatch —
    /// before it looks at the next one.
    ///
    /// Handler **bodies** then run concurrently: each call is its
    /// own task, so which body starts first, and how they
    /// interleave, is the runtime's and is not a contract. Work
    /// that must be observed in request order belongs in something
    /// with one owner — a channel the handler sends to, an actor, a
    /// lock — not in the order handlers happen to be polled. Calls
    /// from different sources are unordered, exactly as two streams
    /// are.
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError>;
}

/// Callback the fold invokes to publish a response back to the
/// caller. Wired up by `Mesh::serve_rpc` to publish on
/// `<service>.replies.<caller_origin>`. Type-erased so the fold
/// doesn't depend on the mesh layer directly.
///
/// Arguments: `(from_node, caller_origin, call_id, response_payload)`.
/// `from_node` (R2-5) is the AEAD-authenticated session peer that
/// delivered the REQUEST — the authoritative response destination, so
/// two sessions pinned to the same entity/origin submitting the same
/// call_id each route their response to their OWN session.
/// `(from_node, receiving_session_id, caller_origin, call_id,
/// payload)`.
///
/// R2-A: the **receiving incarnation** rides with the response.
/// Enrollment ownership is `(node, session, call)`, and call ids
/// are sender-controlled, so a completion that knows only
/// `(node, call)` can consume a successor's reservation after a
/// reconnection that reuses the call id.
pub type RpcResponseEmitter =
    Arc<dyn Fn(u64, u64, u64, u64, RpcResponsePayload) + Send + Sync + 'static>;

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
    dyn Fn(u64, u64, u64, RpcResponsePayload) -> futures::future::BoxFuture<'static, ()>
        + Send
        + Sync
        + 'static,
>;

/// The per-call identity every server fold keys its state on:
/// `(from_node, receiving_session_id, caller_origin, call_id)` (R2-A,
/// C5).
///
/// `from_node` (AV-1 item 1) is the AEAD-authenticated last-hop session
/// peer, so a control frame that copies another peer's origin + call_id
/// lands under a distinct, absent key and cannot cancel or otherwise
/// mutate the victim's call. The **receiving incarnation** is part of
/// the key as well: a peer that reconnects and reuses a call id is
/// making a **new** call, and the old parked handler's entry must not
/// make it look like a duplicate of a call belonging to a session that
/// no longer exists. C5 widened the streaming folds' three-part key to
/// this same four-part identity — a late chunk / CANCEL / GRANT from a
/// *replaced* session carrying the same `(from_node, origin, call_id)`
/// misses the map entirely (session fencing), for public and protected
/// calls alike (Owner Q3).
type StreamCallKey = (u64, u64, u64, u64);

/// `(from_node, receiving_session_id, caller_origin, call_id)` →
/// cancellation token for an in-flight call. Shared across all four
/// server folds (C5: one key shape after the streaming folds gained
/// `session_id`).
type InFlightCalls = Arc<Mutex<HashMap<StreamCallKey, RpcCancellationToken>>>;

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
    /// The **receiving incarnation** of the frame currently being
    /// applied (R2-A), set by `apply_inbound*` from the event and
    /// handed to the emitter with the response. `0` on
    /// test/loopback paths, like `from_node`.
    session_id: u64,
    /// (from_node, receiving_session_id, caller_origin, call_id) →
    /// cancellation token for the in-flight handler. `from_node` is the
    /// AEAD-authenticated last-hop session peer (AV-1 item 1): binding
    /// it into the key means a peer that copies another peer's origin +
    /// call_id onto a forged CANCEL looks up a distinct, absent key and
    /// no-ops rather than cancelling the victim's call. The receiving
    /// incarnation is in the key for the same reason (R2-A). Inserted on
    /// REQUEST, removed by either the spawned handler task on completion
    /// or by the fold on CANCEL. Wrapped in `Arc<Mutex<...>>` so spawned
    /// tasks can remove their own entries without going back through
    /// the fold.
    in_flight: InFlightCalls,
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
            session_id: 0,
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

    /// Test-only: snapshot of the in-flight call set. R2-A keyed it
    /// on `(node, session, origin, call)` — the receiving
    /// incarnation is part of a call's identity.
    #[cfg(test)]
    pub fn in_flight_keys(&self) -> Vec<(u64, u64, u64, u64)> {
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

impl RpcServerFold {
    /// Production-path entry point. The serve bridge calls this with
    /// the AEAD-verified session peer's `NodeId` in `ev.from_node`;
    /// all per-call state is keyed by `(from_node, claimed_origin,
    /// call_id)` so no peer can create, cancel, or otherwise mutate
    /// another peer's call by copying its origin + call_id (AV-1
    /// item 1).
    pub fn apply_inbound(&mut self, ev: &RpcInboundEvent) -> Result<(), RedexError> {
        self.session_id = ev.session_id;
        self.apply_frame(ev.from_node, &ev.payload, None)
    }

    /// OA-2 protected entry point (E1.6). The protected serve bridge
    /// calls this AFTER `verify_org_admission` succeeds, handing the
    /// four-party [`Admitted`](crate::adapter::net::behavior::org_admission::Admitted)
    /// attribution the fold places into `RpcContext::org_admission`; the
    /// fold also STRIPS every `net-org-admission` header from the payload
    /// so the handler never sees the raw proof. Unary-only — the
    /// streaming/duplex folds have no admitted entry point (E1.8).
    ///
    /// With a registry lease (slice 1.4, contract 5's lease-carrying
    /// seam) the fold TRANSFERS ownership before any effect (§3 step 5);
    /// a transfer failure is the typed refusal the bridge routes through
    /// `emit_admission_denial`, and the lease's own Drop releases the
    /// reservation.
    pub fn apply_inbound_admitted(
        &mut self,
        ev: &RpcInboundEvent,
        admitted: crate::adapter::net::behavior::org_admission::Admitted,
        mut lease: Option<ProtectedCallLease>,
    ) -> Result<(), crate::adapter::net::behavior::org_admission::AdmissionDenied> {
        self.session_id = ev.session_id;
        let mut opening = AdmittedOpening {
            admitted,
            confirmed: None,
        };
        if let Some(lease) = lease.as_mut() {
            let registry = Arc::clone(lease.registry());
            let cancellation = RpcCancellationToken::new();
            let retire_signal = Arc::new(StreamRetireSignal::new());
            let call_ref = RegistryCallRef {
                registry: Arc::clone(&registry),
                key: lease.key.clone(),
                incarnation: lease.incarnation,
            };
            let hook_token = cancellation.clone();
            let on_retire: Arc<dyn Fn(StreamTerminalReason) + Send + Sync> =
                Arc::new(move |_reason| hook_token.cancel());
            registry.confirm(lease, retire_signal, Some(on_retire))?;
            opening.confirmed = Some(ConfirmedOpening {
                call_ref,
                cancellation,
            });
        }
        self.apply_frame(ev.from_node, &ev.payload, Some(opening))
            .map_err(|_| {
                crate::adapter::net::behavior::org_admission::AdmissionDenied::MalformedProof
            })
    }

    /// Core frame application shared by [`Self::apply_inbound`] (real
    /// authenticated `from_node`) and the [`RedexFold`] loopback shim
    /// (`from_node = 0`, test / loopback paths with no session peer).
    /// `org_admission` is `Some` only on the initial REQUEST of an
    /// admitted protected call; its attribution is placed into the
    /// handler's `RpcContext` and its presence triggers the proof-header
    /// strip.
    fn apply_frame(
        &mut self,
        from_node: u64,
        frame: &Bytes,
        org_admission: Option<AdmittedOpening>,
    ) -> Result<(), RedexError> {
        // Decode the meta header. A garbled meta means the event
        // doesn't even claim to be an RPC packet — log and skip
        // rather than killing the fold. Returning `Err(Decode)`
        // here would stop the entire cortex adapter for one
        // malformed event, which is wrong for an RPC server that
        // needs to keep serving.
        let Some(meta) = (if frame.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&frame[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = frame.len(),
                "rpc server fold: event payload too short for EventMeta; skipping",
            );
            return Ok(());
        };
        let key = (from_node, self.session_id, meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let mut payload =
                    match RpcRequestPayload::decode(frame.slice(RPC_FRAME_BODY_OFFSET..)) {
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
                            (self.emit)(
                                from_node,
                                self.session_id,
                                meta.origin_hash,
                                meta.seq_or_ts,
                                resp,
                            );
                            return Ok(());
                        }
                    };
                // E1.6: an admitted protected call STRIPS the raw
                // `net-org-admission` proof header(s) before the handler
                // sees the payload — application code receives verified
                // attribution via `RpcContext::org_admission`, never the
                // credential material. Public calls (`org_admission` None)
                // keep their headers untouched.
                if org_admission.is_some() {
                    payload.headers.retain(|(name, _)| {
                        name != crate::adapter::net::behavior::org_call::ORG_ADMISSION_HEADER
                    });
                }
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
                    (self.emit)(
                        from_node,
                        self.session_id,
                        meta.origin_hash,
                        meta.seq_or_ts,
                        resp,
                    );
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
                        (self.emit)(
                            from_node,
                            self.session_id,
                            meta.origin_hash,
                            meta.seq_or_ts,
                            resp,
                        );
                        return Ok(());
                    }
                }
                // §3/§2.4 (slice 1.4): split the verified attribution
                // (→ `RpcContext`) from the transfer's completion guard
                // (→ the spawned task's scope). The guard completes the
                // registry record whenever this opening stops — task end
                // or an effect-boundary refusal after transfer.
                let (org_admission, confirmed) = match org_admission {
                    Some(opening) => (Some(opening.admitted), opening.confirmed),
                    None => (None, None),
                };
                let cancellation = confirmed
                    .as_ref()
                    .map(|c| c.cancellation.clone())
                    .unwrap_or_else(RpcCancellationToken::new);
                self.in_flight.lock().insert(key, cancellation.clone());
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                // R2-A: the receiving incarnation rides into the
                // spawned handler task with the rest of the call's
                // identity.
                let session_id = self.session_id;
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
                // One task per call. The fold does not block on the
                // handler and does not order handlers against each
                // other — see `RpcHandler::call`'s ordering note.
                // Ordering is the transport's (a `Reliable` stream
                // delivers in sequence order) and the dispatch
                // loop's (one bridge task, one `apply_frame` per
                // frame, run to completion); what the scheduler
                // owns is when each handler body starts, which is
                // what `tokio::spawn` has always meant.
                //
                // An earlier round chained these tasks so that call
                // *n* could not be polled until call *n − 1* had
                // been polled once. That bought an ordering on
                // handler entry and cost more than it bought: a
                // ready `.await` does not yield, so one long first
                // poll held every successor from that source; the
                // wait on the predecessor raced neither
                // cancellation, nor the deadline, nor shutdown, so
                // a request CANCELled while queued still entered
                // its handler afterwards; and the map's sweep
                // constant bounded the map, not the queued tasks
                // holding request bytes behind a stalled
                // predecessor. Application-level ordering is a
                // policy an owner asks for, with its own queued
                // ownership and cancellation disposition — not a
                // side effect of a transport repair.
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
                        session_peer: from_node,
                        call_id,
                        payload,
                        cancellation,
                        trace_context,
                        org_admission,
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
                        // A registry-driven retirement (revocation /
                        // session replacement / shutdown) carries its
                        // typed reason into the terminal; a plain CANCEL
                        // keeps the documented Cancelled framing (the
                        // mapping is byte-identical for `Cancelled`).
                        let reason = confirmed
                            .as_ref()
                            .and_then(|c| c.call_ref.registry.terminal_reason(&c.call_ref.key))
                            .unwrap_or(StreamTerminalReason::Cancelled);
                        stream_terminal_payload(&reason)
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
                    emit(from_node, session_id, caller_origin, call_id, resp);
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

impl RedexFold<()> for RpcServerFold {
    /// Loopback / test shim: no session peer to resolve, so this
    /// drives `apply_frame` with `from_node = 0` (AV-1 item 1). The
    /// production wire path uses [`RpcServerFold::apply_inbound`].
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        self.apply_frame(0, &ev.payload, None)
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
    inner: tokio::sync::mpsc::Sender<ChargedChunk>,
    /// Optional metrics handle so a dropped-on-full chunk bumps the
    /// `streaming_chunks_dropped_total` counter. `None` for unit-
    /// test folds that construct without metrics.
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
    /// §2.2's producer-finished gate (protected calls only). Once the
    /// handler returns, sends are refused even from a clone the handler
    /// retained or handed to a detached task, so the drain completes on
    /// the already-admitted items instead of waiting out a stale
    /// producer. `None` on public calls: their deliberately lossy sink
    /// contract is unchanged (Q3).
    gate: Option<Arc<StreamProducerGate>>,
    /// §2.7 response-direction accounting (protected records only). When
    /// present, every item reserves its bytes before submission is
    /// acknowledged and a refused item LATCHES `ResourceExhausted` and
    /// retires the call — it is never merely dropped and counted. `None`
    /// on public calls: their deliberately lossy sink contract is
    /// unchanged (Q3).
    byte_charge: Option<RegistryCallRef>,
}

impl RpcResponseSink {
    /// Emit one non-terminal chunk. Cheap (`try_send` on a
    /// [`STREAMING_PUMP_CAPACITY`]-bounded mpsc); never blocks. On
    /// overflow OR receiver-closed, the chunk is dropped and (when
    /// metrics are wired) `streaming_chunks_dropped_total` is
    /// incremented for the service.
    ///
    /// For a PROTECTED record the §2.7 rule supersedes the lossy
    /// contract: a refused item latches `ResourceExhausted` and retires
    /// the call.
    pub fn send(&self, body: impl Into<bytes::Bytes>) {
        if self.gate.as_ref().is_some_and(|g| g.is_finished()) {
            return;
        }
        let body = body.into();
        let Some(charge) = self.byte_charge.as_ref() else {
            if self
                .inner
                .try_send(ChargedChunk { body, permit: None })
                .is_err()
            {
                if let Some(m) = self.metrics.as_ref() {
                    m.streaming_chunks_dropped_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            return;
        };
        self.send_charged(charge, body);
    }

    /// The protected (void) send: reserve bytes + queue capacity without
    /// ever parking; any refusal latches `ResourceExhausted` and retires
    /// (§2.7 — "it cannot drop an item and subsequently report complete
    /// success").
    fn send_charged(&self, charge: &RegistryCallRef, body: bytes::Bytes) {
        let len = body.len();
        if charge.registry.bytes().validate_item(len).is_err() {
            charge.latch_exhausted();
            return;
        }
        let permit = match charge.registry.bytes().reserve(
            charge.key.clone(),
            charge.incarnation,
            ByteDirection::Response,
            len,
        ) {
            Ok(permit) => permit,
            Err(_) => {
                // A full budget cannot be waited out from a void send —
                // the item is refused, so the call latches.
                charge.latch_exhausted();
                return;
            }
        };
        let Ok(slot) = self.inner.try_reserve() else {
            permit.release();
            charge.latch_exhausted();
            return;
        };
        match charge
            .registry
            .begin_commit(&charge.key, charge.incarnation)
        {
            Ok(txn) => {
                txn.commit_with(permit, |shared| {
                    slot.send(ChargedChunk {
                        body,
                        permit: Some(Arc::clone(shared)),
                    });
                });
            }
            Err(_) => {
                // Already terminal / gone: the item is refused by the
                // call's own disposition (first writer wins at `retire`).
                permit.release();
            }
        }
    }

    /// Emit one non-terminal chunk, **waiting** for pump-queue room
    /// instead of dropping on overflow. The backpressure-aware
    /// sibling of [`Self::send`]: when the per-call pump queue
    /// ([`STREAMING_PUMP_CAPACITY`]) is full — e.g. a flow-controlled
    /// caller has stopped granting credit — this parks until the
    /// pump drains a slot rather than silently discarding the chunk.
    ///
    /// Returns [`RpcSinkClosed`] when the pump receiver is gone (the
    /// call is torn down); the chunk was not sent and the handler
    /// should stop producing.
    ///
    /// Use this from handlers whose stream carries deltas that must
    /// not be lost silently (e.g. the `tool.watch` subscription,
    /// whose overflow contract requires an explicit resync frame
    /// instead of a drop); keep [`Self::send`] for streams where
    /// dropping under backpressure is acceptable.
    ///
    /// For a PROTECTED record the wait is bounded twice over (§2.7):
    /// `send_wait` waits only on satisfiable byte/queue bounds — an
    /// item that can never fit fails promptly, latching
    /// `ResourceExhausted` — and every wait is interruptible by the
    /// call's retirement, which wakes parked producers with
    /// [`RpcSinkClosed`].
    pub async fn send_wait(&self, body: impl Into<bytes::Bytes>) -> Result<(), RpcSinkClosed> {
        if self.gate.as_ref().is_some_and(|g| g.is_finished()) {
            return Err(RpcSinkClosed);
        }
        let body = body.into();
        let Some(charge) = self.byte_charge.as_ref() else {
            return self
                .inner
                .send(ChargedChunk { body, permit: None })
                .await
                .map_err(|_| RpcSinkClosed);
        };
        self.send_wait_charged(charge, body).await
    }

    /// The protected `send_wait`: reserve the item's bytes (parking only
    /// on satisfiable, releasable bounds), reserve a pump-queue slot, then
    /// perform the §2.3 check and the queue admission as ONE
    /// registry-locked ownership operation.
    async fn send_wait_charged(
        &self,
        charge: &RegistryCallRef,
        body: bytes::Bytes,
    ) -> Result<(), RpcSinkClosed> {
        let len = body.len();
        // §2.7: validate against the RPC item cap and the configured
        // per-call budget BEFORE waiting — an item larger than either can
        // never acquire enough capacity and must fail promptly.
        if charge.registry.bytes().validate_item(len).is_err() {
            charge.latch_exhausted();
            return Err(RpcSinkClosed);
        }
        loop {
            // Producer-gate closed mid-wait (§2.2): refused WITHOUT
            // latching — a retained clone cannot change the result the
            // call already holds.
            if self.gate.as_ref().is_some_and(|g| g.is_finished()) {
                return Err(RpcSinkClosed);
            }
            // 1 — byte reservation, call → caller → node.
            let permit = match charge.registry.bytes().reserve(
                charge.key.clone(),
                charge.incarnation,
                ByteDirection::Response,
                len,
            ) {
                Ok(permit) => permit,
                Err(refusal) if refusal.is_satisfiable_by_waiting() => {
                    // Park until a release makes room — or until the
                    // call retires (register-before-recheck, so a
                    // concurrent wake cannot be missed).
                    let notified = charge.registry.bytes().released.notified();
                    if charge.registry.terminal_reason(&charge.key).is_some() {
                        return Err(RpcSinkClosed);
                    }
                    notified.await;
                    continue;
                }
                Err(_) => {
                    charge.latch_exhausted();
                    return Err(RpcSinkClosed);
                }
            };
            // 2 — pump-queue capacity. A dead pump wakes this with a
            // closed receiver (retirement drops the receiver after
            // consuming the queue's permits).
            let slot = match self.inner.reserve().await {
                Ok(slot) => slot,
                Err(_) => {
                    permit.release();
                    return Err(RpcSinkClosed);
                }
            };
            // 3 — the §2.3 check and the queue admission as ONE
            // ownership operation: the registry lock is held across
            // `commit_with`'s admission, so a retirement cannot land
            // between the verdict and the enqueue.
            match charge
                .registry
                .begin_commit(&charge.key, charge.incarnation)
            {
                Ok(txn) => {
                    txn.commit_with(permit, |shared| {
                        slot.send(ChargedChunk {
                            body,
                            permit: Some(Arc::clone(shared)),
                        });
                    });
                    return Ok(());
                }
                Err(_) => {
                    // Retired / gone: roll the reservation back and let
                    // the call's own terminal stand (first writer wins).
                    permit.release();
                    return Err(RpcSinkClosed);
                }
            }
        }
    }
}

/// Error returned by [`RpcResponseSink::send_wait`] when the
/// per-call pump receiver has shut down (the streaming call is
/// being torn down server-side). The chunk was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcSinkClosed;

impl std::fmt::Display for RpcSinkClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rpc streaming sink: pump receiver closed; chunk not sent"
        )
    }
}

impl std::error::Error for RpcSinkClosed {}

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
///
/// **Ledger C1 (Q2, realized in Stage 1 slice 1.3):** `#[non_exhaustive]`
/// and carrying `org_admission`, mirroring [`RpcContext`]. External
/// struct-literal construction is a named source break; the stable
/// [`Self::new`] constructor (which creates **no** admitted org facts —
/// admission facts originate at the verifier) is the migration path.
#[non_exhaustive]
pub struct RpcStreamingContext {
    /// Caller's `origin_hash`, from the inbound packet header. Same
    /// source, and the same caveat, as [`RpcContext::caller_origin`]:
    /// **routing metadata, not identity authentication — do not
    /// authorize on this.**
    pub caller_origin: u64,
    /// Caller-generated correlation id. Matches the initial
    /// REQUEST's `call_id` and every subsequent REQUEST_CHUNK /
    /// CANCEL / REQUEST_GRANT for this call.
    pub call_id: u64,
    /// Absolute deadline (unix nanos) from the initial REQUEST.
    /// `0` means no deadline on PUBLIC calls (unchanged). For an
    /// admitted PROTECTED call the fold always holds a finite
    /// effective deadline (§2.1: the provider default fills an
    /// omitted one), the record's supervisor enforces it, and expiry
    /// surfaces as a typed `Timeout` / `AdmissionDenied(Denied)`
    /// terminal. Public `deadline_ns != 0` is now enforced too (C6):
    /// the fold stops the handler at the deadline and emits
    /// `RpcStatus::Timeout` (C7).
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
    /// OA-2 org-admission attribution (C1/Q2). `Some(Admitted)` for a
    /// call that passed the PROTECTED-service admission gate — the
    /// four-party verified identity. The raw `net-org-admission` proof
    /// header is STRIPPED from `headers` before the handler sees it, so
    /// application code receives verified attribution, never raw
    /// credential material. `None` for public calls, and always `None`
    /// from [`Self::new`] — admission facts originate at the verifier.
    pub org_admission: Option<crate::adapter::net::behavior::org_admission::Admitted>,
}

impl RpcStreamingContext {
    /// The stable constructor (C1/Q2's migration path). Creates **no**
    /// admitted org facts: `org_admission` is always `None` here, so a
    /// fixture or public call can build a context without fabricating
    /// verified attribution. Admission facts originate at the verifier
    /// and are placed on the context by the protected fold's admitted
    /// entry point.
    pub fn new(
        caller_origin: u64,
        call_id: u64,
        deadline_ns: u64,
        headers: Vec<RpcHeader>,
        cancellation: RpcCancellationToken,
        trace_context: Option<TraceContext>,
    ) -> Self {
        Self {
            caller_origin,
            call_id,
            deadline_ns,
            headers,
            cancellation,
            trace_context,
            org_admission: None,
        }
    }
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
/// Arguments: `(from_node, caller_origin, call_id, credits)`. R3-1 —
/// the AEAD-authenticated `from_node` is carried so an upload grant is
/// session-scoped: coalesced and published per `(from_node, origin,
/// call_id)`, and (for a trusted direct route) delivered ONLY to the
/// session that issued the call, never roster-fanned to a same-origin
/// sibling whose request semaphore it would otherwise wrongly refill.
pub type RpcRequestGrantEmitter = Arc<dyn Fn(u64, u64, u64, u32) + Send + Sync + 'static>;

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
    inner: tokio::sync::mpsc::Receiver<ChargedChunk>,
    grant_emitter: Option<RpcRequestGrantEmitter>,
    /// AEAD-authenticated session that issued this call (R3-1) — the
    /// grant identity, so a grant refills only THIS call's semaphore.
    from_node: u64,
    caller_origin: u64,
    call_id: u64,
}

impl RequestStream {
    /// Visible to the fold (and only the fold) for constructing
    /// a stream tied to a specific receiver + caller. The
    /// `grant_emitter` is `None` when the caller didn't opt into
    /// flow control; `Some(...)` when they did. `from_node` is the
    /// authenticated session the fold bound the call to (R3-1).
    pub(crate) fn new(
        inner: tokio::sync::mpsc::Receiver<ChargedChunk>,
        grant_emitter: Option<RpcRequestGrantEmitter>,
        from_node: u64,
        caller_origin: u64,
        call_id: u64,
    ) -> Self {
        Self {
            inner,
            grant_emitter,
            from_node,
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
            std::task::Poll::Ready(Some(chunk)) => {
                // §2.7: the item's byte reservation releases when the
                // handler's `RequestStream` yields the chunk.
                release_chunk_permit(&chunk);
                // Auto-grant fires on every successful pull when
                // flow control was opted into. Cheap and
                // fire-and-forget; missed grants are recovered
                // by subsequent pulls.
                if let Some(emit) = self.grant_emitter.as_ref() {
                    emit(self.from_node, self.caller_origin, self.call_id, 1);
                }
                std::task::Poll::Ready(Some(chunk.body))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
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
/// `(from_node, receiving_session_id, caller_origin_hash, call_id)`
/// (AV-1 item 1, C5); value is a tokio `Semaphore` shared between the
/// pump task (which awaits permits) and the fold's `apply()` method
/// handling STREAM_GRANT events (which add permits). The authenticated
/// last-hop session peer AND the receiving incarnation are part of the
/// key so a peer cannot refill another peer's flow-control window by
/// copying its origin + call_id onto a forged STREAM_GRANT — and a
/// grant from a replaced session cannot credit the successor.
type FlowControlMap = Arc<Mutex<HashMap<StreamCallKey, Arc<tokio::sync::Semaphore>>>>;

// ============================================================================
// §2.1 / §2.2 / §2.6 — protected streaming call lifetime and bounded
// supervision (Stage 1 slice 1.3).
//
// The executable contract is the Stage 0 model in
// `behavior/org_stream_lifecycle.rs` (`resolve_deadline`,
// `LifetimePolicy`, `CallLifecycle`, `run_supervisor`, `ProducerGate`,
// `sink_send`, `TerminalReason`, `TerminalDisposition`). That model is
// `#[cfg(test)]` and STAYS there as the adversarially-scheduled witness
// of these semantics; this block is its production implementation,
// wired into `RpcServerStreamingFold` for PROTECTED records. Where the
// two must agree, the model is the specification and comments here cite
// its sections.
// ============================================================================

/// Provider lifetime policy (Owner Q1 defaults: 300 s / 3600 s). Wall-
/// clock nanosecond durations, so it composes with
/// [`ClockSample::wall_ns`](crate::adapter::net::behavior::admission_clock::ClockSample::wall_ns)
/// without a second unit conversion at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLifetimePolicy {
    /// Used ONLY when the caller supplied no deadline (§2.1 bound 1).
    pub default_live_ns: u64,
    /// Ceiling on an explicitly requested deadline. Exceeding it REFUSES
    /// the opening (§2.1 bound 2: refused, never clamped).
    pub max_live_ns: u64,
}

impl StreamLifetimePolicy {
    /// The Q1 initial defaults: 300 s default, 3600 s maximum.
    pub const fn q1_defaults() -> Self {
        Self {
            default_live_ns: 300 * 1_000_000_000,
            max_live_ns: 3_600 * 1_000_000_000,
        }
    }

    /// Startup validation (Q1): both positive, and the default can never
    /// trip the cap (a configuration bug would otherwise refuse every
    /// call that omitted a deadline).
    pub fn validate(&self) -> Result<(), StreamPolicyError> {
        if self.default_live_ns == 0 || self.max_live_ns == 0 {
            return Err(StreamPolicyError::NotPositive);
        }
        if self.default_live_ns > self.max_live_ns {
            return Err(StreamPolicyError::DefaultOverMax);
        }
        Ok(())
    }
}

/// Why a [`StreamLifetimePolicy`] is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPolicyError {
    /// A zero duration: no call could ever run.
    NotPositive,
    /// `default_live > max_live`: the default itself would be refused.
    DefaultOverMax,
}

/// Which bound produced the effective end (§2.1) — it selects the
/// terminal reason, so it is not a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDeadlineBound {
    /// The caller's deadline, or the provider default for an omitted
    /// one. Expiry is an ordinary [`StreamTerminalReason::Timeout`].
    Deadline,
    /// Credential validity cut the call short (§2.1 bound 3). Expiry is
    /// an authority lapse, not a timeout: `AdmissionDenied(Denied)`.
    Credential,
}

/// The single monotonic-translatable end of a protected call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedStreamDeadline {
    /// Absolute wall-clock end, unix nanoseconds.
    pub end_ns: u64,
    /// Which of the three bounds won.
    pub bound: StreamDeadlineBound,
}

impl ResolvedStreamDeadline {
    /// The terminal this deadline produces when it fires (§2.1):
    /// `Timeout` vs `CredentialExpired` — the two are never conflated.
    pub fn expiry_reason(&self) -> StreamTerminalReason {
        match self.bound {
            StreamDeadlineBound::Deadline => StreamTerminalReason::Timeout,
            StreamDeadlineBound::Credential => StreamTerminalReason::CredentialExpired,
        }
    }
}

/// Why an opening is refused before any handler effect (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDeadlineRefusal {
    /// An explicit caller deadline beyond `max_live`. Refused, never
    /// clamped: the caller asked for something the provider does not
    /// offer and must learn that, not silently get five minutes.
    ExceedsPolicy,
    /// The effective end is already in the past (an explicit past
    /// deadline, or a credential clamp that lands behind `now`).
    AlreadyElapsed,
    /// Checked arithmetic overflowed (a pre-epoch or absurd clock).
    Overflow,
}

/// §2.1's effective deadline — three distinct bounds, never one `min`:
///
/// 1. `requested` is `None` when the caller omitted a deadline
///    (`deadline_ns == 0` on the wire); the provider default is reached
///    ONLY through that arm, so it can never cap an explicit request.
/// 2. An explicit request over `now + max_live` is REFUSED, never
///    clamped.
/// 3. Credential validity CLAMPS and records that it clamped; on an
///    exact tie credential expiry wins (at that instant the authority is
///    gone, and a plain timeout would understate it).
///
/// `credential_ends_ns` carries every applicable validity end in
/// nanoseconds — caller membership, dispatcher grant, the optional
/// capability grant, AND the provider's own authority validity; `None`
/// entries contribute no bound.
pub fn resolve_stream_deadline(
    now_ns: u64,
    requested: Option<u64>,
    credential_ends_ns: &[Option<u64>],
    policy: &StreamLifetimePolicy,
) -> Result<ResolvedStreamDeadline, StreamDeadlineRefusal> {
    let (requested_end, mut bound) = match requested {
        Some(end) => {
            let cap = now_ns
                .checked_add(policy.max_live_ns)
                .ok_or(StreamDeadlineRefusal::Overflow)?;
            if end > cap {
                return Err(StreamDeadlineRefusal::ExceedsPolicy);
            }
            (end, StreamDeadlineBound::Deadline)
        }
        None => (
            now_ns
                .checked_add(policy.default_live_ns)
                .ok_or(StreamDeadlineRefusal::Overflow)?,
            StreamDeadlineBound::Deadline,
        ),
    };

    let mut end_ns = requested_end;
    for candidate in credential_ends_ns.iter().flatten() {
        if *candidate <= end_ns {
            end_ns = *candidate;
            bound = StreamDeadlineBound::Credential;
        }
    }

    if end_ns <= now_ns {
        return Err(StreamDeadlineRefusal::AlreadyElapsed);
    }
    Ok(ResolvedStreamDeadline { end_ns, bound })
}

/// The lifetime inputs one protected opening resolves its §2.1 deadline
/// against — the provider policy (Q1 defaults unless configured
/// otherwise), every applicable credential validity end, and the ONE
/// clock sample of the admission
/// ([`ClockSample`](crate::adapter::net::behavior::admission_clock::ClockSample),
/// per E0.4: freshness and deadline translation read the same instant).
pub struct StreamCallLifetime<'a> {
    /// The provider's lifetime policy.
    pub policy: StreamLifetimePolicy,
    /// Every applicable validity end in unix nanoseconds; `None`
    /// entries contribute no bound (see [`resolve_stream_deadline`]).
    pub credential_ends_ns: &'a [Option<u64>],
    /// The admission's paired clock sample.
    pub clock: crate::adapter::net::behavior::admission_clock::ClockSample,
}

/// What the handler returned (§2.6). Preserved verbatim so an error can
/// never be reported as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamHandlerResult {
    /// Clean return.
    Ok,
    /// Typed failure: the exact terminal status + body the handler's
    /// error maps to (`Application(code)` / `Internal`), preserved
    /// through the drain.
    Err(RpcStatus, String),
}

/// The input (caller → provider) half of a call (§2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamCallInput {
    /// Accepting request chunks.
    Open,
    /// The caller sent END. Legitimate remaining output is unaffected.
    Ended,
    /// The *consumer* is gone (the handler returned). Further chunks are
    /// refused and discarded.
    Closed,
}

/// The output (provider → caller) half (§2.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamCallOutput {
    /// The handler may still produce.
    Open,
    /// The handler returned and its result is held here while the pump
    /// drains already-queued items. **Not terminal** — producer finished
    /// is not terminal.
    Draining(StreamHandlerResult),
    /// The pump has stopped; nothing further can be published.
    Ended,
}

/// The single terminal disposition of a call (§2.6). First writer wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTerminalReason {
    /// The handler finished and the pump drained. Carries the handler's
    /// own result: an `Err` handler yields `Completed(Err(..))`, never
    /// `Ok`.
    Completed(StreamHandlerResult),
    /// Caller CANCEL, or the caller handle dropped.
    Cancelled,
    /// The effective deadline fired under
    /// [`StreamDeadlineBound::Deadline`].
    Timeout,
    /// The effective deadline fired under
    /// [`StreamDeadlineBound::Credential`] — authority lapsed rather
    /// than time running out.
    CredentialExpired,
    /// A revocation floor rose past this call's member generation.
    Revoked,
    /// The authority or revocation store moved, was removed, or is
    /// poisoned: fail closed.
    AuthorityUnavailable,
    /// An admitted item could neither be reserved nor delivered. The
    /// call dies; it never completes `Ok` having silently dropped
    /// input.
    ResourceExhausted,
    /// The peer's session was replaced or the peer disconnected.
    SessionReplaced,
    /// The registration's `ServeHandle` dropped, or the node shut down.
    ServeHandleDropped,
    /// The pump stopped without the handler returning — a typed failure,
    /// never successful completion.
    PumpFailed,
}

impl StreamTerminalReason {
    /// Whether already-queued response items are published before the
    /// terminal (§2.2's queued-data table). Only a genuine completion
    /// drains; every retirement discards.
    pub fn drains_queued_output(&self) -> bool {
        matches!(self, StreamTerminalReason::Completed(_))
    }
}

/// What the control path actually did with the terminal (§2.8). The
/// records are distinct because an attempted send is **not** peer
/// receipt. At this layer the control path is the response-emitter
/// seam; `Queued`/`Refused`/`Unreachable` distinctions inside the
/// bounded response drainer and the route layer become separately
/// observable at the `RpcResponseJob` seam (slice 1.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamTerminalDisposition {
    /// The control queue accepted the terminal. Not peer receipt.
    Queued,
    /// The transport seam accepted the terminal job (recorded at the
    /// send seam — the emit future completed). Still not endpoint
    /// receipt, which is attributed at the peer.
    Sent,
    /// The session or route is gone. The peer will observe interruption
    /// or its own deadline.
    Unreachable,
    /// The control queue refused the terminal. Recorded as interruption;
    /// ownership is still released.
    Refused,
}

/// The committed terminal plus its one-shot emission record (§2.8).
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamTerminal {
    /// The selected outcome. First writer wins.
    reason: StreamTerminalReason,
    /// The control-path disposition, recorded exactly once by the
    /// supervisor after the pump stopped.
    emission: Option<StreamTerminalDisposition>,
}
/// One call's lifecycle state (§2.6) — the production mirror of the
/// model's `CallLifecycle`. The input half starts `Ended` for
/// server-streaming (the model's `CallLifecycle::new`: the single
/// request arrived with the opening) and carries the §2.6 rules so the
/// client-streaming/duplex records of Stage 2 extend the same machine.
#[derive(Debug)]
struct StreamCallRecord {
    input: StreamCallInput,
    output: StreamCallOutput,
    terminal: Option<StreamTerminal>,
}

impl StreamCallRecord {
    fn new_server_streaming() -> Self {
        Self {
            input: StreamCallInput::Ended,
            output: StreamCallOutput::Open,
            terminal: None,
        }
    }

    /// §2.6's initial state for a CLIENT-STREAMING record: both halves
    /// `Open` (the upload is still arriving; the single response has not
    /// been produced). Stage 2 slice 2.2.
    fn new_client_streaming() -> Self {
        Self {
            input: StreamCallInput::Open,
            output: StreamCallOutput::Open,
            terminal: None,
        }
    }

    /// §2.6's initial state for a DUPLEX record: both halves `Open`.
    /// Stage 2 slice 2.2.
    fn new_duplex() -> Self {
        Self {
            input: StreamCallInput::Open,
            output: StreamCallOutput::Open,
            terminal: None,
        }
    }

    /// END from the caller ⇒ `input = Ended` ONCE, idempotent, never
    /// touching `output` (§2.6 — half-close independence protects
    /// legitimate remaining output from an early END). Returns `true`
    /// only for the transition that closed the half: a second END, an
    /// END after the handler returned (`Closed`), or an END on a
    /// server-streaming record (`Ended` at birth) all return `false` and
    /// change NOTHING — an END can never reopen a closed/terminal half.
    /// Stage 2 slice 2.4.
    fn end_input(&mut self) -> bool {
        if self.input == StreamCallInput::Open {
            self.input = StreamCallInput::Ended;
            true
        } else {
            false
        }
    }

    /// Frames while terminal are dropped, no credit, no delivery (§2.6).
    /// Credit survives the handler's return — it is what lets a drain
    /// finish — and stops once the output half has ended.
    fn credit_grantable(&self) -> bool {
        self.terminal.is_none()
            && matches!(
                self.output,
                StreamCallOutput::Open | StreamCallOutput::Draining(_)
            )
    }

    /// The handler returned (§2.6): output enters `Draining` with the
    /// result preserved, and an open input half becomes `Closed` — its
    /// consumer is gone. Returns `false` once terminal or already
    /// draining.
    fn handler_returned(&mut self, result: StreamHandlerResult) -> bool {
        if self.terminal.is_some() || !matches!(self.output, StreamCallOutput::Open) {
            return false;
        }
        self.output = StreamCallOutput::Draining(result);
        if self.input == StreamCallInput::Open {
            self.input = StreamCallInput::Closed;
        }
        true
    }

    /// The pump stopped (§2.6). While `Draining` this commits
    /// `Completed(result)`; while still `Open` the producer died under
    /// the handler — a typed failure, never a success. Returns the
    /// committed reason exactly once.
    fn pump_exited(&mut self) -> Option<StreamTerminalReason> {
        if self.terminal.is_some() {
            return None;
        }
        let reason = match std::mem::replace(&mut self.output, StreamCallOutput::Ended) {
            StreamCallOutput::Draining(result) => StreamTerminalReason::Completed(result),
            StreamCallOutput::Open => StreamTerminalReason::PumpFailed,
            StreamCallOutput::Ended => return None,
        };
        self.terminal = Some(StreamTerminal {
            reason: reason.clone(),
            emission: None,
        });
        Some(reason)
    }

    /// Retire from ANY state, including `Draining`. First writer wins;
    /// later END, handler return or pump exit are no-ops (§2.6).
    fn retire(&mut self, reason: StreamTerminalReason) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(StreamTerminal {
            reason,
            emission: None,
        });
        true
    }

    /// Record the terminal's control-path disposition (§2.8). Returns
    /// `true` exactly once, for the supervisor that owns the emission.
    fn record_emission(&mut self, disposition: StreamTerminalDisposition) -> bool {
        match self.terminal.as_mut() {
            Some(terminal) if terminal.emission.is_none() => {
                terminal.emission = Some(disposition);
                true
            }
            _ => false,
        }
    }

    fn terminal_reason(&self) -> Option<StreamTerminalReason> {
        self.terminal.as_ref().map(|t| t.reason.clone())
    }

    fn emission(&self) -> Option<StreamTerminalDisposition> {
        self.terminal.as_ref().and_then(|t| t.emission)
    }
}

/// Retirement signal shared by the CANCEL arm, `ServeHandle::drop`, and
/// (once wired) the revocation callback, session sweep and node
/// shutdown (§2.2) — the production mirror of the model's
/// `RetireSignal`. First reason wins, matching
/// `StreamCallRecord::retire`.
#[derive(Debug, Default)]
pub struct StreamRetireSignal {
    notify: Notify,
    reason: Mutex<Option<StreamTerminalReason>>,
}

impl StreamRetireSignal {
    /// A fresh, unsignalled handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Signal retirement. First reason wins; every signal is idempotent
    /// afterwards.
    pub fn fire(&self, reason: StreamTerminalReason) {
        let mut slot = self.reason.lock();
        if slot.is_none() {
            *slot = Some(reason);
        }
        drop(slot);
        self.notify.notify_waiters();
    }

    fn taken(&self) -> Option<StreamTerminalReason> {
        self.reason.lock().clone()
    }

    /// Wait for a retirement signal (register-before-recheck, so a
    /// `fire` between the check and the await cannot be missed).
    async fn wait(&self) -> StreamTerminalReason {
        loop {
            let notified = self.notify.notified();
            if let Some(reason) = self.taken() {
                return reason;
            }
            notified.await;
        }
    }
}

/// The producer-finished gate (§2.2) — the production mirror of the
/// model's `ProducerGate`. Once the handler returns, the call's sink is
/// logically closed: new sends are refused even from a clone the handler
/// retained or handed to a detached task, and the pump drains only what
/// was already admitted. Without it a retained clone keeps the queue
/// open and the drain never completes.
#[derive(Debug, Default)]
pub struct StreamProducerGate {
    finished: AtomicBool,
    woken: Notify,
}

impl StreamProducerGate {
    /// A gate that is still admitting.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the producer half is closed.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// Close the producer half and wake a pump parked on `recv`.
    pub fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        self.woken.notify_waiters();
    }
}

/// One PROTECTED streaming call's supervisor-owned state — the §2.2
/// ownership handle. Retirement reaches the supervisor through
/// [`Self::retire`] even before its task has been scheduled; the
/// lifecycle record answers the observation questions a witness (or the
/// 1.4 registry transfer) needs.
#[derive(Debug)]
pub struct ProtectedStreamCall {
    record: Arc<Mutex<StreamCallRecord>>,
    retire: Arc<StreamRetireSignal>,
    deadline: ResolvedStreamDeadline,
    /// §2.4 registry linkage (slice 1.4). `Some` for registry-backed
    /// records: [`Self::retire`] goes through the registry so the record's
    /// terminal, its queued byte permits and the owner signal are settled
    /// synchronously in one first-writer-wins operation.
    call_ref: Option<RegistryCallRef>,
}

impl ProtectedStreamCall {
    /// The resolved §2.1 effective end (wall-clock unix ns).
    pub fn deadline_end_ns(&self) -> u64 {
        self.deadline.end_ns
    }

    /// Which bound produced [`Self::deadline_end_ns`] — it selects the
    /// expiry terminal (`Timeout` vs `CredentialExpired`).
    pub fn deadline_bound(&self) -> StreamDeadlineBound {
        self.deadline.bound
    }

    /// Whether the call still owns live work (not terminal).
    pub fn is_live(&self) -> bool {
        self.record.lock().terminal.is_none()
    }

    /// The committed terminal, once one exists.
    pub fn terminal(&self) -> Option<StreamTerminalReason> {
        self.record.lock().terminal_reason()
    }

    /// Test-only: the §2.6 input half's state — the half-close witnesses'
    /// "END closes input once / an END can never reopen it" observation
    /// (Stage 2 slices 2.2/2.4).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn input_half(&self) -> StreamCallInput {
        self.record.lock().input
    }

    /// The retirement reason the owner has ALREADY been handed (§2.4:
    /// "retirement reached the owner"), if any — observable the instant
    /// the synchronous retirement lands, before the supervisor's async
    /// cleanup commits the terminal.
    pub fn retire_reason(&self) -> Option<StreamTerminalReason> {
        self.retire.taken()
    }

    /// The control-path disposition recorded for the terminal (§2.8),
    /// exactly once.
    pub fn emission(&self) -> Option<StreamTerminalDisposition> {
        self.record.lock().emission()
    }

    /// Retire this exact call (§2.2/§2.4). Idempotent: the first reason
    /// wins at the supervisor. Registry-backed records settle the
    /// registry side (terminal + byte permits) first, so no item can
    /// commit after the retirement lands.
    pub fn retire(&self, reason: StreamTerminalReason) {
        if let Some(call_ref) = self.call_ref.as_ref() {
            call_ref
                .registry
                .retire(&call_ref.key, call_ref.incarnation, reason);
            return;
        }
        self.retire.fire(reason);
    }
}

/// The registration-owned set of live PROTECTED stream calls (§2.2's
/// "every record of the registration"). `ServeHandle::drop` fires
/// `ServeHandleDropped` on every record here (Q3: protected-only on
/// handle drop — PUBLIC calls never enter this set and keep their
/// documented outstanding-call behavior); node shutdown is meant to
/// fire it node-wide (see the 1.3 report — the shutdown hook is not
/// reachable from `mesh_rpc.rs`).
#[derive(Debug, Clone, Default)]
pub struct ProtectedStreamOwners {
    calls: Arc<Mutex<HashMap<StreamCallKey, Arc<ProtectedStreamCall>>>>,
}

impl ProtectedStreamOwners {
    /// Empty ownership for one registration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retire every live record with `reason` (the supervisor selects
    /// each terminal; already-terminal records ignore the signal). All
    /// retirement is asynchronous by nature — §2.2: "async cleanup is
    /// not guaranteed to finish before the synchronous revocation
    /// callback returns". Returns how many signals were fired.
    pub fn retire_all(&self, reason: StreamTerminalReason) -> usize {
        let calls: Vec<Arc<ProtectedStreamCall>> = self.calls.lock().values().cloned().collect();
        let count = calls.len();
        for call in calls {
            call.retire(reason.clone());
        }
        count
    }

    fn insert(&self, key: StreamCallKey, call: Arc<ProtectedStreamCall>) {
        self.calls.lock().insert(key, call);
    }

    fn remove(&self, key: &StreamCallKey) {
        self.calls.lock().remove(key);
    }

    /// The live record for `key`, if any.
    pub fn get(&self, key: &StreamCallKey) -> Option<Arc<ProtectedStreamCall>> {
        self.calls.lock().get(key).cloned()
    }

    /// How many records this registration currently tracks.
    pub fn len(&self) -> usize {
        self.calls.lock().len()
    }

    /// Whether this registration tracks no records.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Test-only: the tracked key set.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn keys(&self) -> Vec<StreamCallKey> {
        self.calls.lock().keys().copied().collect()
    }
}

/// The fold-owned per-call state the supervisor removes at completion —
/// the 1.3 form of §2.4's single removal point (the exact-incarnation
/// registry of contract 3 lands in slice 1.4; the four-part key does the
/// session fencing here).
struct StreamCallRegistration {
    key: StreamCallKey,
    in_flight: InFlightCalls,
    /// `None` on the client-streaming fold: its shape has no response pump
    /// to credit (a `STREAM_GRANT` there is direction-wrong and no-ops).
    flow_control: Option<FlowControlMap>,
    protected: ProtectedStreamOwners,
    /// §2.4's single removal point (the model's `complete`): registry-
    /// backed records are removed from the registry here too, after the
    /// supervisor has disposed of its owned work.
    registry: Option<RegistryCallRef>,
    /// The CS/DX request-chunk sender map (Stage 2 slices 2.2/2.4):
    /// §2.2's "close input admission … through the queue owner" and
    /// §2.4's single removal both cover it, so a retired call's sender is
    /// gone at the same boundary as every other per-call map. `None` on
    /// the server-streaming fold (no request direction).
    senders: Option<RequestChunkSenders>,
}

impl StreamCallRegistration {
    fn complete(&self) {
        self.in_flight.lock().remove(&self.key);
        if let Some(flow_control) = self.flow_control.as_ref() {
            flow_control.lock().remove(&self.key);
        }
        self.protected.remove(&self.key);
        if let Some(senders) = self.senders.as_ref() {
            senders.lock().remove(&self.key);
        }
        if let Some(call_ref) = self.registry.as_ref() {
            call_ref
                .registry
                .complete(&call_ref.key, call_ref.incarnation);
        }
    }
}

/// Whether a REQUEST's flags claim exactly the server-streaming shape
/// (contract 4's SS flag check, unified with the CS/DX arms): the
/// streaming-response flag set and the client-streaming flag clear —
/// i.e. [`RpcCallShape::from_streaming_flags`](crate::adapter::net::behavior::org_call::RpcCallShape::from_streaming_flags)
/// derives `ServerStreaming` from them.
fn ss_request_flags_ok(flags: u16) -> bool {
    flags & FLAG_RPC_STREAMING_RESPONSE != 0 && flags & FLAG_RPC_CLIENT_STREAMING_REQUEST == 0
}

/// Whether a REQUEST's flags claim exactly the CLIENT-STREAMING shape
/// (Stage 2 slice 2.2 — the CS twin of [`ss_request_flags_ok`]): the
/// client-streaming flag set and the streaming-response flag clear — i.e.
/// `RpcCallShape::from_streaming_flags` derives `ClientStreaming`.
fn cs_request_flags_ok(flags: u16) -> bool {
    flags & FLAG_RPC_CLIENT_STREAMING_REQUEST != 0 && flags & FLAG_RPC_STREAMING_RESPONSE == 0
}

/// Whether a REQUEST's flags claim exactly the DUPLEX shape (Stage 2
/// slice 2.2): BOTH streaming flags set — `from_streaming_flags` derives
/// `Duplex`.
fn dx_request_flags_ok(flags: u16) -> bool {
    flags & FLAG_RPC_CLIENT_STREAMING_REQUEST != 0 && flags & FLAG_RPC_STREAMING_RESPONSE != 0
}

/// The terminal frame one selected reason emits (§2.2's queued-data
/// table + §2.1's `Timeout` vs `CredentialExpired` split + §2.8's "no
/// synthetic success"). `Completed` carries the handler's own result
/// verbatim; every retirement maps onto the frozen wire vocabulary —
/// `Timeout` (C7), `Cancelled` (incl. `ServeHandleDropped` /
/// `SessionReplaced`, per §2.2's table), `AdmissionDenied` + coarse
/// byte (`CredentialExpired` / `Revoked` / `AuthorityUnavailable` →
/// `Denied`, `ResourceExhausted` → `Unavailable`), `Internal` for a
/// failed pump.
fn stream_terminal_payload(reason: &StreamTerminalReason) -> RpcResponsePayload {
    match reason {
        StreamTerminalReason::Completed(StreamHandlerResult::Ok) => RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![(
                HEADER_NRPC_STREAMING.to_string(),
                HEADER_NRPC_STREAMING_END.to_vec(),
            )],
            body: Bytes::new(),
        },
        StreamTerminalReason::Completed(StreamHandlerResult::Err(status, message)) => {
            RpcResponsePayload {
                status: *status,
                headers: vec![],
                body: Bytes::from(message.clone()),
            }
        }
        StreamTerminalReason::Cancelled | StreamTerminalReason::ServeHandleDropped => {
            RpcResponsePayload {
                status: RpcStatus::Cancelled,
                headers: vec![],
                body: Bytes::from_static(
                    b"server observed CANCEL during streaming handler execution",
                ),
            }
        }
        StreamTerminalReason::SessionReplaced => RpcResponsePayload {
            status: RpcStatus::Cancelled,
            headers: vec![],
            body: Bytes::from_static(b"peer session replaced"),
        },
        StreamTerminalReason::Timeout => RpcResponsePayload {
            status: RpcStatus::Timeout,
            headers: vec![],
            body: Bytes::from_static(b"stream deadline_ns exceeded"),
        },
        StreamTerminalReason::CredentialExpired => RpcResponsePayload {
            status: RpcStatus::AdmissionDenied,
            headers: vec![],
            body: Bytes::from_static(&[0]), // coarse `Denied` — authority lapsed
        },
        StreamTerminalReason::Revoked | StreamTerminalReason::AuthorityUnavailable => {
            RpcResponsePayload {
                status: RpcStatus::AdmissionDenied,
                headers: vec![],
                body: Bytes::from_static(&[0]), // coarse `Denied`
            }
        }
        StreamTerminalReason::ResourceExhausted => RpcResponsePayload {
            status: RpcStatus::AdmissionDenied,
            headers: vec![],
            body: Bytes::from_static(&[2]), // coarse `Unavailable`
        },
        StreamTerminalReason::PumpFailed => RpcResponsePayload {
            status: RpcStatus::Internal,
            headers: vec![],
            body: Bytes::from_static(b"response pump failed"),
        },
    }
}

// ============================================================================
// Slice 1.4 — the §3 exact-incarnation protected-call registry, the §2.3
// revocation requalification, and the §2.7 byte accounting.
//
// This is the PRODUCTION mirror of the Stage 0 model
// `behavior/org_stream_registry.rs` (whose surface is frozen): the same
// reserve → verify → rollback → install → transfer → complete transaction,
// the same StampMovement-aware commit-point requalification, the same
// call → caller → node byte reservation order with release-once permits.
// Deviations from the model's abstract types are named inline:
//
// - the model's `SessionRef.establishment: u64` is the exact handshake that
//   produced the session; production carries it whole
//   (`NetSession::handshake_binding`, the full Noise transcript hash) in
//   `SessionIdentity::establishment` — strictly more precise than a u64, and
//   matching is still the exact triple (bare truncated ids never match);
// - the model's `Denial` vocabulary collapses onto the C4 `AdmissionDenied`
//   set (the report's finding F-S1.4-1 names every mapping);
// - the model's `SupervisorOwner` is realized as the call's
//   `StreamRetireSignal` plus an optional `on_retire` hook (the unary fold's
//   cancellation), so retirement reaches the owner before its task runs.
//
// The registry is one per `MeshNode`, resolved by node id (mesh.rs cannot
// hold it — its five authorized hook sites are call lines only). It holds
// its own `RaiseSubscription` (§2.3's second `subscribe_floors_raised`
// subscriber) and is bound to the installed `(NodeAuthority,
// OrgRevocationStore)` pair by the store-install hook; records captured
// under a replaced pair are retired before the install returns.
// ============================================================================

use dashmap::DashMap;
use parking_lot::MutexGuard;

use crate::adapter::net::behavior::org::OrgId;
use crate::adapter::net::behavior::org_admission::AdmissionDenied;
use crate::adapter::net::behavior::org_authority::NodeAuthority;
use crate::adapter::net::behavior::org_call::RpcCallShape;
use crate::adapter::net::behavior::org_revocation::{
    OrgRevocationState, OrgRevocationStore, RaiseSubscription, RaisedFloor,
};
use crate::adapter::net::identity::EntityId;
use crate::adapter::net::org_admission_gate::AdmissionStamp;

/// The nRPC correlation identity a protected admission is keyed on — the
/// replay guard's `(caller, call_id)` (§3), where `caller` is the
/// TOFU-authenticated direct-session entity, never a request field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProtectedCallKey {
    /// The authenticated caller entity.
    pub caller: EntityId,
    /// `EventMeta::seq_or_ts`.
    pub call_id: u64,
}

/// The exact originating session (§3 step 4) — the production form of the
/// model's `SessionRef`. `establishment` is the full Noise handshake hash
/// the session was established with (`NetSession::handshake_binding`);
/// `None` for hand-built sessions (which can never admit a protected call).
/// Retirement matches the EXACT triple: the truncated wire id alone is
/// shared across unrelated peers and must never retire a bystander.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    /// The authenticated peer node.
    pub peer: u64,
    /// The truncated 8-byte wire session id.
    pub session_id: u64,
    /// The exact handshake that produced this session.
    pub establishment: Option<[u8; 32]>,
}

/// How a captured [`AdmissionStamp`] relates to the live one — the §2.3
/// discriminator (the model's `AuthorityStamp::movement`). The unary gate
/// only needs `is_current` because a stale view means "do not admit"; a
/// LIVE call must distinguish "the store moved (fail closed)" from "a floor
/// was published (requalify)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMovement {
    /// Same store, same generation: proceed.
    Unchanged,
    /// Same store, a floor was published: requalify against the floors,
    /// refreshing the captured generation on success.
    GenerationOnly,
    /// The authority/store moved, the store is poisoned, or a generation is
    /// exhausted: fail closed — there is nothing left to requalify against.
    Unusable,
}

/// Classify the movement between a record's captured security view and the
/// live one. Exactly the model's `movement()`: whole-stamp identity for the
/// fast path, with the poison/exhaustion failures falling to `Unusable`.
pub fn stamp_movement(captured: &AdmissionStamp, live: &AdmissionStamp) -> ViewMovement {
    if captured.store_generation.is_none()
        || live.store_generation.is_none()
        || captured.poisoned
        || live.poisoned
    {
        return ViewMovement::Unusable;
    }
    if captured.authority_ptr != live.authority_ptr || captured.store_ptr != live.store_ptr {
        return ViewMovement::Unusable;
    }
    if captured.store_generation == live.store_generation {
        ViewMovement::Unchanged
    } else {
        ViewMovement::GenerationOnly
    }
}

/// §2.7 — why an item cannot be queued (the model's `ByteRefusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRefusal {
    /// Larger than the RPC item cap: no amount of waiting helps.
    ItemTooLarge,
    /// Larger than the *entire* per-call budget: likewise impossible.
    ExceedsCallBudget,
    /// The per-call budget for this direction is currently full.
    CallBudgetFull,
    /// The caller's combined budget is currently full.
    CallerBudgetFull,
    /// The node's aggregate budget is currently full.
    NodeBudgetFull,
    /// Checked arithmetic overflowed.
    Overflow,
}

impl ByteRefusal {
    /// Whether waiting could ever satisfy this request. `false` means the
    /// item must fail promptly rather than park on permits that can never
    /// be granted (§2.7).
    pub fn is_satisfiable_by_waiting(&self) -> bool {
        matches!(
            self,
            ByteRefusal::CallBudgetFull
                | ByteRefusal::CallerBudgetFull
                | ByteRefusal::NodeBudgetFull
        )
    }
}

/// Which queue an item entered (the model's `Direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ByteDirection {
    /// Caller → provider (`apply_request_chunk_to_senders`).
    Request,
    /// Provider → caller (`RpcResponseSink::send_wait`).
    Response,
}

/// The Q1 queued-byte ceilings (§2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteLimits {
    /// Queued bytes per call **per direction**.
    pub per_call: usize,
    /// Queued bytes per caller, combined directions and calls.
    pub per_caller: usize,
    /// Queued bytes per node, combined everything.
    pub per_node: usize,
}

impl ByteLimits {
    /// Q1: 16 MiB per call per direction, 64 MiB per caller, 512 MiB per node.
    pub const fn q1_defaults() -> Self {
        Self {
            per_call: 16 * 1024 * 1024,
            per_caller: 64 * 1024 * 1024,
            per_node: 512 * 1024 * 1024,
        }
    }

    /// Positive, and each scope inside the next (Q1 validation).
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self.per_call == 0 || self.per_caller == 0 || self.per_node == 0 {
            return Err(LimitsError::NotPositive);
        }
        if self.per_call > self.per_caller {
            return Err(LimitsError::BytePerCallAboveCaller);
        }
        if self.per_caller > self.per_node {
            return Err(LimitsError::BytePerCallerAboveNode);
        }
        Ok(())
    }
}

/// The Q1 active-call ceilings (§3/Q1). The model's `CallLimits` folds the
/// §2.1 lifetime policy in as well; production keeps the lifetime at the
/// fold (`StreamLifetimePolicy`), so only the quota half lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLimits {
    /// Active protected calls per node, including `Opening` reservations
    /// and terminal records not yet reclaimed.
    pub max_active_node: usize,
    /// Active calls per authenticated caller, across its sessions.
    pub max_active_per_caller: usize,
    /// Active calls per external acting org, across its member identities.
    pub max_active_per_org: usize,
    /// How long an `Opening` reservation may wait for verification before
    /// it is reaped. Finite by requirement: a lost bridge must not pin a
    /// slot.
    pub verification_deadline_ns: u64,
}

impl CallLimits {
    /// Q1 initial defaults: 4096 / 64 / 512, 30 s verification deadline.
    pub const fn q1_defaults() -> Self {
        Self {
            max_active_node: 4096,
            max_active_per_caller: 64,
            max_active_per_org: 512,
            verification_deadline_ns: 30 * 1_000_000_000,
        }
    }

    /// Startup validation (Q1): positive values and caller/org ceilings
    /// inside the node ceiling.
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self.max_active_node == 0
            || self.max_active_per_caller == 0
            || self.max_active_per_org == 0
            || self.verification_deadline_ns == 0
        {
            return Err(LimitsError::NotPositive);
        }
        if self.max_active_per_caller > self.max_active_node {
            return Err(LimitsError::PerCallerAboveNode);
        }
        if self.max_active_per_org > self.max_active_node {
            return Err(LimitsError::PerOrgAboveNode);
        }
        Ok(())
    }
}

/// Why a limit set is unusable at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitsError {
    /// A zero ceiling: nothing could ever be admitted.
    NotPositive,
    /// `max_active_per_caller > max_active_node`.
    PerCallerAboveNode,
    /// `max_active_per_org > max_active_node`.
    PerOrgAboveNode,
    /// `per_call > per_caller`.
    BytePerCallAboveCaller,
    /// `per_caller > per_node`.
    BytePerCallerAboveNode,
}

impl std::fmt::Display for LimitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LimitsError::NotPositive => write!(f, "protected-call limits must be positive"),
            LimitsError::PerCallerAboveNode => {
                write!(f, "per-caller active-call limit exceeds the node limit")
            }
            LimitsError::PerOrgAboveNode => {
                write!(f, "per-org active-call limit exceeds the node limit")
            }
            LimitsError::BytePerCallAboveCaller => {
                write!(f, "per-call byte budget exceeds the caller budget")
            }
            LimitsError::BytePerCallerAboveNode => {
                write!(f, "per-caller byte budget exceeds the node budget")
            }
        }
    }
}

impl std::error::Error for LimitsError {}

/// The existing RPC item cap (`MAX_RPC_BODY_LEN`). Queue budgets do not
/// raise it (Q1), and it bounds a single item, never an aggregate.
pub const MAX_RPC_ITEM_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct ByteState {
    per_call: HashMap<(ProtectedCallKey, u64, ByteDirection), usize>,
    per_caller: HashMap<EntityId, usize>,
    node: usize,
}

/// The three-level byte budget of §2.7 (the model's `ByteBudgets`).
///
/// Reservation is a short locked reservation that checks before it
/// increments, in the documented order call → caller → node, rolling back
/// everything it already acquired on a later refusal. `fetch_add` followed
/// by a check is explicitly not a hard bound: two racing producers would
/// both observe an under-limit total after both had already published
/// their increments.
pub struct ByteBudgets {
    limits: ByteLimits,
    state: Mutex<ByteState>,
    /// Wakes producers parked in `send_wait` on a full budget: any release
    /// (or the owning call's retirement) makes them retry.
    released: Notify,
    unsettled_drops: std::sync::atomic::AtomicUsize,
}

impl ByteBudgets {
    /// Validate and build.
    pub fn new(limits: ByteLimits) -> Result<Arc<Self>, LimitsError> {
        limits.validate()?;
        Ok(Arc::new(Self {
            limits,
            state: Mutex::new(ByteState::default()),
            released: Notify::new(),
            unsettled_drops: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    /// The configured ceilings.
    pub fn limits(&self) -> ByteLimits {
        self.limits
    }

    /// Validate an item against the RPC item cap and the configured
    /// per-call budget **before any wait** (§2.7: an oversized item can
    /// never acquire enough capacity and must fail promptly).
    pub fn validate_item(&self, len: usize) -> Result<(), ByteRefusal> {
        if len > MAX_RPC_ITEM_BYTES {
            return Err(ByteRefusal::ItemTooLarge);
        }
        if len > self.limits.per_call {
            return Err(ByteRefusal::ExceedsCallBudget);
        }
        Ok(())
    }

    /// Reserve `len` bytes for one item. Order: call → caller → node, each
    /// level checked before it is incremented, each acquired level rolled
    /// back if a later one refuses.
    pub fn reserve(
        self: &Arc<Self>,
        key: ProtectedCallKey,
        incarnation: u64,
        direction: ByteDirection,
        len: usize,
    ) -> Result<ItemPermit, ByteRefusal> {
        self.validate_item(len)?;
        let mut state = self.state.lock();

        // 1 — per call, per direction.
        let call_slot = (key.clone(), incarnation, direction);
        let call_cur = state.per_call.get(&call_slot).copied().unwrap_or(0);
        let call_next = call_cur.checked_add(len).ok_or(ByteRefusal::Overflow)?;
        if call_next > self.limits.per_call {
            return Err(ByteRefusal::CallBudgetFull);
        }
        state.per_call.insert(call_slot, call_next);

        // 2 — per caller. On refusal, undo (1).
        let caller_cur = state.per_caller.get(&key.caller).copied().unwrap_or(0);
        let caller_next = match caller_cur.checked_add(len) {
            Some(next) if next <= self.limits.per_caller => next,
            Some(_) => {
                state
                    .per_call
                    .insert((key.clone(), incarnation, direction), call_cur);
                return Err(ByteRefusal::CallerBudgetFull);
            }
            None => {
                state
                    .per_call
                    .insert((key.clone(), incarnation, direction), call_cur);
                return Err(ByteRefusal::Overflow);
            }
        };
        state.per_caller.insert(key.caller.clone(), caller_next);

        // 3 — per node. On refusal, undo (2) and (1).
        let node_next = match state.node.checked_add(len) {
            Some(next) if next <= self.limits.per_node => next,
            Some(_) => {
                state.per_caller.insert(key.caller.clone(), caller_cur);
                state
                    .per_call
                    .insert((key.clone(), incarnation, direction), call_cur);
                return Err(ByteRefusal::NodeBudgetFull);
            }
            None => {
                state.per_caller.insert(key.caller.clone(), caller_cur);
                state
                    .per_call
                    .insert((key.clone(), incarnation, direction), call_cur);
                return Err(ByteRefusal::Overflow);
            }
        };
        state.node = node_next;
        drop(state);

        Ok(ItemPermit {
            budgets: Arc::clone(self),
            charge: ByteCharge {
                key,
                incarnation,
                direction,
                len,
            },
            settled: false,
        })
    }

    /// Aggregate bytes charged to the node.
    pub fn node_bytes(&self) -> usize {
        self.state.lock().node
    }

    /// Bytes charged to one caller.
    pub fn caller_bytes(&self, caller: &EntityId) -> usize {
        self.state
            .lock()
            .per_caller
            .get(caller)
            .copied()
            .unwrap_or(0)
    }

    /// Bytes charged to one call incarnation in one direction.
    pub fn call_bytes(
        &self,
        key: &ProtectedCallKey,
        incarnation: u64,
        direction: ByteDirection,
    ) -> usize {
        self.state
            .lock()
            .per_call
            .get(&(key.clone(), incarnation, direction))
            .copied()
            .unwrap_or(0)
    }

    /// How many permits were dropped without being released or
    /// transferred. A correct flow leaves this at zero; a nonzero value is
    /// an ownership bug the accounting would otherwise hide.
    pub fn unsettled_drops(&self) -> usize {
        self.unsettled_drops.load(Ordering::Relaxed)
    }

    #[expect(
        clippy::expect_used,
        reason = "invariant: byte permits release exactly once; a miss is a double-release ownership bug the accounting exists to surface"
    )]
    fn release_charge(&self, charge: ByteCharge) {
        let mut state = self.state.lock();
        let call_slot = (charge.key.clone(), charge.incarnation, charge.direction);
        // CHECKED, never saturating: saturating subtraction is exactly how
        // a double release or a refund of another call's bytes stays
        // invisible.
        let call_cur = state.per_call.get(&call_slot).copied().unwrap_or(0);
        let call_next = call_cur
            .checked_sub(charge.len)
            .expect("byte permit released twice, or against the wrong call");
        if call_next == 0 {
            state.per_call.remove(&call_slot);
        } else {
            state.per_call.insert(call_slot, call_next);
        }
        let caller_cur = state
            .per_caller
            .get(&charge.key.caller)
            .copied()
            .unwrap_or(0);
        let caller_next = caller_cur
            .checked_sub(charge.len)
            .expect("byte permit released twice, or against the wrong caller");
        if caller_next == 0 {
            state.per_caller.remove(&charge.key.caller);
        } else {
            state
                .per_caller
                .insert(charge.key.caller.clone(), caller_next);
        }
        state.node = state
            .node
            .checked_sub(charge.len)
            .expect("byte permit released twice against the node budget");
        drop(state);
        // A released reservation may satisfy a parked `send_wait`.
        self.released.notify_waiters();
    }
}

/// One item's byte charge, tied to the exact call incarnation so a refund
/// can never land on a successor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteCharge {
    /// The charged call.
    pub key: ProtectedCallKey,
    /// The exact incarnation.
    pub incarnation: u64,
    /// Which direction's per-call budget was charged.
    pub direction: ByteDirection,
    /// Bytes reserved.
    pub len: usize,
}

/// One admitted item's release-once permit bundle, tied to the call
/// incarnation (the model's `ItemPermit`). Not `Clone`: ownership is the
/// mechanism — dequeue, cancellation and queue discard compete to consume
/// this, rather than each subtracting a guessed byte count.
#[must_use = "a byte permit must be released or transferred exactly once"]
pub struct ItemPermit {
    budgets: Arc<ByteBudgets>,
    charge: ByteCharge,
    settled: bool,
}

impl ItemPermit {
    /// What this permit holds.
    pub fn charge(&self) -> &ByteCharge {
        &self.charge
    }

    /// Actual release: the bytes leave the three counters.
    pub fn release(mut self) {
        self.settle();
    }

    /// Hand the *same* charge to another bounded queue. The bytes stay
    /// charged — handoff is not memory reclamation — and exactly one live
    /// permit continues to own them.
    pub fn transfer(mut self) -> ItemPermit {
        self.settled = true;
        ItemPermit {
            budgets: Arc::clone(&self.budgets),
            charge: self.charge.clone(),
            settled: false,
        }
    }

    fn settle(&mut self) {
        if !self.settled {
            self.settled = true;
            self.budgets.release_charge(self.charge.clone());
        }
    }
}

impl std::fmt::Debug for ItemPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ItemPermit")
            .field("charge", &self.charge)
            .field("settled", &self.settled)
            .finish()
    }
}

impl Drop for ItemPermit {
    fn drop(&mut self) {
        if !self.settled {
            self.budgets.unsettled_drops.fetch_add(1, Ordering::Relaxed);
            self.settle();
        }
    }
}

/// A queued item whose permit several parties race to consume: the pump
/// (or `RequestStream`) yielding it, retirement cancelling it, and the
/// queue discarding it. Exactly one [`SharedPermit::take`] wins (the
/// model's `SharedPermit`).
pub struct SharedPermit {
    slot: Mutex<Option<ItemPermit>>,
}

impl SharedPermit {
    /// Wrap a permit for contended consumption.
    pub fn new(permit: ItemPermit) -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Some(permit)),
        })
    }

    /// Consume the permit. Exactly one caller ever gets `Some`.
    pub fn take(&self) -> Option<ItemPermit> {
        self.slot.lock().take()
    }

    /// Whether some party has already consumed it.
    pub fn is_consumed(&self) -> bool {
        self.slot.lock().is_none()
    }
}

impl std::fmt::Debug for SharedPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedPermit")
            .field("consumed", &self.is_consumed())
            .finish()
    }
}

/// A queued chunk riding its byte permit. The permit is consumed by
/// whichever party wins — the consumer yielding the body, or retirement /
/// queue discard releasing it.
#[derive(Debug)]
pub struct ChargedChunk {
    /// The chunk body.
    pub body: bytes::Bytes,
    /// The item's byte reservation (release-once).
    pub permit: Option<Arc<SharedPermit>>,
}

/// Release a chunk's permit at the moment its bytes leave the queue
/// accounting (emit / yield). Items without protected accounting carry
/// `None` and cost one branch.
fn release_chunk_permit(chunk: &ChargedChunk) {
    if let Some(shared) = chunk.permit.as_ref() {
        if let Some(permit) = shared.take() {
            permit.release();
        }
    }
}

/// Verified member facts, known only after the proof is checked (the
/// model's `VerifiedFacts`).
#[derive(Debug, Clone)]
pub struct VerifiedCallFacts {
    /// The org the caller is verified to be acting for.
    pub acting_org: OrgId,
    /// The verified member identity.
    pub member: EntityId,
    /// That member's certificate generation (the floor comparison term).
    pub member_generation: u32,
    /// The §2.1 effective deadline, when this shape resolves one
    /// (`None` for unary records at slice 1.4 — the §2.1 machinery is
    /// streaming-scoped, and enforcing it on unary would change unary
    /// behavior).
    pub deadline: Option<ResolvedStreamDeadline>,
}

/// Everything `reserve` can know before decode (the model's
/// `OpeningRequest`).
#[derive(Debug, Clone)]
pub struct OpeningRequest {
    /// `(caller, call_id)`.
    pub key: ProtectedCallKey,
    /// The exact originating session.
    pub session: SessionIdentity,
    /// The routing registry's session generation; `None` models the
    /// `u64::MAX` terminal marker (`SessionCurrentness` exhaustion).
    pub session_generation: Option<u64>,
    /// The protected registration this opening targets.
    pub registration: u64,
    /// The streaming shape of the call (shape-aware; `Unary` for the
    /// protected unary path).
    pub shape: RpcCallShape,
    /// Wall-clock now, nanoseconds (the admission's one clock sample).
    pub now_ns: u64,
}

/// A held `Opening` slot (the model's `Reservation`) with the bridge's
/// reservation-guard ownership (§2.4): dropping it untransferred releases
/// the slot exactly once.
#[must_use = "an opening reservation must be installed or released"]
pub struct ReservationGuard {
    registry: Arc<ProtectedCallRegistry>,
    /// The reserved key.
    pub key: ProtectedCallKey,
    /// The exact incarnation every later operation is conditional on.
    pub incarnation: u64,
    /// The registry's authority epoch at reserve time.
    pub epoch_at_reserve: u64,
    /// When this reservation is reaped if verification has not completed.
    pub verify_by_ns: u64,
    armed: bool,
}

impl std::fmt::Debug for ReservationGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReservationGuard")
            .field("key", &self.key)
            .field("incarnation", &self.incarnation)
            .field("epoch_at_reserve", &self.epoch_at_reserve)
            .finish()
    }
}

impl ReservationGuard {
    /// Disarm the bridge-side rollback — used when ownership moves into a
    /// [`ProtectedCallLease`] at install.
    fn defuse(&mut self) {
        self.armed = false;
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.registry.release(&self.key, self.incarnation);
        }
    }
}

/// An installed record, ready for the fold's confirm — contract 5's
/// lease-carrying seam. Dropping it untransferred releases the record
/// (bridge ownership, §2.4); a successful transfer disarms it so only the
/// supervisor completes the record.
#[must_use = "an admission lease must be transferred to a supervisor"]
pub struct ProtectedCallLease {
    registry: Arc<ProtectedCallRegistry>,
    /// The admitted key.
    pub key: ProtectedCallKey,
    /// The exact incarnation.
    pub incarnation: u64,
    /// The record's registration.
    pub registration: u64,
    /// The record's exact session binding.
    pub session: SessionIdentity,
    armed: bool,
}

impl std::fmt::Debug for ProtectedCallLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedCallLease")
            .field("key", &self.key)
            .field("incarnation", &self.incarnation)
            .field("registration", &self.registration)
            .field("session", &self.session)
            .finish()
    }
}

impl ProtectedCallLease {
    /// The registry this lease belongs to (the fold's transfer target).
    pub fn registry(&self) -> &Arc<ProtectedCallRegistry> {
        &self.registry
    }

    fn defuse(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProtectedCallLease {
    fn drop(&mut self) {
        if self.armed {
            self.registry.release(&self.key, self.incarnation);
        }
    }
}

/// Where a record sits in the admission transaction (the model's `Phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryPhase {
    /// Reserved, not yet verified. No member facts.
    Opening,
    /// Installed with verified facts; the fold has not taken ownership.
    Admitted,
    /// Ownership transferred to a supervisor. `Draining` lives inside the
    /// call record under this phase — it is still live.
    Running,
    /// A terminal has been selected. The record still owns its key and its
    /// quota slots until its one cleanup owner removes it.
    Terminal,
}

/// Which side owns the single conditional removal (§2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOwner {
    /// Before transfer: the bridge's reservation guard.
    Bridge,
    /// After transfer: the supervisor, after disposing of its work.
    Supervisor,
}

struct RegistryRecord {
    incarnation: u64,
    phase: RegistryPhase,
    session: SessionIdentity,
    registration: u64,
    shape: RpcCallShape,
    verify_by_ns: u64,
    captured: AdmissionStamp,
    facts: Option<VerifiedCallFacts>,
    charged_org: Option<OrgId>,
    cleanup: CleanupOwner,
    terminal: Option<StreamTerminalReason>,
    /// The registered owner hook (the model's `SupervisorOwner`): the
    /// call's retire signal plus an optional synchronous side effect (the
    /// unary fold's cancellation).
    signal: Arc<StreamRetireSignal>,
    on_retire: Option<Arc<dyn Fn(StreamTerminalReason) + Send + Sync>>,
    /// Queued items whose byte permits this record still tracks (§2.7 —
    /// retirement consumes whatever has not been taken by a consumer).
    queued: Vec<Arc<SharedPermit>>,
}

/// The per-item §2.3 verdict (the model's `CommitVerdict`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitVerdict {
    /// The captured view is still live.
    Proceed,
    /// The generation moved, the floor still permits this member, and the
    /// captured generation was refreshed.
    Requalified,
    /// The call is retired with this reason.
    Retired(StreamTerminalReason),
    /// No record with that exact incarnation.
    Unknown,
}

struct RegistryInner {
    /// The bound `(authority, store)` pair the live views sample.
    authority: Option<Arc<NodeAuthority>>,
    store: Option<Arc<OrgRevocationStore>>,
    authority_epoch: u64,
    next_incarnation: u64,
    records: HashMap<ProtectedCallKey, RegistryRecord>,
    active_node: usize,
    active_per_caller: HashMap<EntityId, usize>,
    active_per_org: HashMap<OrgId, usize>,
}

/// One per `MeshNode`: the exact-incarnation admission/retirement
/// transaction of §3 (the model's `ProtectedCallRegistry`, wired to the
/// real `OrgRevocationStore`/`NodeAuthority`).
pub struct ProtectedCallRegistry {
    inner: Mutex<RegistryInner>,
    limits: CallLimits,
    bytes: Arc<ByteBudgets>,
    /// §2.3's raise feed — the second `subscribe_floors_raised`
    /// subscriber, owned here so replacement can drop it outside the
    /// registry lock (a subscription drop drains in-flight callbacks).
    subscription: Mutex<Option<RaiseSubscription>>,
    removals: Mutex<HashMap<(ProtectedCallKey, u64), usize>>,
}

impl ProtectedCallRegistry {
    /// Validate the Q1 limits and build an unbound registry. The store
    /// binding (and its raise subscription) arrives through
    /// [`Self::bind_store`] at the node's store-install site.
    pub fn with_limits(
        limits: CallLimits,
        byte_limits: ByteLimits,
    ) -> Result<Arc<Self>, LimitsError> {
        limits.validate()?;
        let bytes = ByteBudgets::new(byte_limits)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(RegistryInner {
                authority: None,
                store: None,
                authority_epoch: 0,
                next_incarnation: 1,
                records: HashMap::new(),
                active_node: 0,
                active_per_caller: HashMap::new(),
                active_per_org: HashMap::new(),
            }),
            limits,
            bytes,
            subscription: Mutex::new(None),
            removals: Mutex::new(HashMap::new()),
        }))
    }

    /// A registry with the Q1 defaults (validated at construction).
    pub fn with_q1_defaults() -> Result<Arc<Self>, LimitsError> {
        Self::with_limits(CallLimits::q1_defaults(), ByteLimits::q1_defaults())
    }

    /// The configured active-call ceilings.
    pub fn limits(&self) -> CallLimits {
        self.limits
    }

    /// The §2.7 byte budgets.
    pub fn bytes(&self) -> &Arc<ByteBudgets> {
        &self.bytes
    }

    /// The registry's authority epoch. Only a *notified* raise moves it,
    /// which is precisely why it cannot be the sole basis for an install
    /// or commit decision (publication lands before notification).
    pub fn authority_epoch(&self) -> u64 {
        self.inner.lock().authority_epoch
    }

    /// How many records exist, in any phase.
    pub fn record_count(&self) -> usize {
        self.inner.lock().records.len()
    }

    /// Active calls charged to the node.
    pub fn active_node(&self) -> usize {
        self.inner.lock().active_node
    }

    /// Active calls charged to one caller.
    pub fn active_for_caller(&self, caller: &EntityId) -> usize {
        self.inner
            .lock()
            .active_per_caller
            .get(caller)
            .copied()
            .unwrap_or(0)
    }

    /// Active calls charged to one verified acting org.
    pub fn active_for_org(&self, org: &OrgId) -> usize {
        self.inner
            .lock()
            .active_per_org
            .get(org)
            .copied()
            .unwrap_or(0)
    }

    /// The phase of a record, if it exists.
    pub fn phase(&self, key: &ProtectedCallKey) -> Option<RegistryPhase> {
        self.inner.lock().records.get(key).map(|r| r.phase)
    }

    /// The admitted shape of a record (the registry is shape-aware).
    pub fn shape(&self, key: &ProtectedCallKey) -> Option<RpcCallShape> {
        self.inner.lock().records.get(key).map(|r| r.shape)
    }

    /// The selected terminal, if any.
    pub fn terminal_reason(&self, key: &ProtectedCallKey) -> Option<StreamTerminalReason> {
        self.inner
            .lock()
            .records
            .get(key)
            .and_then(|r| r.terminal.clone())
    }

    /// Which side currently owns the single removal.
    pub fn cleanup_owner(&self, key: &ProtectedCallKey) -> Option<CleanupOwner> {
        self.inner.lock().records.get(key).map(|r| r.cleanup)
    }

    /// The captured security view of a record, refreshed by
    /// requalification.
    pub fn captured_view(&self, key: &ProtectedCallKey) -> Option<AdmissionStamp> {
        self.inner.lock().records.get(key).map(|r| r.captured)
    }

    /// How many times `(key, incarnation)` has been removed. Exactly-once
    /// removal is the property; a ledger makes "exactly once" observable
    /// rather than inferred from a boolean.
    pub fn removals(&self, key: &ProtectedCallKey, incarnation: u64) -> usize {
        self.removals
            .lock()
            .get(&(key.clone(), incarnation))
            .copied()
            .unwrap_or(0)
    }

    // ----------------------------------------------------------------
    // §2.3 — the real-store binding and raise feed
    // ----------------------------------------------------------------

    /// The store's poison flag through the bound handle (None when
    /// unbound).
    pub fn store_poisoned(&self) -> Option<bool> {
        let inner = self.inner.lock();
        inner.store.as_ref().map(|s| s.is_poisoned())
    }

    /// Bind (or REBIND) the registry to the installed `(authority, store)`
    /// pair — §2.3's store/authority replacement path. On a changed pair:
    /// every record captured under the old `(authority_ptr, store_ptr)` is
    /// retired (`AuthorityUnavailable`) BEFORE this returns, and the raise
    /// subscription moves to the new store (created if absent). The old
    /// `RaiseSubscription` is dropped OUTSIDE the registry lock: dropping
    /// it drains in-flight callbacks, and a callback takes this lock.
    pub fn bind_store(
        self: &Arc<Self>,
        authority: Option<Arc<NodeAuthority>>,
        store: Arc<OrgRevocationStore>,
    ) {
        let new_authority_ptr = authority
            .as_ref()
            .map_or(0, |a| Arc::as_ptr(a) as *const () as usize);
        let new_store_ptr = Arc::as_ptr(&store) as *const () as usize;
        {
            let mut inner = self.inner.lock();
            let same_pair = inner.store.as_ref().is_some_and(|s| {
                Arc::as_ptr(s) as *const () as usize == new_store_ptr
                    && inner
                        .authority
                        .as_ref()
                        .map_or(0, |a| Arc::as_ptr(a) as *const () as usize)
                        == new_authority_ptr
            });
            if same_pair {
                return;
            }
            inner.authority_epoch = inner.authority_epoch.wrapping_add(1);
            let victims: Vec<(ProtectedCallKey, u64)> = inner
                .records
                .iter()
                .filter(|(_, record)| {
                    record.phase != RegistryPhase::Terminal
                        && (record.captured.authority_ptr != new_authority_ptr
                            || record.captured.store_ptr != new_store_ptr)
                })
                .map(|(key, record)| (key.clone(), record.incarnation))
                .collect();
            for (key, incarnation) in victims {
                self.retire_locked(
                    &mut inner,
                    &key,
                    incarnation,
                    StreamTerminalReason::AuthorityUnavailable,
                );
            }
            inner.authority = authority;
            inner.store = Some(Arc::clone(&store));
        }
        // §2.3: the registry re-subscribes to the new store so it never
        // sits without a raise feed. The outgoing guard drops HERE, outside
        // the registry lock (its Drop drains in-flight callbacks).
        let weak = Arc::downgrade(self);
        let subscription = store.subscribe_floors_raised(move |raised: &[RaisedFloor]| {
            // A detached store's late raises must not mutate a node it no
            // longer speaks for (the same store-identity gate the existing
            // fold/routing subscriber applies).
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let still_bound = {
                let inner = registry.inner.lock();
                inner
                    .store
                    .as_ref()
                    .is_some_and(|s| Arc::as_ptr(s) as *const () as usize == new_store_ptr)
            };
            if !still_bound {
                return;
            }
            registry.on_floors_raised(raised);
        });
        let old_subscription = self.subscription.lock().replace(subscription);
        drop(old_subscription);
    }

    /// Selective raise callback (§2.3). Bumps the epoch, then retires
    /// every record whose `(acting_org, member)` generation is below a
    /// raised floor. An empty slice is the authority-changed wake
    /// (`notify_authority_changed`: authority moved or poison recovery)
    /// and retires everything, including `Opening` reservations that have
    /// no facts to compare.
    pub fn on_floors_raised(&self, raised: &[RaisedFloor]) {
        let mut inner = self.inner.lock();
        inner.authority_epoch = inner.authority_epoch.wrapping_add(1);
        let victims: Vec<(ProtectedCallKey, u64, StreamTerminalReason)> = inner
            .records
            .iter()
            .filter_map(|(key, record)| {
                if record.phase == RegistryPhase::Terminal {
                    return None;
                }
                if raised.is_empty() {
                    return Some((
                        key.clone(),
                        record.incarnation,
                        StreamTerminalReason::AuthorityUnavailable,
                    ));
                }
                let facts = record.facts.as_ref()?;
                raised
                    .iter()
                    .any(|(org, member, floor)| {
                        *org == facts.acting_org
                            && *member == facts.member
                            && *floor > facts.member_generation
                    })
                    .then_some((
                        key.clone(),
                        record.incarnation,
                        StreamTerminalReason::Revoked,
                    ))
            })
            .collect();
        for (key, incarnation, reason) in victims {
            self.retire_locked(&mut inner, &key, incarnation, reason);
        }
    }

    /// The authority moved or poison recovered with no floor raised:
    /// retire all.
    pub fn on_authority_changed(&self) {
        self.on_floors_raised(&[]);
    }

    /// Session replacement or a dead-peer sweep (§2.3's quantified
    /// boundary: before `install_peer_locked`/the sweep returns). Matches
    /// the EXACT `(peer, session_id, establishment)` triple — a bare
    /// truncated session id is shared across unrelated peers, so matching
    /// on it alone would retire a bystander's call.
    pub fn retire_session(&self, session: &SessionIdentity, reason: StreamTerminalReason) -> usize {
        let mut inner = self.inner.lock();
        let victims: Vec<(ProtectedCallKey, u64)> = inner
            .records
            .iter()
            .filter(|(_, r)| r.phase != RegistryPhase::Terminal && r.session == *session)
            .map(|(key, r)| (key.clone(), r.incarnation))
            .collect();
        let mut retired = 0;
        for (key, incarnation) in victims {
            if self.retire_locked(&mut inner, &key, incarnation, reason.clone()) {
                retired += 1;
            }
        }
        retired
    }

    /// `ServeHandle::drop` for one protected registration.
    pub fn retire_registration(&self, registration: u64, reason: StreamTerminalReason) -> usize {
        self.retire_where(reason, |r| r.registration == registration)
    }

    /// Node shutdown: every live record of this node retires (Q3/C9 — the
    /// `ProtectedStreamOwners::retire_all` discipline at node scope).
    pub fn retire_all(&self, reason: StreamTerminalReason) -> usize {
        self.retire_where(reason, |_| true)
    }

    fn retire_where(
        &self,
        reason: StreamTerminalReason,
        pred: impl Fn(&RegistryRecord) -> bool,
    ) -> usize {
        let mut inner = self.inner.lock();
        let victims: Vec<(ProtectedCallKey, u64)> = inner
            .records
            .iter()
            .filter(|(_, r)| r.phase != RegistryPhase::Terminal && pred(r))
            .map(|(key, r)| (key.clone(), r.incarnation))
            .collect();
        let mut retired = 0;
        for (key, incarnation) in victims {
            if self.retire_locked(&mut inner, &key, incarnation, reason.clone()) {
                retired += 1;
            }
        }
        retired
    }

    // ----------------------------------------------------------------
    // §3 step 1 — reserve
    // ----------------------------------------------------------------

    /// Take an `Opening` slot before any signature work (the model's
    /// `reserve`). Charges the authenticated caller and the node only:
    /// the acting org is whatever an *unverified* proof claims at this
    /// point, so charging it would let a forged label exhaust a real
    /// organization's quota.
    pub fn reserve(
        self: &Arc<Self>,
        req: OpeningRequest,
    ) -> Result<ReservationGuard, AdmissionDenied> {
        if req.session_generation.is_none() {
            // `SessionCurrentness` generation `u64::MAX` (§3): refuse
            // admission. No C4 variant names it (finding F-S1.4-1).
            return Err(AdmissionDenied::AuthorityChanged);
        }
        let mut inner = self.inner.lock();

        // The key check comes first: it is the one refusal that must land
        // before decode, and it costs a hash lookup.
        if inner.records.contains_key(&req.key) {
            return Err(AdmissionDenied::ActiveCallOwned);
        }
        if inner.active_node >= self.limits.max_active_node {
            return Err(AdmissionDenied::ActiveStreamCapacity);
        }
        let caller_active = inner
            .active_per_caller
            .get(&req.key.caller)
            .copied()
            .unwrap_or(0);
        if caller_active >= self.limits.max_active_per_caller {
            return Err(AdmissionDenied::ActiveStreamCapacity);
        }
        let (Some(node_next), Some(caller_next)) = (
            inner.active_node.checked_add(1),
            caller_active.checked_add(1),
        ) else {
            return Err(AdmissionDenied::ActiveStreamCapacity);
        };
        let Some(verify_by_ns) = req.now_ns.checked_add(self.limits.verification_deadline_ns)
        else {
            return Err(AdmissionDenied::ActiveStreamCapacity);
        };

        let Some((captured, _)) = self.live_view_locked(&inner) else {
            return Err(AdmissionDenied::ProviderAuthorityUnavailable);
        };
        if captured.store_generation.is_none() || captured.poisoned {
            return Err(AdmissionDenied::ProviderAuthorityUnavailable);
        }

        let incarnation = inner.next_incarnation;
        inner.next_incarnation += 1;
        inner.active_node = node_next;
        inner
            .active_per_caller
            .insert(req.key.caller.clone(), caller_next);
        inner.records.insert(
            req.key.clone(),
            RegistryRecord {
                incarnation,
                phase: RegistryPhase::Opening,
                session: req.session,
                registration: req.registration,
                shape: req.shape,
                verify_by_ns,
                captured,
                facts: None,
                charged_org: None,
                cleanup: CleanupOwner::Bridge,
                terminal: None,
                signal: Arc::new(StreamRetireSignal::new()),
                on_retire: None,
                queued: Vec::new(),
            },
        );
        let epoch_at_reserve = inner.authority_epoch;
        drop(inner);
        Ok(ReservationGuard {
            registry: Arc::clone(self),
            key: req.key,
            incarnation,
            epoch_at_reserve,
            verify_by_ns,
            armed: true,
        })
    }

    fn live_view_locked(
        &self,
        inner: &RegistryInner,
    ) -> Option<(AdmissionStamp, Arc<OrgRevocationState>)> {
        let store = inner.store.as_ref()?;
        let authority_ptr = inner
            .authority
            .as_ref()
            .map_or(0, |a| Arc::as_ptr(a) as *const () as usize);
        let store_ptr = Arc::as_ptr(store) as *const () as usize;
        let (floors, generation) = store.snapshot_with_generation().ok()?;
        let stamp = AdmissionStamp {
            authority_ptr,
            store_ptr,
            store_generation: Some(generation),
            poisoned: store.is_poisoned(),
        };
        Some((stamp, floors))
    }

    // ----------------------------------------------------------------
    // §3 step 4 — install
    // ----------------------------------------------------------------

    /// Fill the verified facts and transition `Opening → Admitted`, under
    /// the registry lock (the model's `install`). Three refusals, in
    /// order: the reservation was retired/lost meanwhile (`AuthorityChanged`);
    /// the security view moved ⇒ requalify per §2.3 (store moved / poison /
    /// exhausted generation is `ProviderAuthorityUnavailable`; a
    /// generation-only move compares `floor_for(acting_org, member)`
    /// against THIS record's member generation and refreshes the captured
    /// generation on success); the now-verified acting-org quota is
    /// reserved atomically with the transition, rolling back on refusal.
    #[expect(
        clippy::expect_used,
        reason = "invariant: record presence and verified facts are validated under this same lock; None here means the lock discipline is broken and must surface, not be papered over"
    )]
    pub fn install(
        self: &Arc<Self>,
        reservation: &mut ReservationGuard,
        facts: VerifiedCallFacts,
        now_ns: u64,
    ) -> Result<ProtectedCallLease, AdmissionDenied> {
        let mut inner = self.inner.lock();
        let Some((live, floors)) = self.live_view_locked(&inner) else {
            return Err(AdmissionDenied::ProviderAuthorityUnavailable);
        };
        let epoch_now = inner.authority_epoch;

        let Some(record) = inner.records.get(&reservation.key) else {
            // Reclaimed by its cleanup owner while we verified.
            reservation.defuse();
            return Err(AdmissionDenied::AuthorityChanged);
        };
        if record.incarnation != reservation.incarnation {
            reservation.defuse();
            return Err(AdmissionDenied::AuthorityChanged);
        }
        match record.phase {
            RegistryPhase::Opening => {}
            RegistryPhase::Terminal => {
                reservation.defuse();
                return Err(AdmissionDenied::AuthorityChanged);
            }
            RegistryPhase::Admitted | RegistryPhase::Running => {
                return Err(AdmissionDenied::AuthorityChanged);
            }
        }
        let captured = record.captured;
        let verify_by_ns = record.verify_by_ns;

        if now_ns > verify_by_ns {
            self.retire_locked(
                &mut inner,
                &reservation.key,
                reservation.incarnation,
                StreamTerminalReason::AuthorityUnavailable,
            );
            return Err(AdmissionDenied::AuthorityChanged);
        }

        if epoch_now != reservation.epoch_at_reserve || !is_current(&captured, &live) {
            match stamp_movement(&captured, &live) {
                ViewMovement::Unusable => {
                    self.retire_locked(
                        &mut inner,
                        &reservation.key,
                        reservation.incarnation,
                        StreamTerminalReason::AuthorityUnavailable,
                    );
                    return Err(AdmissionDenied::ProviderAuthorityUnavailable);
                }
                ViewMovement::Unchanged | ViewMovement::GenerationOnly => {
                    if floors.floor_for(&facts.acting_org, &facts.member) > facts.member_generation
                    {
                        self.retire_locked(
                            &mut inner,
                            &reservation.key,
                            reservation.incarnation,
                            StreamTerminalReason::Revoked,
                        );
                        return Err(AdmissionDenied::Revoked);
                    }
                }
            }
        }

        // The acting org is verified only now, so this is the first moment
        // its quota may legitimately be charged.
        let org_active = inner
            .active_per_org
            .get(&facts.acting_org)
            .copied()
            .unwrap_or(0);
        if org_active >= self.limits.max_active_per_org {
            self.retire_locked(
                &mut inner,
                &reservation.key,
                reservation.incarnation,
                StreamTerminalReason::ResourceExhausted,
            );
            return Err(AdmissionDenied::ActiveStreamCapacity);
        }
        let Some(org_next) = org_active.checked_add(1) else {
            self.retire_locked(
                &mut inner,
                &reservation.key,
                reservation.incarnation,
                StreamTerminalReason::ResourceExhausted,
            );
            return Err(AdmissionDenied::ActiveStreamCapacity);
        };
        inner.active_per_org.insert(facts.acting_org, org_next);

        let record = inner
            .records
            .get_mut(&reservation.key)
            .expect("record presence checked under this same lock");
        record.captured = live;
        record.facts = Some(facts);
        record.charged_org = Some(record.facts.as_ref().expect("just filled").acting_org);
        record.phase = RegistryPhase::Admitted;
        let registration = record.registration;
        let session = record.session.clone();
        drop(inner);
        reservation.defuse();
        Ok(ProtectedCallLease {
            registry: Arc::clone(self),
            key: reservation.key.clone(),
            incarnation: reservation.incarnation,
            registration,
            session,
            armed: true,
        })
    }

    // ----------------------------------------------------------------
    // §3 step 5 — confirm, as an ownership transfer
    // ----------------------------------------------------------------

    /// Atomically register the cancellation-ready owner and mark `Running`
    /// (the model's `confirm` — one lock-held operation, so a retire
    /// cannot land between the check and the transfer). The owner (the
    /// call's retire signal + optional hook) exists before its task is
    /// scheduled: a retire arriving immediately after this returns still
    /// reaches something.
    pub fn confirm(
        &self,
        lease: &mut ProtectedCallLease,
        signal: Arc<StreamRetireSignal>,
        on_retire: Option<Arc<dyn Fn(StreamTerminalReason) + Send + Sync>>,
    ) -> Result<(), AdmissionDenied> {
        let mut inner = self.inner.lock();
        let Some(record) = inner.records.get_mut(&lease.key) else {
            return Err(AdmissionDenied::AuthorityChanged);
        };
        if record.incarnation != lease.incarnation {
            return Err(AdmissionDenied::AuthorityChanged);
        }
        match record.phase {
            RegistryPhase::Admitted => {}
            RegistryPhase::Terminal => return Err(AdmissionDenied::AuthorityChanged),
            RegistryPhase::Opening | RegistryPhase::Running => {
                return Err(AdmissionDenied::AuthorityChanged)
            }
        }
        record.phase = RegistryPhase::Running;
        record.cleanup = CleanupOwner::Supervisor;
        record.signal = signal;
        record.on_retire = on_retire;
        drop(inner);
        lease.defuse();
        Ok(())
    }

    // ----------------------------------------------------------------
    // §2.3 — the per-item commit boundary
    // ----------------------------------------------------------------

    /// The per-item check of §2.3 WITHOUT committing anything (the model's
    /// `commit_check`) — exposed because production has this boundary, but
    /// it is not a licence to enqueue afterwards. [`Self::begin_commit`]
    /// is the ownership-preserving form.
    pub fn commit_check(&self, key: &ProtectedCallKey, incarnation: u64) -> CommitVerdict {
        let mut inner = self.inner.lock();
        self.commit_check_locked(&mut inner, key, incarnation)
    }

    /// Check and hold: the verdict and the enqueue are one ownership
    /// operation (the model's `begin_commit`). Holding the returned
    /// transaction holds the registry lock, so retirement cannot land
    /// between the verdict and the enqueue.
    pub fn begin_commit(
        &self,
        key: &ProtectedCallKey,
        incarnation: u64,
    ) -> Result<CommitTxn<'_>, CommitVerdict> {
        let mut inner = self.inner.lock();
        let verdict = self.commit_check_locked(&mut inner, key, incarnation);
        match verdict {
            CommitVerdict::Proceed | CommitVerdict::Requalified => Ok(CommitTxn {
                inner,
                key: key.clone(),
                incarnation,
                verdict,
            }),
            other => Err(other),
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "invariant: record presence is validated under this same lock"
    )]
    fn commit_check_locked(
        &self,
        inner: &mut RegistryInner,
        key: &ProtectedCallKey,
        incarnation: u64,
    ) -> CommitVerdict {
        let Some((live, floors)) = self.live_view_locked(inner) else {
            // The bound store vanished under us: fail closed for the
            // record that is trying to commit.
            let Some(record) = inner.records.get(key) else {
                return CommitVerdict::Unknown;
            };
            if record.incarnation != incarnation {
                return CommitVerdict::Unknown;
            }
            self.retire_locked(
                inner,
                key,
                incarnation,
                StreamTerminalReason::AuthorityUnavailable,
            );
            return CommitVerdict::Retired(StreamTerminalReason::AuthorityUnavailable);
        };
        let Some(record) = inner.records.get(key) else {
            return CommitVerdict::Unknown;
        };
        if record.incarnation != incarnation {
            return CommitVerdict::Unknown;
        }
        // "token.is_cancelled() → retired" is the first test, before any
        // stamp work: a cancelled call has already selected its outcome.
        if record.phase == RegistryPhase::Terminal {
            return CommitVerdict::Retired(
                record
                    .terminal
                    .clone()
                    .unwrap_or(StreamTerminalReason::Cancelled),
            );
        }
        let captured = record.captured;
        if is_current(&captured, &live) {
            return CommitVerdict::Proceed;
        }
        let Some(facts) = record.facts.clone() else {
            // No verified facts: there is nothing to requalify against.
            self.retire_locked(
                inner,
                key,
                incarnation,
                StreamTerminalReason::AuthorityUnavailable,
            );
            return CommitVerdict::Retired(StreamTerminalReason::AuthorityUnavailable);
        };
        match stamp_movement(&captured, &live) {
            ViewMovement::Unusable => {
                self.retire_locked(
                    inner,
                    key,
                    incarnation,
                    StreamTerminalReason::AuthorityUnavailable,
                );
                CommitVerdict::Retired(StreamTerminalReason::AuthorityUnavailable)
            }
            ViewMovement::Unchanged => CommitVerdict::Proceed,
            ViewMovement::GenerationOnly => {
                if floors.floor_for(&facts.acting_org, &facts.member) > facts.member_generation {
                    self.retire_locked(inner, key, incarnation, StreamTerminalReason::Revoked);
                    CommitVerdict::Retired(StreamTerminalReason::Revoked)
                } else {
                    let record = inner
                        .records
                        .get_mut(key)
                        .expect("record presence checked under this same lock");
                    record.captured = live;
                    CommitVerdict::Requalified
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // §2.4 — retirement and the two removal paths
    // ----------------------------------------------------------------

    /// Mark a record terminal, conditional on the exact incarnation, and
    /// signal the registered owner. First writer wins; a late retire
    /// against a reused key is a no-op. **Never removes** — removal
    /// belongs to the record's one cleanup owner. Retirement also consumes
    /// every queued byte permit the call still owns (§2.7: cancellation
    /// and queue discard compete for the item's ownership, exactly once)
    /// and wakes producers parked in `send_wait` on a full budget.
    pub fn retire(
        &self,
        key: &ProtectedCallKey,
        incarnation: u64,
        reason: StreamTerminalReason,
    ) -> bool {
        let mut inner = self.inner.lock();
        self.retire_locked(&mut inner, key, incarnation, reason)
    }

    fn retire_locked(
        &self,
        inner: &mut RegistryInner,
        key: &ProtectedCallKey,
        incarnation: u64,
        reason: StreamTerminalReason,
    ) -> bool {
        let Some(record) = inner.records.get_mut(key) else {
            return false;
        };
        if record.incarnation != incarnation || record.phase == RegistryPhase::Terminal {
            return false;
        }
        record.phase = RegistryPhase::Terminal;
        record.terminal = Some(reason.clone());
        let queued = std::mem::take(&mut record.queued);
        let signal = Arc::clone(&record.signal);
        let on_retire = record.on_retire.clone();
        // Explicit releases, not guessed subtraction: each queued item's
        // permit is consumed here unless a consumer already took it.
        for item in queued {
            if let Some(permit) = item.take() {
                permit.release();
            }
        }
        // Wake `send_wait` producers parked on a full budget — retirement
        // is the interruption §2.7 requires of every satisfiable wait.
        self.bytes.released.notify_waiters();
        signal.fire(reason.clone());
        if let Some(hook) = on_retire {
            hook(reason);
        }
        true
    }

    /// Pre-transfer rollback, owned by the bridge's reservation guard.
    /// Refuses once ownership has moved to a supervisor.
    pub fn release(&self, key: &ProtectedCallKey, incarnation: u64) -> bool {
        self.remove(key, incarnation, CleanupOwner::Bridge)
    }

    /// Post-transfer removal, owned by the supervisor after it has
    /// disposed of its work. Refuses before ownership transferred.
    pub fn complete(&self, key: &ProtectedCallKey, incarnation: u64) -> bool {
        self.remove(key, incarnation, CleanupOwner::Supervisor)
    }

    #[expect(
        clippy::expect_used,
        reason = "invariant: the record and its counters are validated under this same lock; a miss is a double-release ownership bug"
    )]
    fn remove(&self, key: &ProtectedCallKey, incarnation: u64, expected: CleanupOwner) -> bool {
        let mut inner = self.inner.lock();
        let matches = inner
            .records
            .get(key)
            .is_some_and(|r| r.incarnation == incarnation && r.cleanup == expected);
        if !matches {
            return false;
        }
        let record = inner
            .records
            .remove(key)
            .expect("presence checked under this same lock");
        inner.active_node = inner
            .active_node
            .checked_sub(1)
            .expect("node active-call counter released twice");
        let caller_slot = inner
            .active_per_caller
            .get_mut(&key.caller)
            .expect("caller active-call counter released twice");
        *caller_slot = caller_slot
            .checked_sub(1)
            .expect("caller active-call counter released twice");
        if *caller_slot == 0 {
            inner.active_per_caller.remove(&key.caller);
        }
        if let Some(org) = record.charged_org {
            let org_slot = inner
                .active_per_org
                .get_mut(&org)
                .expect("org active-call counter released twice");
            *org_slot = org_slot
                .checked_sub(1)
                .expect("org active-call counter released twice");
            if *org_slot == 0 {
                inner.active_per_org.remove(&org);
            }
        }
        // Queued items the removed call still owned: their permits are
        // consumed here, not guessed at.
        for item in &record.queued {
            if let Some(permit) = item.take() {
                permit.release();
            }
        }
        drop(inner);
        *self
            .removals
            .lock()
            .entry((key.clone(), incarnation))
            .or_insert(0) += 1;
        true
    }

    /// Reap `Opening` reservations whose verification deadline passed —
    /// the lost-bridge path. An opening retired before transfer must not
    /// wait for a supervisor that was never created, so this both retires
    /// and removes under the bridge's ownership.
    pub fn reap_expired_openings(&self, now_ns: u64) -> usize {
        let expired: Vec<(ProtectedCallKey, u64)> = {
            let inner = self.inner.lock();
            inner
                .records
                .iter()
                .filter(|(_, r)| r.phase == RegistryPhase::Opening && r.verify_by_ns < now_ns)
                .map(|(k, r)| (k.clone(), r.incarnation))
                .collect()
        };
        let mut reaped = 0;
        for (key, incarnation) in expired {
            self.retire(&key, incarnation, StreamTerminalReason::Timeout);
            if self.release(&key, incarnation) {
                reaped += 1;
            }
        }
        reaped
    }

    // ----------------------------------------------------------------
    // §2.7 — queue ownership
    // ----------------------------------------------------------------

    /// How many items the call still owns.
    pub fn queued_items(&self, key: &ProtectedCallKey) -> usize {
        self.inner
            .lock()
            .records
            .get(key)
            .map_or(0, |r| r.queued.len())
    }

    /// Retirement/removal consumes every item the call still owns.
    /// Returns how many permits this call actually won — items another
    /// party already consumed are not double counted, and no other call's
    /// bytes are touched.
    pub fn cancel_queued(&self, key: &ProtectedCallKey, incarnation: u64) -> usize {
        let mut inner = self.inner.lock();
        let Some(record) = inner.records.get_mut(key) else {
            return 0;
        };
        if record.incarnation != incarnation {
            return 0;
        }
        let items = std::mem::take(&mut record.queued);
        drop(inner);
        let mut released = 0;
        for item in items {
            if let Some(permit) = item.take() {
                permit.release();
                released += 1;
            }
        }
        released
    }
}

/// `AdmissionStamp::is_current` (`org_admission_gate.rs`) as a free
/// function — whole-stamp identity plus both generations usable and the
/// live view unpoisoned. The FAST PATH of the commit check uses this; the
/// fallback discriminates the movement (never retire a sibling for
/// another member's floor raise).
fn is_current(captured: &AdmissionStamp, live: &AdmissionStamp) -> bool {
    captured.is_current(live)
}

/// Check and commit as one ownership operation (the model's `CommitTxn`).
/// Holding this holds the registry lock, so retirement cannot land between
/// the verdict and the enqueue.
pub struct CommitTxn<'a> {
    inner: MutexGuard<'a, RegistryInner>,
    key: ProtectedCallKey,
    incarnation: u64,
    verdict: CommitVerdict,
}

impl CommitTxn<'_> {
    /// The verdict this transaction opened on.
    pub fn verdict(&self) -> &CommitVerdict {
        &self.verdict
    }

    /// Enqueue the item, handing its permit to the call's queue, and run
    /// the caller's queue-admission closure WHILE this transaction still
    /// holds the registry lock — so the §2.3 check and the enqueue are
    /// one ownership operation.
    #[expect(
        clippy::expect_used,
        reason = "invariant: record presence is validated when the transaction opened, with the guard held across it"
    )]
    pub fn commit_with(
        mut self,
        permit: ItemPermit,
        admit: impl FnOnce(&Arc<SharedPermit>),
    ) -> Arc<SharedPermit> {
        let shared = SharedPermit::new(permit);
        {
            let record = self
                .inner
                .records
                .get_mut(&self.key)
                .expect("presence validated when this transaction opened");
            debug_assert_eq!(record.incarnation, self.incarnation);
            record.queued.push(Arc::clone(&shared));
        }
        admit(&shared);
        shared
    }
}

// ---------------------------------------------------------------------
// Per-node registry resolution + the mesh.rs hook surface
// ---------------------------------------------------------------------

static PROTECTED_CALL_REGISTRIES: std::sync::LazyLock<DashMap<u64, Arc<ProtectedCallRegistry>>> =
    std::sync::LazyLock::new(DashMap::new);

/// The node's protected-call registry, created on first use with the Q1
/// defaults (validated at construction).
#[expect(
    clippy::expect_used,
    reason = "the Q1 limits validate by construction (startup validation per Q1); a panic here is a constant defect, not a runtime contingency"
)]
pub fn protected_call_registry_for(node_id: u64) -> Arc<ProtectedCallRegistry> {
    if let Some(existing) = PROTECTED_CALL_REGISTRIES.get(&node_id) {
        return existing.value().clone();
    }
    let registry = ProtectedCallRegistry::with_q1_defaults()
        .expect("the Q1 protected-call limits validate by construction");
    PROTECTED_CALL_REGISTRIES
        .entry(node_id)
        .or_insert_with(|| Arc::clone(&registry))
        .value()
        .clone()
}

/// Test/fixture seam: install a registry with explicit limits as THE
/// registry for `node_id` (replacing any current one — the replacement is
/// dropped, which drops its raise subscription). Used to drive the §2.7
/// byte witnesses with small budgets; production always uses the Q1
/// defaults.
#[cfg(any(test, feature = "fixtures"))]
pub fn set_protected_call_registry_for_node(
    node_id: u64,
    limits: CallLimits,
    byte_limits: ByteLimits,
) -> Result<Arc<ProtectedCallRegistry>, LimitsError> {
    let registry = ProtectedCallRegistry::with_limits(limits, byte_limits)?;
    PROTECTED_CALL_REGISTRIES.insert(node_id, Arc::clone(&registry));
    Ok(registry)
}

/// Test/fixture seam: the current registry for `node_id`, if any.
#[cfg(any(test, feature = "fixtures"))]
pub fn existing_protected_call_registry(node_id: u64) -> Option<Arc<ProtectedCallRegistry>> {
    PROTECTED_CALL_REGISTRIES
        .get(&node_id)
        .map(|r| r.value().clone())
}

/// The registry's Q1 call ceilings with TINY byte budgets (24 B per call
/// per direction) — the §2.7 accounting semantics run identically at
/// small numbers, and the byte witnesses park in bounded time.
#[cfg(any(test, feature = "fixtures"))]
pub fn tiny_byte_call_limits() -> CallLimits {
    CallLimits {
        max_active_node: 8,
        max_active_per_caller: 8,
        max_active_per_org: 8,
        verification_deadline_ns: 30 * 1_000_000_000,
    }
}

/// Test/fixture seam: tiny byte budgets for the §2.7 witnesses.
#[cfg(any(test, feature = "fixtures"))]
pub fn tiny_byte_byte_limits() -> ByteLimits {
    ByteLimits {
        per_call: 24,
        per_caller: 32,
        per_node: 64,
    }
}

/// mesh.rs hook — the store/authority install site (`install_org_revocation_store_locked`):
/// §2.3's second `subscribe_floors_raised` subscriber plus the
/// replacement semantics (retire every record captured under the old
/// `(authority_ptr, store_ptr)`; re-subscribe to the new store).
pub fn org_registry_store_installed(
    node_id: u64,
    authority: Option<Arc<NodeAuthority>>,
    store: Arc<OrgRevocationStore>,
) {
    let registry = protected_call_registry_for(node_id);
    registry.bind_store(authority, store);
}

/// mesh.rs hook — `install_peer_locked`'s displaced branch and the
/// dead-peer sweep: retire exactly the displaced session's records
/// (session replacement / disconnect).
pub fn org_registry_retire_session(
    node_id: u64,
    peer: u64,
    session_id: u64,
    establishment: Option<[u8; 32]>,
) {
    let Some(registry) = PROTECTED_CALL_REGISTRIES
        .get(&node_id)
        .map(|r| r.value().clone())
    else {
        return;
    };
    let session = SessionIdentity {
        peer,
        session_id,
        establishment,
    };
    registry.retire_session(&session, StreamTerminalReason::SessionReplaced);
}

/// mesh.rs hook — node shutdown (`Adapter::shutdown`): retire every live
/// protected record of this node. Q3/C9: node shutdown retires all
/// node-owned calls.
pub fn org_registry_retire_all(node_id: u64) {
    let Some(registry) = PROTECTED_CALL_REGISTRIES
        .get(&node_id)
        .map(|r| r.value().clone())
    else {
        return;
    };
    registry.retire_all(StreamTerminalReason::ServeHandleDropped);
}

/// mesh.rs hook — `Drop for MeshNode`: retire (best-effort, idempotent)
/// and DISENGAGE the node's registry so its raise subscription dies with
/// the node and a reused node id cannot inherit stale records.
pub fn org_registry_node_dropped(node_id: u64) {
    if let Some((_, registry)) = PROTECTED_CALL_REGISTRIES.remove(&node_id) {
        registry.retire_all(StreamTerminalReason::ServeHandleDropped);
    }
}

/// The §2.7/§2.4 registry linkage one protected call carries (the sink's
/// byte-accounting handle and the retire/complete identity — the same
/// `(registry, key, incarnation)` triple everywhere an exact-incarnation
/// operation is required).
#[derive(Clone)]
pub struct RegistryCallRef {
    /// The owning registry (byte budgets + commit boundary + records).
    pub registry: Arc<ProtectedCallRegistry>,
    /// The charged call.
    pub key: ProtectedCallKey,
    /// The exact incarnation.
    pub incarnation: u64,
}

impl std::fmt::Debug for RegistryCallRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryCallRef")
            .field("key", &self.key)
            .field("incarnation", &self.incarnation)
            .finish()
    }
}

impl RegistryCallRef {
    /// Latch `ResourceExhausted` and retire the call (§2.7: a refused
    /// protected item can never be dropped and then report success).
    fn latch_exhausted(&self) {
        self.registry.retire(
            &self.key,
            self.incarnation,
            StreamTerminalReason::ResourceExhausted,
        );
    }
}

/// The post-transfer state of a unary protected opening (slice 1.4). It
/// carries the cancellation token the retire hook fires and completes the
/// registry record on drop — §2.4's single removal, owned post-transfer by
/// the supervisor ("after disposition of owned work"). For a unary record
/// that supervisor is the spawned handler task; if the opening is refused
/// at the effect boundary after the transfer, this scope guard stands in
/// for it so no `Running` record is ever orphaned.
struct ConfirmedOpening {
    call_ref: RegistryCallRef,
    cancellation: RpcCancellationToken,
}

impl Drop for ConfirmedOpening {
    fn drop(&mut self) {
        self.call_ref
            .registry
            .complete(&self.call_ref.key, self.call_ref.incarnation);
    }
}

/// The unary fold's admitted-opening carrier (`apply_frame`'s
/// `org_admission` slot): the verified attribution plus, when a registry
/// lease rode in, the post-transfer confirmation.
struct AdmittedOpening {
    admitted: crate::adapter::net::behavior::org_admission::Admitted,
    confirmed: Option<ConfirmedOpening>,
}

use std::future::Future;

/// The handler shape one §2.2-supervised call runs (Stage 2 slice 2.2
/// extends the seam to duplex). The supervisor owns "the handler future"
/// (§2.2's table) for every pumping shape alike; only the invocation
/// differs — server-streaming takes `RpcContext` + sink, duplex takes
/// `RpcStreamingContext` + request stream + sink. Client-streaming has no
/// pump (its bounded single-response emission IS the drain-complete
/// event) and runs [`run_client_stream_call`] instead.
enum SupervisedHandler {
    /// Server-streaming: one REQUEST in, many RESPONSE chunks out.
    ServerStreaming(Arc<dyn RpcStreamingHandler>, RpcContext),
    /// Duplex: request stream in, RESPONSE chunks out.
    Duplex(
        Arc<dyn RpcDuplexHandler>,
        RpcStreamingContext,
        RequestStream,
    ),
}

impl SupervisedHandler {
    /// The call's cancellation token (both context shapes carry one).
    fn cancellation(&self) -> RpcCancellationToken {
        match self {
            Self::ServerStreaming(_, ctx) => ctx.cancellation.clone(),
            Self::Duplex(_, ctx, _) => ctx.cancellation.clone(),
        }
    }

    /// Build the owned handler future against the supervisor-built sink.
    /// The async block OWNS the handler and context so the future is
    /// self-contained (`async_trait` futures borrow their `&self`).
    fn into_future(
        self,
        sink: RpcResponseSink,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), RpcHandlerError>> + Send>> {
        match self {
            Self::ServerStreaming(handler, ctx) => {
                Box::pin(async move { handler.call(ctx, sink).await })
            }
            Self::Duplex(handler, ctx, requests) => {
                Box::pin(async move { handler.call(ctx, requests, sink).await })
            }
        }
    }
}

/// Run one server-streaming call to a bounded terminal (§2.2) — the
/// production shape of the model's `run_supervisor`. The supervisor owns
/// the handler, the response pump's `JoinHandle`, the flow semaphore and
/// the single terminal emission, and stays in its `select!` after the
/// handler returns: **producer finished is not terminal**. It exits only
/// when the pump has stopped — because it drained, or because retirement
/// closed its semaphore / aborted it. Retirement therefore bounds a
/// drain the caller never credits; the drain never bounds retirement.
///
/// The §2.2 retirement order is exact: signal the handler's cancellation
/// token, close the flow semaphore (unparking a pump stalled on credit),
/// `abort()` + `await` the pump so no chunk can be published after the
/// terminal, select the terminal (first writer wins), and emit exactly
/// one terminal AFTER the pump stopped. Queued-data policy (§2.2's
/// table): only `Completed(_)` drains, every retirement discards the
/// remaining queue with the aborted receiver.
///
/// `deadline` is `(monotonic end, expiry reason)` — `Timeout` for a
/// §2.1 `Deadline` bound, `CredentialExpired` for the `Credential`
/// clamp. `retire`/`gate` are `Some` for PROTECTED records (the §2.2
/// supervisor contract) and `None` for public calls, whose documented
/// behavior is preserved apart from the shared Q3 repairs (C6 nonzero
/// `deadline_ns` enforcement, C7 `Timeout` classification — the latter
/// rides `cancel_wins == false` + the `Timeout` reason). `cancel_wins`
/// keeps the PUBLIC fold's documented CANCEL-wins terminal override;
/// protected records follow §2.6's first-writer-wins instead.
///
/// (The model's byte-permit semaphore lands with slice 1.4's §2.7 byte
/// accounting; this supervisor closes the semaphores the call owns.)
#[expect(
    clippy::too_many_arguments,
    reason = "the §2.2 supervisor-owned pieces are named one-for-one (record, handler, \
              context, metrics, flow semaphore, producer gate, retire signal, deadline, \
              cancel policy, emitter, call identity, registration) and a params struct \
              would only rename them and hide the model mapping"
)]
async fn run_stream_call_supervisor(
    record: Arc<Mutex<StreamCallRecord>>,
    handler: SupervisedHandler,
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
    flow_sem: Option<Arc<tokio::sync::Semaphore>>,
    gate: Option<Arc<StreamProducerGate>>,
    retire: Option<Arc<StreamRetireSignal>>,
    deadline: Option<(tokio::time::Instant, StreamTerminalReason)>,
    cancel_wins: bool,
    emit: RpcAsyncResponseEmitter,
    identity: (u64, u64, u64),
    registration: StreamCallRegistration,
) {
    let (from_node, caller_origin, call_id) = identity;

    if let Some(m) = metrics.as_ref() {
        m.handler_invocations_total.fetch_add(1, Ordering::Relaxed);
        m.handler_in_flight.fetch_add(1, Ordering::Relaxed);
    }
    let handler_started = std::time::Instant::now();
    let cancel_token = handler.cancellation();

    // The sink + pump pair (§2.2). Bounded at
    // STREAMING_PUMP_CAPACITY. The pump pays one flow-control credit
    // per chunk when the caller opted in; a CLOSED semaphore (retirement)
    // unparks it (`Err(_) => break`).
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_PUMP_CAPACITY);
    let sink = RpcResponseSink {
        inner: tx,
        metrics: metrics.clone(),
        gate: gate.clone(),
        byte_charge: registration.registry.clone(),
    };
    let pump_emit = emit.clone();
    let pump_metrics = metrics.clone();
    let pump_flow = flow_sem.clone();
    let pump_gate = gate.clone();
    let pump_task = tokio::spawn(async move {
        loop {
            // After the producer half closes (protected only), drain
            // what is already admitted and stop — never wait for another
            // send the gate has just made impossible.
            let next = match pump_gate.as_ref() {
                Some(g) if g.is_finished() => rx.try_recv().ok(),
                Some(g) => tokio::select! {
                    item = rx.recv() => item,
                    () = g.woken.notified() => continue,
                },
                None => rx.recv().await,
            };
            let Some(chunk) = next else { break };
            if let Some(sem) = pump_flow.as_ref() {
                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    // Semaphore closed by retirement: stop publishing.
                    Err(_) => break,
                };
                permit.forget();
            }
            if let Some(m) = pump_metrics.as_ref() {
                m.streaming_chunks_emitted_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            let resp = RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![(
                    HEADER_NRPC_STREAMING.to_string(),
                    HEADER_NRPC_STREAMING_CONTINUE.to_vec(),
                )],
                body: chunk.body,
            };
            let chunk_permit = chunk.permit;
            // Await per-chunk publish so chunks for one call_id reach
            // the network in send order.
            pump_emit(from_node, caller_origin, call_id, resp).await;
            // §2.7: publishing releases the item's byte reservation —
            // the bytes left the queue, they were not merely counted.
            if let Some(shared) = chunk_permit {
                if let Some(permit) = shared.take() {
                    permit.release();
                }
            }
        }
    });
    tokio::pin!(pump_task);

    // The handler future, panics caught so a misbehaving handler can't
    // take down the runtime — same shape as the unary fold.
    let mut handler_fut = std::pin::pin!(futures::FutureExt::catch_unwind(
        std::panic::AssertUnwindSafe(handler.into_future(sink))
    ));
    let mut handler_done = false;
    let mut handler_panicked = false;
    let mut pump_done = false;
    let mut pump_failed = false;

    let sleep = async {
        match deadline {
            Some((at, reason)) => {
                tokio::time::sleep_until(at).await;
                reason
            }
            // No deadline arm (public `deadline_ns == 0`): park forever
            // rather than fire immediately.
            None => std::future::pending().await,
        }
    };
    let mut sleep = std::pin::pin!(sleep);

    // The persistent select! (§2.2): handler / pump / sleep_until /
    // retire — and it STAYS in the loop after the handler returns.
    let forced: Option<StreamTerminalReason> = loop {
        if pump_done {
            break None;
        }
        tokio::select! {
            biased;

            reason = async { match retire.as_ref() { Some(r) => r.wait().await, None => std::future::pending().await } } => {
                break Some(reason)
            }

            reason = &mut sleep => break Some(reason),

            result = &mut handler_fut, if !handler_done => {
                handler_done = true;
                let result = match result {
                    Ok(Ok(())) => StreamHandlerResult::Ok,
                    Ok(Err(RpcHandlerError::Application { code, message })) => {
                        StreamHandlerResult::Err(RpcStatus::Application(code), message)
                    }
                    Ok(Err(RpcHandlerError::Internal(message))) => {
                        StreamHandlerResult::Err(RpcStatus::Internal, message)
                    }
                    Err(panic) => {
                        handler_panicked = true;
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
                        StreamHandlerResult::Err(
                            RpcStatus::Internal,
                            format!("handler panicked: {panic_msg}"),
                        )
                    }
                };
                // Producer finished: Draining, NOT terminal. The gate
                // closes the sink so a retained clone cannot extend the
                // drain, while grants stay creditable and expiry stays
                // armed (§2.2 / §2.6).
                record.lock().handler_returned(result);
                if let Some(g) = gate.as_ref() {
                    g.finish();
                }
            }

            joined = &mut pump_task, if !pump_done => {
                pump_done = true;
                pump_failed = joined.is_err();
            }
        }
    };

    if let Some(m) = metrics.as_ref() {
        m.handler_in_flight.fetch_sub(1, Ordering::Relaxed);
        m.record_handler_duration(handler_started.elapsed());
        if handler_panicked {
            m.handler_panics_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    // The PUBLIC fold's documented CANCEL-wins terminal override samples
    // the token BEFORE this teardown signals it (below) — only a cancel
    // that fired during handler execution wins, never the supervisor's
    // own retirement signal (Q3 preserved this public behavior; C5–C8
    // are the approved public changes).
    let cancelled_by_caller = cancel_wins && cancel_token.is_cancelled();

    // Retirement path (§2.2's order): signal the handler's cancellation,
    // close the semaphore so a parked pump errors out, then abort AND
    // join it — the join is what establishes "no chunk is published
    // after the terminal", not the abort call. The owned handler future
    // drops at scope end.
    if let Some(reason) = forced {
        // §2.4: settle the registry side first (terminal + queued byte
        // permits + owner signal), so no item can commit after the
        // retirement lands — then the §2.6 record.
        if let Some(call_ref) = registration.registry.as_ref() {
            call_ref
                .registry
                .retire(&call_ref.key, call_ref.incarnation, reason.clone());
        }
        record.lock().retire(reason);
        cancel_token.cancel();
        if let Some(sem) = flow_sem.as_ref() {
            sem.close();
        }
        if !pump_done {
            pump_task.as_mut().abort();
            let _ = pump_task.as_mut().await;
        }
    } else if pump_failed {
        record.lock().retire(StreamTerminalReason::PumpFailed);
    } else {
        // Clean pump exit: the state machine decides whether that is a
        // completion (Draining) or a producer that died under the
        // handler (PumpFailed).
        record.lock().pump_exited();
    }

    let reason = record
        .lock()
        .terminal_reason()
        .unwrap_or(StreamTerminalReason::PumpFailed);
    let reason = if cancelled_by_caller {
        StreamTerminalReason::Cancelled
    } else {
        reason
    };

    // Single removal point (§2.4's `complete`): the in-flight token,
    // the flow semaphore and the protected record leave together, so a
    // stale GRANT/CANCEL after this point misses every map.
    registration.complete();

    // §2.8 — exactly one terminal, AFTER pump stop, handed to the
    // response-emitter seam. The handoff is non-blocking — the model's
    // control-path `try_send`, not an unbounded transport wait ("bound
    // all library-controlled waits… rather than hang"; "network receipt
    // is a separate observation"): `Queued` records that the control
    // path TOOK the job — not peer receipt — and ownership completes
    // here whether or not the transport can deliver. The `Sent` /
    // `Refused` / `Unreachable` dispositions become distinguishable at
    // the bounded `RpcResponseJob` drainer / route layer (slice 1.5's
    // seam). Pump stop already ordered this behind every chunk emit.
    let terminal = stream_terminal_payload(&reason);
    tokio::spawn(emit(from_node, caller_origin, call_id, terminal));
    record
        .lock()
        .record_emission(StreamTerminalDisposition::Queued);
}

/// Run one PROTECTED client-streaming call to its bounded single-response
/// emission (§2.2: "Client-streaming has a single-response emitter, not an
/// SS/DX pump: its bounded emission completion supplies the corresponding
/// drain-complete event"). The supervisor owns the handler future, the
/// request-chunk queue and the ONE terminal: `select!` over {handler,
/// `sleep_until` the §2.1 effective end, retire}, then the §2.6 record
/// drives the transitions — handler return ⇒ `Draining(result)` AND input
/// `Closed` (the consumer is gone), and the single-response emission is
/// the drain-complete event ⇒ `pump_exited` ⇒ `Completed(result)`; a
/// forced exit (deadline / retire) commits the reason FIRST-WRITER-WINS,
/// cancels the handler token and discards queued input with the dropped
/// receiver. The handler's own payload is the wire terminal verbatim —
/// its error is the terminal, never `Ok` (§2.6) — while every retirement
/// maps through [`stream_terminal_payload`].
#[expect(
    clippy::too_many_arguments,
    reason = "the §2.2 supervisor-owned pieces are named one-for-one (record, handler, \
              context, request stream, metrics, retire signal, deadline, emitter, call \
              identity, registration) and a params struct would only rename them"
)]
async fn run_client_stream_call(
    record: Arc<Mutex<StreamCallRecord>>,
    handler: Arc<dyn RpcClientStreamingHandler>,
    ctx: RpcStreamingContext,
    requests: RequestStream,
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
    retire: Option<Arc<StreamRetireSignal>>,
    deadline: Option<(tokio::time::Instant, StreamTerminalReason)>,
    emit: RpcResponseEmitter,
    identity: (u64, u64, u64, u64),
    registration: StreamCallRegistration,
) {
    let (from_node, session_id, caller_origin, call_id) = identity;

    if let Some(m) = metrics.as_ref() {
        m.handler_invocations_total.fetch_add(1, Ordering::Relaxed);
        m.handler_in_flight.fetch_add(1, Ordering::Relaxed);
    }
    let handler_started = std::time::Instant::now();
    let cancel_token = ctx.cancellation.clone();
    // Panics caught so a misbehaving handler can't take down the runtime —
    // same shape as the SS supervisor and the public CS fold.
    let call_fut =
        futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(handler.call(ctx, requests)));
    tokio::pin!(call_fut);

    let sleep = async {
        match deadline {
            Some((at, reason)) => {
                tokio::time::sleep_until(at).await;
                reason
            }
            None => std::future::pending().await,
        }
    };
    tokio::pin!(sleep);

    let mut handler_panicked = false;
    let mut outcome_payload: Option<RpcResponsePayload> = None;
    let mut forced: Option<StreamTerminalReason> = None;
    tokio::select! {
        biased;

        reason = async { match retire.as_ref() { Some(r) => r.wait().await, None => std::future::pending().await } } => {
            forced = Some(reason);
        }

        reason = &mut sleep => {
            forced = Some(reason);
        }

        result = &mut call_fut => {
            // §2.6: handler return ⇒ `Draining(result)` with the input
            // half CLOSED (its consumer is gone) — then the single-response
            // emission below is the drain-complete event (`pump_exited` ⇒
            // `Completed(result)`).
            let (result, payload) = match result {
                Ok(Ok(payload)) => {
                    // The handler's OWN response is the terminal, verbatim.
                    // A non-`Ok` status on it is the handler's error
                    // terminal (§2.6: "the handler's error is the terminal,
                    // never `Ok`").
                    let result = if payload.status.is_ok() {
                        StreamHandlerResult::Ok
                    } else {
                        StreamHandlerResult::Err(
                            payload.status,
                            String::from_utf8_lossy(&payload.body).into_owned(),
                        )
                    };
                    (result, payload)
                }
                Ok(Err(RpcHandlerError::Application { code, message })) => {
                    let result = StreamHandlerResult::Err(RpcStatus::Application(code), message);
                    let payload =
                        stream_terminal_payload(&StreamTerminalReason::Completed(result.clone()));
                    (result, payload)
                }
                Ok(Err(RpcHandlerError::Internal(message))) => {
                    let result =
                        StreamHandlerResult::Err(RpcStatus::Internal, message);
                    let payload =
                        stream_terminal_payload(&StreamTerminalReason::Completed(result.clone()));
                    (result, payload)
                }
                Err(panic) => {
                    handler_panicked = true;
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
                    let result = StreamHandlerResult::Err(
                        RpcStatus::Internal,
                        format!("handler panicked: {panic_msg}"),
                    );
                    let payload =
                        stream_terminal_payload(&StreamTerminalReason::Completed(result.clone()));
                    (result, payload)
                }
            };
            {
                let mut rec = record.lock();
                rec.handler_returned(result);
                rec.pump_exited();
            }
            outcome_payload = Some(payload);
        }
    }

    if let Some(m) = metrics.as_ref() {
        m.handler_in_flight.fetch_sub(1, Ordering::Relaxed);
        m.record_handler_duration(handler_started.elapsed());
        if handler_panicked {
            m.handler_panics_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    if let Some(reason) = forced {
        // §2.2's retirement order: settle the registry side first (terminal
        // + queued byte permits + owner signal), then the §2.6 record
        // (first writer wins), then the handler's cancellation token. The
        // handler future is dropped at scope end; its library-controlled
        // input half is fenced NOW (the retire-aware `RequestStream` and
        // the registration's sender removal) and queued input is discarded
        // with the dropped receiver.
        if let Some(call_ref) = registration.registry.as_ref() {
            call_ref
                .registry
                .retire(&call_ref.key, call_ref.incarnation, reason.clone());
        }
        record.lock().retire(reason);
        cancel_token.cancel();
    }

    let reason = record
        .lock()
        .terminal_reason()
        .unwrap_or(StreamTerminalReason::PumpFailed);

    // §2.4's single removal point (the in-flight token, the sender and the
    // protected record leave together, so a stale CHUNK/END/CANCEL after
    // this point misses every map).
    registration.complete();

    // §2.8 — exactly one terminal, after handler/queue ownership ended.
    let payload = outcome_payload.unwrap_or_else(|| stream_terminal_payload(&reason));
    emit(from_node, session_id, caller_origin, call_id, payload);
    record
        .lock()
        .record_emission(StreamTerminalDisposition::Queued);
}

/// Server-side fold for streaming RPC. Parallel to `RpcServerFold`
/// but multi-fire emit: each handler invocation may produce many
/// `RESPONSE` events for the same `call_id`, marked
/// non-terminal/terminal via the `nrpc-streaming` header.
///
/// State `()` — like the unary fold, the handler owns user state
/// via captured `Arc<Mutex<S>>`. The fold's own state (in-flight
/// cancellation tokens) lives on `&mut self`.
pub struct RpcServerStreamingFold {
    /// R2-A/C5: the receiving incarnation of the frame currently being
    /// applied, set by `apply_inbound*` from the event. Part of every
    /// per-call key below; `0` on test/loopback paths, like `from_node`.
    session_id: u64,
    handler: Arc<dyn RpcStreamingHandler>,
    emit: RpcAsyncResponseEmitter,
    /// (from_node, receiving_session_id, caller_origin, call_id) →
    /// cancellation token — session-fenced so a forged CANCEL can't
    /// cancel another peer's stream (AV-1 item 1) and a late frame from
    /// a replaced session misses the map entirely (C5).
    in_flight: InFlightCalls,
    /// Per-call flow-control semaphore (when the caller opted in).
    /// `Some(sem)` means "pump must `acquire().await` one permit
    /// per chunk before emitting; STREAM_GRANT events
    /// `add_permits(n)`". Absence of an entry for a key means
    /// unbounded credit (no flow control — pump emits as fast as the
    /// publish path can take chunks).
    flow_control: FlowControlMap,
    /// The registration's live PROTECTED calls (§2.2). Public calls
    /// never enter this set; `ServeHandle::drop` retires exactly these
    /// (Q3). Empty until the protected admitted entry point is fed.
    protected_calls: ProtectedStreamOwners,
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
            session_id: 0,
            handler,
            emit,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            flow_control: Arc::new(Mutex::new(HashMap::new())),
            protected_calls: ProtectedStreamOwners::new(),
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

    /// Test-only: snapshot of the in-flight call set (four-part keys —
    /// `(from_node, session_id, origin, call_id)`).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn in_flight_keys(&self) -> Vec<StreamCallKey> {
        self.in_flight.lock().keys().copied().collect()
    }

    /// Test-only: available flow-control permits for a call key, or
    /// `None` if no per-call semaphore is installed. Lets the AV-1
    /// STREAM_GRANT-hijack witness prove a forged grant from a foreign
    /// session does not refill the victim's window.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn flow_control_permits(&self, key: StreamCallKey) -> Option<usize> {
        self.flow_control
            .lock()
            .get(&key)
            .map(|s| s.available_permits())
    }

    /// The registration-owned live PROTECTED calls (§2.2). The serve
    /// seam clones this into its `ServeHandle` so handle drop retires
    /// exactly these records (Q3).
    pub fn protected_owners(&self) -> ProtectedStreamOwners {
        self.protected_calls.clone()
    }
}

impl RpcServerStreamingFold {
    /// Production-path entry point. Keys per-call state (in-flight
    /// token + flow-control semaphore) by `(from_node,
    /// receiving_session_id, claimed_origin, call_id)` so a forged
    /// CANCEL / STREAM_GRANT from another peer misses the map and
    /// no-ops (AV-1 item 1) — and a late frame from a *replaced*
    /// session carrying the same `(from_node, origin, call_id)` misses
    /// it too (C5, public + protected).
    pub fn apply_inbound(&mut self, ev: &RpcInboundEvent) -> Result<(), RedexError> {
        self.session_id = ev.session_id;
        self.apply_frame(ev.from_node, &ev.payload)
    }

    /// Fold seam of frozen contract 4 — `apply_inbound_admitted(frame,
    /// lease)`. At slice 1.3 the `lease` slot carries the
    /// [`Admitted`](crate::adapter::net::behavior::org_admission::Admitted)
    /// facts: the exact-incarnation `AdmissionLease` of contract 3 lands
    /// with slice 1.4's registry, and the four-part key does the session
    /// fencing here.
    ///
    /// The PROTECTED opening path. §2.1's deadline resolution runs
    /// BEFORE any handler effect: the default fills only an omitted
    /// deadline, an explicit request over `max_live` is REFUSED (never
    /// clamped), and credential validity clamps with the `Deadline` vs
    /// `Credential` bound recorded. Every refusal is one typed
    /// [`AdmissionDenied`]
    /// the bridge routes through the unchanged `emit_admission_denial`
    /// — the fold emits NOTHING on refusal, so a denied opening has
    /// exactly one bounded denial and zero handler effects (no handler,
    /// no in-flight entry, no sender, no semaphore). On admission the
    /// raw `net-org-admission` proof header is stripped (E1.6) and the
    /// §2.2 supervisor is spawned owning the handler, the pump, the
    /// flow semaphore and the one terminal.
    ///
    /// A non-`DISPATCH_RPC_REQUEST` frame is a caller-logic error here
    /// (control frames for an admitted call ride [`Self::apply_inbound`]
    /// without re-admission) and is refused as
    /// `AdmissionDenied::NotOrgProtected`.
    pub fn apply_inbound_admitted(
        &mut self,
        ev: &RpcInboundEvent,
        admitted: crate::adapter::net::behavior::org_admission::Admitted,
        lifetime: &StreamCallLifetime<'_>,
        mut lease: Option<ProtectedCallLease>,
    ) -> Result<
        Arc<ProtectedStreamCall>,
        crate::adapter::net::behavior::org_admission::AdmissionDenied,
    > {
        use crate::adapter::net::behavior::org_admission::AdmissionDenied;

        self.session_id = ev.session_id;
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            return Err(AdmissionDenied::MalformedProof);
        };
        if meta.dispatch != DISPATCH_RPC_REQUEST {
            return Err(AdmissionDenied::NotOrgProtected);
        }
        let key = (
            ev.from_node,
            self.session_id,
            meta.origin_hash,
            meta.seq_or_ts,
        );
        // §3: a duplicate while the key is live is refused as
        // `ActiveCallOwned` BEFORE the payload decode.
        if self.in_flight.lock().contains_key(&key) {
            return Err(AdmissionDenied::ActiveCallOwned);
        }
        if ev.payload.len() < RPC_FRAME_BODY_OFFSET {
            return Err(AdmissionDenied::MalformedProof);
        }
        let Ok(mut payload) = RpcRequestPayload::decode(ev.payload.slice(RPC_FRAME_BODY_OFFSET..))
        else {
            return Err(AdmissionDenied::MalformedProof);
        };
        // The SS REQUEST flag check (contract 4): flags whose derived
        // shape is not server-streaming are `ShapeMismatch`, never
        // admitted.
        if !ss_request_flags_ok(payload.flags) {
            return Err(AdmissionDenied::ShapeMismatch);
        }
        // §2.1 — resolve before any handler effect. `deadline_ns == 0`
        // is an OMITTED deadline (the provider default fills it); any
        // nonzero value is an explicit request and is never capped by
        // the default. Over `max_live` ⇒ refused, never clamped.
        let requested = (payload.deadline_ns != 0).then_some(payload.deadline_ns);
        let resolved = resolve_stream_deadline(
            lifetime.clock.wall_ns,
            requested,
            lifetime.credential_ends_ns,
            &lifetime.policy,
        )
        .map_err(|refusal| {
            tracing::warn!(
                caller_origin = format!("{:#x}", meta.origin_hash),
                call_id = meta.seq_or_ts,
                ?refusal,
                "rpc streaming server fold: protected opening refused before handler effects",
            );
            AdmissionDenied::DeadlineExceedsPolicy
        })?;
        // E1.6: verified attribution in, raw credential material out.
        payload.headers.retain(|(name, _)| {
            name != crate::adapter::net::behavior::org_call::ORG_ADMISSION_HEADER
        });

        // §3 step 5 — the registry's ownership TRANSFER (slice 1.4),
        // before any fold effect (in-flight insert, sender creation,
        // handler spawn). `confirm` is the transfer, not a boolean check
        // followed by a spawn: a retire that wins before it prevents
        // every effect and the lease's Drop releases the reservation;
        // one that wins after reaches the registered signal even before
        // the supervisor task has been scheduled.
        let retire_signal = Arc::new(StreamRetireSignal::new());
        let mut call_ref: Option<RegistryCallRef> = None;
        if let Some(lease) = lease.as_mut() {
            let registry = Arc::clone(lease.registry());
            let ref_for_call = RegistryCallRef {
                registry: Arc::clone(&registry),
                key: lease.key.clone(),
                incarnation: lease.incarnation,
            };
            registry.confirm(lease, Arc::clone(&retire_signal), None)?;
            call_ref = Some(ref_for_call);
        }

        let cancellation = RpcCancellationToken::new();
        self.in_flight.lock().insert(key, cancellation.clone());
        let flow_sem = parse_stream_window_initial(&payload.headers).map(|n| {
            let sem = Arc::new(tokio::sync::Semaphore::new(n as usize));
            self.flow_control.lock().insert(key, sem.clone());
            sem
        });
        let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
            extract_trace_context(&payload.headers)
        } else {
            None
        };
        let ctx = RpcContext {
            caller_origin: meta.origin_hash,
            session_peer: ev.from_node,
            call_id: meta.seq_or_ts,
            payload,
            cancellation,
            trace_context,
            org_admission: Some(admitted),
        };
        let call = Arc::new(ProtectedStreamCall {
            record: Arc::new(Mutex::new(StreamCallRecord::new_server_streaming())),
            retire: retire_signal,
            deadline: resolved,
            call_ref: call_ref.clone(),
        });
        self.protected_calls.insert(key, call.clone());
        // The deadline's monotonic end derives from the admission's ONE
        // clock sample (`monotonic_deadline_for`), so a wall-clock jump
        // cannot move it.
        let at =
            tokio::time::Instant::from_std(lifetime.clock.monotonic_deadline_for(resolved.end_ns));
        tokio::spawn(run_stream_call_supervisor(
            Arc::clone(&call.record),
            SupervisedHandler::ServerStreaming(self.handler.clone(), ctx),
            self.metrics.clone(),
            flow_sem,
            Some(Arc::new(StreamProducerGate::new())),
            Some(Arc::clone(&call.retire)),
            Some((at, resolved.expiry_reason())),
            false,
            self.emit.clone(),
            (ev.from_node, meta.origin_hash, meta.seq_or_ts),
            StreamCallRegistration {
                key,
                in_flight: self.in_flight.clone(),
                flow_control: Some(self.flow_control.clone()),
                protected: self.protected_calls.clone(),
                registry: call_ref,
                senders: None,
            },
        ));
        Ok(call)
    }

    /// Core frame application shared by [`Self::apply_inbound`] (real
    /// `from_node`) and the [`RedexFold`] loopback shim (`0`).
    fn apply_frame(&mut self, from_node: u64, frame: &Bytes) -> Result<(), RedexError> {
        let Some(meta) = (if frame.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&frame[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = frame.len(),
                "rpc streaming server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (from_node, self.session_id, meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(frame.slice(RPC_FRAME_BODY_OFFSET..))
                {
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
                            emit(from_node, caller_origin, call_id, resp).await;
                        });
                        return Ok(());
                    }
                };
                // Caller-bug guard (contract 4 — unifying with the
                // CS/DX folds' flag checks): a server-streaming REQUEST
                // must carry flags whose derived shape is exactly
                // server-streaming (`FLAG_RPC_STREAMING_RESPONSE` set,
                // `FLAG_RPC_CLIENT_STREAMING_REQUEST` clear). Anything
                // else is refused cleanly before any call state exists.
                if !ss_request_flags_ok(payload.flags) {
                    tracing::warn!(
                        caller_origin = format!("{:#x}", meta.origin_hash),
                        call_id = meta.seq_or_ts,
                        flags = format!("{:#06x}", payload.flags),
                        "rpc streaming server fold: REQUEST flags are not server-streaming",
                    );
                    let resp = RpcResponsePayload {
                        status: RpcStatus::Internal,
                        headers: vec![(
                            HEADER_NRPC_STREAMING.to_string(),
                            HEADER_NRPC_STREAMING_END.to_vec(),
                        )],
                        body: Bytes::from_static(
                            b"REQUEST on a server-streaming service must set FLAG_RPC_STREAMING_RESPONSE \
                              and must not set FLAG_RPC_CLIENT_STREAMING_REQUEST",
                        ),
                    };
                    let emit = self.emit.clone();
                    let caller_origin = meta.origin_hash;
                    let call_id = meta.seq_or_ts;
                    tokio::spawn(async move {
                        emit(from_node, caller_origin, call_id, resp).await;
                    });
                    return Ok(());
                }
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
                            emit(from_node, caller_origin, call_id, resp).await;
                        });
                        return Ok(());
                    }
                }
                // Cancellation token + in-flight bookkeeping —
                // identical to the unary fold's pattern (C5: the key
                // carries the receiving incarnation).
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
                let caller_origin = meta.origin_hash;
                let call_id = meta.seq_or_ts;
                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let metrics = self.metrics.clone();
                let deadline_ns = payload.deadline_ns;
                let ctx = RpcContext {
                    caller_origin,
                    session_peer: from_node,
                    call_id,
                    payload,
                    cancellation,
                    trace_context,
                    // Streaming is never protected on this path (E1.8):
                    // protected records ride `apply_inbound_admitted`.
                    org_admission: None,
                };
                // Public calls run the same bounded call shape under the
                // Q3 shared repairs ONLY: a NONZERO `deadline_ns` is
                // enforced (C6) with a `Timeout` terminal (C7);
                // `deadline_ns == 0` keeps meaning "no deadline"; the
                // documented CANCEL-wins terminal override is preserved;
                // and there is no §2.2 retire signal / producer gate —
                // public calls retain their documented outstanding-call
                // and lossy-sink behavior (Q3 / C9).
                let deadline = (deadline_ns != 0).then(|| {
                    let now_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    let remaining = deadline_ns.saturating_sub(now_ns);
                    (
                        tokio::time::Instant::now() + std::time::Duration::from_nanos(remaining),
                        StreamTerminalReason::Timeout,
                    )
                });
                tokio::spawn(run_stream_call_supervisor(
                    Arc::new(Mutex::new(StreamCallRecord::new_server_streaming())),
                    SupervisedHandler::ServerStreaming(handler, ctx),
                    metrics,
                    flow_sem,
                    None,
                    None,
                    deadline,
                    true,
                    emit,
                    (from_node, caller_origin, call_id),
                    StreamCallRegistration {
                        key,
                        in_flight: self.in_flight.clone(),
                        flow_control: Some(self.flow_control.clone()),
                        protected: self.protected_calls.clone(),
                        registry: None,
                        senders: None,
                    },
                ));
            }
            DISPATCH_RPC_CANCEL => {
                // A PROTECTED record enters retirement through its
                // supervisor (§2.2/§2.6): the signal reaches the
                // supervisor's `select!`, and terminal selection + map
                // removal stay that one owner's work (§2.4's single
                // removal point), so a re-REQUEST under this key cannot
                // race the retired call's cleanup. The handler still
                // sees the cancellation token immediately.
                if let Some(call) = self.protected_calls.get(&key) {
                    if let Some(token) = self.in_flight.lock().get(&key).cloned() {
                        token.cancel();
                    }
                    call.retire(StreamTerminalReason::Cancelled);
                    return Ok(());
                }
                if let Some(token) = self.in_flight.lock().remove(&key) {
                    token.cancel();
                }
                // Also drop the flow-control entry — the supervisor's
                // terminal cleanup will run too, but doing
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
                let amount = match decode_stream_grant(&frame[RPC_FRAME_BODY_OFFSET..]) {
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
                    // Protected records classify the frame (§2.6):
                    // credit survives the handler's return — it is what
                    // lets a drain finish — and stops once the call is
                    // terminal or its output half has ended.
                    if let Some(call) = self.protected_calls.get(&key) {
                        if !call.record.lock().credit_grantable() {
                            return Ok(());
                        }
                    }
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

impl RedexFold<()> for RpcServerStreamingFold {
    /// Loopback / test shim: drives `apply_frame` with
    /// `from_node = 0` (AV-1 item 1). Production uses
    /// [`RpcServerStreamingFold::apply_inbound`].
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        self.apply_frame(0, &ev.payload)
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
/// `(from_node, receiving_session_id, caller_origin_hash, call_id)`
/// (AV-1 item 1, C5): the AEAD-authenticated last-hop session peer AND
/// the receiving incarnation are part of the key so a peer cannot push
/// a REQUEST_CHUNK into another peer's upload stream by copying its
/// origin + call_id, and a late chunk from a *replaced* session misses
/// the map entirely. Value is the bounded mpsc sender the fold's
/// `apply_frame()` pushes REQUEST_CHUNK bodies into. The matching
/// receiver lives inside the handler's [`RequestStream`]; dropping the
/// sender (on REQUEST_END or CANCEL) closes the stream.
type RequestChunkSenders = Arc<Mutex<HashMap<StreamCallKey, RequestChunkSender>>>;

/// One per-call request-chunk sender plus its optional §2.7
/// request-direction byte accounting (the charge rides the entry so a
/// non-deliverable chunk can retire the EXACT call). Protected CS/DX
/// records (Stage 2) insert `Some`; public uploads keep `None` and the
/// drop-and-continue contract.
#[derive(Clone)]
struct RequestChunkSender {
    tx: tokio::sync::mpsc::Sender<ChargedChunk>,
    charge: Option<RegistryCallRef>,
    /// §2.6's half-close record (protected CS/DX records only; Stage 2
    /// slices 2.2/2.4): END closes input ONCE (idempotent, never touching
    /// `output`), and a chunk is delivered only while the record's input
    /// half is `Open` and the call is not terminal. `None` on public
    /// uploads (their documented drop-and-continue contract is unchanged).
    record: Option<Arc<Mutex<StreamCallRecord>>>,
}

/// Deliver one request-direction body (the opening's first chunk or any
/// later REQUEST_CHUNK) into a PROTECTED call's queue under the §2.7 byte
/// accounting and the §2.3 check-and-commit as ONE ownership operation
/// (Stage 2 slices 2.2/2.4). Returns `false` when the item could neither
/// be reserved nor delivered (over budget, full mpsc, or a retired call):
/// the caller RETIRES the exact call with `ResourceExhausted`
/// (`RegistryCallRef::latch_exhausted`) and stops all further delivery —
/// never a silent drop followed by `Ok`.
fn deliver_protected_body(
    charge: &RegistryCallRef,
    tx: &tokio::sync::mpsc::Sender<ChargedChunk>,
    body: Bytes,
) -> bool {
    let body_len = body.len();
    let permit = match charge.registry.bytes().reserve(
        charge.key.clone(),
        charge.incarnation,
        ByteDirection::Request,
        body_len,
    ) {
        Ok(permit) => permit,
        Err(_) => return false,
    };
    // The queue slot is reserved first (a full/closed queue is a delivery
    // failure, not a wait), then the §2.3 check and the admission run as
    // ONE ownership operation under the registry lock.
    let slot = match tx.try_reserve() {
        Ok(slot) => slot,
        Err(_) => {
            permit.release();
            return false;
        }
    };
    match charge
        .registry
        .begin_commit(&charge.key, charge.incarnation)
    {
        Ok(txn) => {
            txn.commit_with(permit, |shared| {
                slot.send(ChargedChunk {
                    body,
                    permit: Some(Arc::clone(shared)),
                });
            });
            true
        }
        Err(_) => {
            permit.release();
            false
        }
    }
}

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
    from_node: u64,
    session_id: u64,
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
    // Scope the sender lookup to the authenticated session peer and its
    // receiving incarnation so a forged REQUEST_CHUNK carrying another
    // peer's origin + call_id misses the map (AV-1 item 1) — and a late
    // chunk from a replaced session cannot feed the successor (C5).
    let key = (from_node, session_id, meta.origin_hash, meta.seq_or_ts);
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
    // §2.6 (Stage 2 slices 2.2/2.4): a PROTECTED record's shape and
    // half-state gate delivery BEFORE anything is queued. Frames while the
    // call is terminal are dropped (no credit, no delivery); once the input
    // half is not `Open` (the caller's END closed it, or the handler
    // returned and its consumer is gone) a late chunk is refused/discarded
    // without replacing the handler's result — and an END can never reopen
    // the half (the sender is gone after the first END; this is the
    // record-level belt for a frame that somehow re-arrives).
    if let Some(record) = sender.record.as_ref() {
        let rec = record.lock();
        if rec.terminal.is_some() || rec.input != StreamCallInput::Open {
            return;
        }
    }
    let is_pure_terminator = is_end && payload.body.is_empty();
    if !is_pure_terminator {
        let body = payload.body;
        match sender.charge.as_ref() {
            // PUBLIC uploads keep the drop-and-continue contract (Q3).
            None => {
                if sender
                    .tx
                    .try_send(ChargedChunk { body, permit: None })
                    .is_err()
                {
                    tracing::debug!(
                        caller_origin = format!("{:#x}", meta.origin_hash),
                        call_id = meta.seq_or_ts,
                        tag = diag_tag,
                        "rpc server fold: request-chunk mpsc full or closed; dropping",
                    );
                }
            }
            // §2.7 request direction (protected records): account the
            // body BEFORE the queue/handler allocation (the shared
            // [`deliver_protected_body`]). A chunk that cannot be
            // reserved or delivered — over budget, full mpsc, or a closed
            // sender for an admitted call — RETIRES the call with
            // `ResourceExhausted` and stops all further delivery. Never a
            // silent drop followed by `Ok`.
            Some(charge) => {
                if !deliver_protected_body(charge, &sender.tx, body) {
                    charge.latch_exhausted();
                    senders.lock().remove(&key);
                    return;
                }
            }
        }
    }
    if is_end {
        // §2.6: END closes the input half ONCE — idempotent, never
        // touching `output` (legitimate remaining output completes
        // unaffected).
        if let Some(record) = sender.record.as_ref() {
            record.lock().end_input();
        }
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
    /// R2-A: the receiving incarnation of the frame being applied.
    session_id: u64,
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
    /// (from_node, caller_origin, call_id) → cancellation token —
    /// authenticated-peer-scoped (AV-1 item 1).
    in_flight: InFlightCalls,
    senders: RequestChunkSenders,
    /// The registration's live PROTECTED calls (§2.2 — Stage 2 slice 2.2
    /// extends the seam to this fold). Public calls never enter this set;
    /// `ServeHandle::drop` retires exactly these (Q3).
    protected_calls: ProtectedStreamOwners,
    /// Optional per-service metrics handle. Same shape as
    /// the other folds. Reuses the response-side counters where they
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
            session_id: 0,
            handler,
            emit,
            grant_emit: None,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            protected_calls: ProtectedStreamOwners::new(),
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

    /// Test-only: snapshot of the in-flight call set (four-part keys —
    /// `(from_node, session_id, origin, call_id)`, C5).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn in_flight_keys(&self) -> Vec<StreamCallKey> {
        self.in_flight.lock().keys().copied().collect()
    }

    /// Test-only: snapshot of the in-flight per-call senders.
    /// Useful for tests that need to assert a call's sender has
    /// been dropped after REQUEST_END / CANCEL.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn sender_keys(&self) -> Vec<StreamCallKey> {
        self.senders.lock().keys().copied().collect()
    }

    /// The registration-owned live PROTECTED calls (§2.2) — the serve
    /// seam clones this into its `ServeHandle` so handle drop retires
    /// exactly these records (Q3).
    pub fn protected_owners(&self) -> ProtectedStreamOwners {
        self.protected_calls.clone()
    }
}

impl RpcStreamingRequestFold {
    /// Production-path entry point. Keys per-call state (in-flight
    /// token + request-chunk sender) by `(from_node, claimed_origin,
    /// call_id)` so a forged REQUEST_CHUNK / CANCEL from another peer
    /// misses the map and no-ops (AV-1 item 1).
    pub fn apply_inbound(&mut self, ev: &RpcInboundEvent) -> Result<(), RedexError> {
        self.session_id = ev.session_id;
        self.apply_frame(ev.from_node, &ev.payload)
    }

    /// Core frame application shared by [`Self::apply_inbound`] (real
    /// `from_node`) and the [`RedexFold`] loopback shim (`0`).
    fn apply_frame(&mut self, from_node: u64, frame: &Bytes) -> Result<(), RedexError> {
        let Some(meta) = (if frame.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&frame[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = frame.len(),
                "rpc client-streaming server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (from_node, self.session_id, meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(frame.slice(RPC_FRAME_BODY_OFFSET..))
                {
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
                        (self.emit)(
                            from_node,
                            self.session_id,
                            meta.origin_hash,
                            meta.seq_or_ts,
                            resp,
                        );
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
                    (self.emit)(
                        from_node,
                        self.session_id,
                        meta.origin_hash,
                        meta.seq_or_ts,
                        resp,
                    );
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
                        (self.emit)(
                            from_node,
                            self.session_id,
                            meta.origin_hash,
                            meta.seq_or_ts,
                            resp,
                        );
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
                    tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_REQUEST_PUMP_CAPACITY);
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
                    if tx
                        .try_send(ChargedChunk {
                            body: payload.body,
                            permit: None,
                        })
                        .is_err()
                    {
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
                    self.senders.lock().insert(
                        key,
                        RequestChunkSender {
                            tx,
                            charge: None,
                            record: None,
                        },
                    );
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
                let request_stream = RequestStream::new(
                    rx,
                    grant_emitter,
                    from_node,
                    meta.origin_hash,
                    meta.seq_or_ts,
                );
                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let deadline_ns = payload.deadline_ns;
                let ctx = RpcStreamingContext::new(
                    meta.origin_hash,
                    meta.seq_or_ts,
                    deadline_ns,
                    payload.headers,
                    cancellation.clone(),
                    trace_context,
                );
                let handler = self.handler.clone();
                let emit = self.emit.clone();
                let in_flight = self.in_flight.clone();
                let senders = self.senders.clone();
                let session_id = self.session_id;
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
                    // C7 (Q3): a deadline expiry's terminal is typed
                    // `Timeout` — recorded here so the terminal
                    // selection below can distinguish the deadline's own
                    // cancel signal from a caller CANCEL (which keeps
                    // its documented CANCEL-wins override).
                    let mut deadline_expired: Option<String> = None;
                    let outcome = if deadline_ns > 0 {
                        let now_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);
                        let remaining = deadline_ns.saturating_sub(now_ns);
                        if remaining == 0 {
                            if !cancel_for_deadline.is_cancelled() {
                                deadline_expired = Some(
                                    "handler deadline_ns already expired at spawn".to_string(),
                                );
                            }
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
                                    if !cancel_for_deadline.is_cancelled() {
                                        deadline_expired =
                                            Some("handler deadline_ns exceeded".to_string());
                                    }
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
                    // handler's terminal with Cancelled. C7 (Q3): a
                    // deadline expiry that no caller CANCEL preceded is
                    // typed `Timeout`.
                    let terminal = if let Some(message) = deadline_expired {
                        RpcResponsePayload {
                            status: RpcStatus::Timeout,
                            headers: vec![],
                            body: Bytes::from(message),
                        }
                    } else if cancel_probe.is_cancelled() {
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
                    (emit)(from_node, session_id, caller_origin, call_id, terminal);
                });
            }
            DISPATCH_RPC_REQUEST_CHUNK => {
                apply_request_chunk_to_senders(
                    from_node,
                    self.session_id,
                    frame.slice(RPC_FRAME_BODY_OFFSET..),
                    &meta,
                    &self.senders,
                    "client-streaming",
                );
            }
            DISPATCH_RPC_CANCEL => {
                // A PROTECTED record enters retirement through its
                // supervisor (§2.2/§2.6 — Stage 2 slice 2.2): the signal
                // reaches the supervisor's `select!`, and terminal
                // selection + map removal stay that one owner's work
                // (§2.4's single removal point). The handler still sees
                // the cancellation token immediately.
                if let Some(call) = self.protected_calls.get(&key) {
                    if let Some(token) = self.in_flight.lock().get(&key).cloned() {
                        token.cancel();
                    }
                    call.retire(StreamTerminalReason::Cancelled);
                    return Ok(());
                }
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

impl RpcStreamingRequestFold {
    /// The PROTECTED opening path (contract 4's fold seam — Stage 2 slice
    /// 2.2 extends it to client-streaming). The transaction shape is the
    /// SS seam's verbatim: §2.1's deadline resolution BEFORE any handler
    /// effect, the raw proof header stripped (E1.6), §3 step-5's ownership
    /// TRANSFER at the effect boundary — then
    /// [`run_client_stream_call`] owns the handler, the request-chunk
    /// queue and the ONE single-response terminal (§2.2's bounded
    /// supervision). Every refusal is one typed
    /// [`AdmissionDenied`]
    /// the bridge routes through the unchanged `emit_admission_denial`;
    /// the fold emits NOTHING on refusal, so a denied opening has exactly
    /// one bounded denial and zero handler effects.
    ///
    /// A non-`DISPATCH_RPC_REQUEST` frame is refused as `NotOrgProtected`
    /// (control frames for an admitted call ride [`Self::apply_inbound`]
    /// without re-admission) and is keyed by the authenticated session
    /// peer + receiving incarnation.
    pub fn apply_inbound_admitted(
        &mut self,
        ev: &RpcInboundEvent,
        admitted: crate::adapter::net::behavior::org_admission::Admitted,
        lifetime: &StreamCallLifetime<'_>,
        mut lease: Option<ProtectedCallLease>,
    ) -> Result<
        Arc<ProtectedStreamCall>,
        crate::adapter::net::behavior::org_admission::AdmissionDenied,
    > {
        use crate::adapter::net::behavior::org_admission::AdmissionDenied;

        self.session_id = ev.session_id;
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            return Err(AdmissionDenied::MalformedProof);
        };
        if meta.dispatch != DISPATCH_RPC_REQUEST {
            return Err(AdmissionDenied::NotOrgProtected);
        }
        let key = (
            ev.from_node,
            self.session_id,
            meta.origin_hash,
            meta.seq_or_ts,
        );
        // §3: a duplicate while the key is live is refused as
        // `ActiveCallOwned` BEFORE the payload decode.
        if self.in_flight.lock().contains_key(&key) {
            return Err(AdmissionDenied::ActiveCallOwned);
        }
        if ev.payload.len() < RPC_FRAME_BODY_OFFSET {
            return Err(AdmissionDenied::MalformedProof);
        }
        let Ok(mut payload) = RpcRequestPayload::decode(ev.payload.slice(RPC_FRAME_BODY_OFFSET..))
        else {
            return Err(AdmissionDenied::MalformedProof);
        };
        // The CS REQUEST flag check (contract 4 / §1.5): flags whose
        // derived shape is not client-streaming are `ShapeMismatch`, never
        // admitted.
        if !cs_request_flags_ok(payload.flags) {
            return Err(AdmissionDenied::ShapeMismatch);
        }
        // §2.1 — resolve before any handler effect (the SS seam's exact
        // resolution): the default fills only an omitted deadline, an
        // explicit request over `max_live` is REFUSED (never clamped), and
        // credential validity clamps with the `Deadline` vs `Credential`
        // bound recorded. Every refusal here is
        // `DeadlineExceedsPolicy`.
        let requested = (payload.deadline_ns != 0).then_some(payload.deadline_ns);
        let resolved = resolve_stream_deadline(
            lifetime.clock.wall_ns,
            requested,
            lifetime.credential_ends_ns,
            &lifetime.policy,
        )
        .map_err(|refusal| {
            tracing::warn!(
                caller_origin = format!("{:#x}", meta.origin_hash),
                call_id = meta.seq_or_ts,
                ?refusal,
                "rpc client-streaming server fold: protected opening refused before handler effects",
            );
            AdmissionDenied::DeadlineExceedsPolicy
        })?;
        // E1.6: verified attribution in, raw credential material out.
        payload.headers.retain(|(name, _)| {
            name != crate::adapter::net::behavior::org_call::ORG_ADMISSION_HEADER
        });

        // §3 step 5 — the registry's ownership TRANSFER before any fold
        // effect (in-flight insert, sender creation, handler spawn).
        let retire_signal = Arc::new(StreamRetireSignal::new());
        let mut call_ref: Option<RegistryCallRef> = None;
        if let Some(lease) = lease.as_mut() {
            let registry = Arc::clone(lease.registry());
            let ref_for_call = RegistryCallRef {
                registry: Arc::clone(&registry),
                key: lease.key.clone(),
                incarnation: lease.incarnation,
            };
            registry.confirm(lease, Arc::clone(&retire_signal), None)?;
            call_ref = Some(ref_for_call);
        }

        let record = Arc::new(Mutex::new(StreamCallRecord::new_client_streaming()));
        let cancellation = RpcCancellationToken::new();
        self.in_flight.lock().insert(key, cancellation.clone());
        // Per-call request-chunk mpsc (the public arm's shape) with the
        // §2.7 charge + §2.6 record on the sender for protected uploads.
        let (tx, rx) = tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_REQUEST_PUMP_CAPACITY);
        let end_on_initial = payload.flags & FLAG_RPC_REQUEST_END != 0;
        let is_pure_terminator = end_on_initial && payload.body.is_empty();
        if !is_pure_terminator {
            // §2.7 ("refuse the call, never truncate it"): the OPENING
            // body reserves its bytes and is delivered under the same
            // check-and-commit as any chunk. A refusal retires the exact
            // call (`ResourceExhausted`) and refuses the opening with zero
            // further delivery.
            match call_ref.as_ref() {
                Some(charge) => {
                    if !deliver_protected_body(charge, &tx, payload.body.clone()) {
                        charge.latch_exhausted();
                        // Ownership already transferred (§3 step 5) but no
                        // supervisor exists yet on this path — the FOLD is
                        // this refusal's cleanup owner (the unary
                        // `ConfirmedOpening` scope-guard precedent): settle
                        // the registry record's release-once `complete` and
                        // every map entry, then refuse with zero further
                        // delivery (§2.7: "refuse the call, never truncate
                        // it").
                        self.in_flight.lock().remove(&key);
                        charge.registry.complete(&charge.key, charge.incarnation);
                        return Err(AdmissionDenied::ResourceExhausted);
                    }
                }
                None => {
                    let _ = tx.try_send(ChargedChunk {
                        body: payload.body.clone(),
                        permit: None,
                    });
                }
            }
        }
        if end_on_initial {
            // §2.6: the opening already carried END — the input half is
            // `Ended` from birth (the SS record's shape).
            record.lock().end_input();
        } else {
            self.senders.lock().insert(
                key,
                RequestChunkSender {
                    tx,
                    charge: call_ref.clone(),
                    record: Some(Arc::clone(&record)),
                },
            );
        }
        // Auto-grant (public arm's shape): opted-in uploads + a wired
        // emitter.
        let grant_emitter = if parse_request_window_initial(&payload.headers).is_some() {
            self.grant_emit.clone()
        } else {
            None
        };
        let request_stream = RequestStream::new(
            rx,
            grant_emitter,
            ev.from_node,
            meta.origin_hash,
            meta.seq_or_ts,
        );
        let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
            extract_trace_context(&payload.headers)
        } else {
            None
        };
        let mut ctx = RpcStreamingContext::new(
            meta.origin_hash,
            meta.seq_or_ts,
            payload.deadline_ns,
            payload.headers,
            cancellation.clone(),
            trace_context,
        );
        ctx.org_admission = Some(admitted);

        let call = Arc::new(ProtectedStreamCall {
            record: Arc::clone(&record),
            retire: Arc::clone(&retire_signal),
            deadline: resolved,
            call_ref: call_ref.clone(),
        });
        self.protected_calls.insert(key, call.clone());
        // The deadline's monotonic end derives from the admission's ONE
        // clock sample (`monotonic_deadline_for`), so a wall-clock jump
        // cannot move it.
        let at =
            tokio::time::Instant::from_std(lifetime.clock.monotonic_deadline_for(resolved.end_ns));
        tokio::spawn(run_client_stream_call(
            record,
            self.handler.clone(),
            ctx,
            request_stream,
            self.metrics.clone(),
            Some(retire_signal),
            Some((at, resolved.expiry_reason())),
            self.emit.clone(),
            (
                ev.from_node,
                self.session_id,
                meta.origin_hash,
                meta.seq_or_ts,
            ),
            StreamCallRegistration {
                key,
                in_flight: self.in_flight.clone(),
                flow_control: None,
                protected: self.protected_calls.clone(),
                registry: call_ref,
                senders: Some(self.senders.clone()),
            },
        ));
        Ok(call)
    }
}

impl RedexFold<()> for RpcStreamingRequestFold {
    /// Loopback / test shim: drives `apply_frame` with
    /// `from_node = 0` (AV-1 item 1). Production uses
    /// [`RpcStreamingRequestFold::apply_inbound`].
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        self.apply_frame(0, &ev.payload)
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
    /// R2-A/C5: the receiving incarnation of the frame currently being
    /// applied, set by `apply_inbound` from the event. Part of every
    /// per-call key below; `0` on test/loopback paths, like `from_node`.
    session_id: u64,
    handler: Arc<dyn RpcDuplexHandler>,
    /// Async emitter for response chunks (per-call ordering via
    /// awaited emits — same rationale as `RpcServerStreamingFold`).
    emit: RpcAsyncResponseEmitter,
    /// Optional request-direction grant emitter.
    grant_emit: Option<RpcRequestGrantEmitter>,
    /// (from_node, receiving_session_id, caller_origin, call_id) →
    /// cancellation token — session-fenced (AV-1 item 1, C5).
    in_flight: InFlightCalls,
    senders: RequestChunkSenders,
    /// Stage 2 slices 2.2/2.3 (C8): the response-direction flow-control
    /// map + `STREAM_GRANT` arm "as SS" — the caller's
    /// `nrpc-stream-window-initial` header installs a per-call semaphore
    /// the response pump awaits per chunk, and a `STREAM_GRANT` refills
    /// it. Absent entry = unbounded credit (back-compat).
    flow_control: FlowControlMap,
    /// The registration's live PROTECTED calls (§2.2 — Stage 2 slice 2.2
    /// extends the seam to this fold). Public calls never enter this set;
    /// `ServeHandle::drop` retires exactly these (Q3).
    protected_calls: ProtectedStreamOwners,
    metrics: Option<Arc<crate::adapter::net::mesh_rpc_metrics::ServiceMetricsAtomic>>,
}

impl RpcDuplexFold {
    /// Construct a duplex server fold. `emit` publishes individual
    /// response chunks AND the terminal frame on the caller's
    /// reply channel (uses the async emitter for per-call
    /// ordering, same as `RpcServerStreamingFold`).
    pub fn new(handler: Arc<dyn RpcDuplexHandler>, emit: RpcAsyncResponseEmitter) -> Self {
        Self {
            session_id: 0,
            handler,
            emit,
            grant_emit: None,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            flow_control: Arc::new(Mutex::new(HashMap::new())),
            protected_calls: ProtectedStreamOwners::new(),
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

    /// Test-only: snapshot of the in-flight call set (four-part keys —
    /// `(from_node, session_id, origin, call_id)`, C5).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn in_flight_keys(&self) -> Vec<StreamCallKey> {
        self.in_flight.lock().keys().copied().collect()
    }

    /// Test-only: snapshot of the in-flight per-call senders.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn sender_keys(&self) -> Vec<StreamCallKey> {
        self.senders.lock().keys().copied().collect()
    }

    /// Test-only: available response-direction flow-control permits for a
    /// call key, or `None` if no per-call semaphore is installed (Stage 2
    /// slices 2.2/2.3 — the wrong-session GRANT witness's observation).
    #[cfg(any(test, feature = "fixtures"))]
    pub fn flow_control_permits(&self, key: StreamCallKey) -> Option<usize> {
        self.flow_control
            .lock()
            .get(&key)
            .map(|s| s.available_permits())
    }

    /// The registration-owned live PROTECTED calls (§2.2) — the serve
    /// seam clones this into its `ServeHandle` so handle drop retires
    /// exactly these records (Q3).
    pub fn protected_owners(&self) -> ProtectedStreamOwners {
        self.protected_calls.clone()
    }
}

impl RpcDuplexFold {
    /// Production-path entry point. Keys per-call state (in-flight
    /// token + request-chunk sender) by `(from_node, claimed_origin,
    /// call_id)` so a forged REQUEST_CHUNK / CANCEL from another peer
    /// misses the map and no-ops (AV-1 item 1).
    pub fn apply_inbound(&mut self, ev: &RpcInboundEvent) -> Result<(), RedexError> {
        self.session_id = ev.session_id;
        self.apply_frame(ev.from_node, &ev.payload)
    }

    /// Core frame application shared by [`Self::apply_inbound`] (real
    /// `from_node`) and the [`RedexFold`] loopback shim (`0`).
    fn apply_frame(&mut self, from_node: u64, frame: &Bytes) -> Result<(), RedexError> {
        let Some(meta) = (if frame.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&frame[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            tracing::warn!(
                payload_len = frame.len(),
                "rpc duplex server fold: event payload too short for EventMeta",
            );
            return Ok(());
        };
        let key = (from_node, self.session_id, meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let payload = match RpcRequestPayload::decode(frame.slice(RPC_FRAME_BODY_OFFSET..))
                {
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
                            emit(from_node, caller_origin, call_id, resp).await;
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
                        emit(from_node, caller_origin, call_id, resp).await;
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
                            emit(from_node, caller_origin, call_id, resp).await;
                        });
                        return Ok(());
                    }
                }
                let cancellation = RpcCancellationToken::new();
                self.in_flight.lock().insert(key, cancellation.clone());

                // Build per-call request-side mpsc (Phase B
                // pattern).
                let (req_tx, req_rx) =
                    tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_REQUEST_PUMP_CAPACITY);
                let end_on_initial = payload.flags & FLAG_RPC_REQUEST_END != 0;
                let is_pure_terminator = end_on_initial && payload.body.is_empty();
                if !is_pure_terminator {
                    // Same invariant as the client-streaming fold:
                    // fresh bounded mpsc with a live receiver cannot
                    // reject the first send.
                    if req_tx
                        .try_send(ChargedChunk {
                            body: payload.body,
                            permit: None,
                        })
                        .is_err()
                    {
                        debug_assert!(false, "fresh duplex request mpsc rejected initial body");
                        tracing::error!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc duplex server fold: fresh mpsc rejected initial REQUEST body (invariant break)",
                        );
                    }
                }
                if !end_on_initial {
                    self.senders.lock().insert(
                        key,
                        RequestChunkSender {
                            tx: req_tx,
                            charge: None,
                            record: None,
                        },
                    );
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
                let request_stream = RequestStream::new(
                    req_rx,
                    grant_emitter,
                    from_node,
                    meta.origin_hash,
                    meta.seq_or_ts,
                );

                // Build the per-call response-side mpsc (existing
                // server-streaming-response pattern). The handler
                // writes chunks to the sink; the pump task drains
                // the receiver and publishes RESPONSE events.
                let (resp_tx, mut resp_rx) =
                    tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_PUMP_CAPACITY);
                let response_sink = RpcResponseSink {
                    inner: resp_tx,
                    metrics: self.metrics.clone(),
                    byte_charge: None,
                    gate: None,
                };

                let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
                    extract_trace_context(&payload.headers)
                } else {
                    None
                };
                let deadline_ns = payload.deadline_ns;
                let ctx = RpcStreamingContext::new(
                    meta.origin_hash,
                    meta.seq_or_ts,
                    deadline_ns,
                    payload.headers,
                    cancellation.clone(),
                    trace_context,
                );
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
                            body: chunk.body,
                        };
                        let chunk_permit = chunk.permit;
                        pump_emit(from_node, caller_origin, call_id, resp).await;
                        // §2.7: publishing releases the item's byte
                        // reservation (none today — the duplex response
                        // sink is public — but the discipline is shared).
                        if let Some(shared) = chunk_permit {
                            if let Some(permit) = shared.take() {
                                permit.release();
                            }
                        }
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
                    // C7 (Q3): a deadline expiry's terminal is typed
                    // `Timeout` — recorded so the terminal selection can
                    // distinguish the deadline's own cancel signal from a
                    // caller CANCEL (CANCEL-wins preserved).
                    let mut deadline_expired: Option<String> = None;
                    let outcome = if deadline_ns > 0 {
                        let now_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0);
                        let remaining = deadline_ns.saturating_sub(now_ns);
                        if remaining == 0 {
                            if !cancel_for_deadline.is_cancelled() {
                                deadline_expired = Some(
                                    "duplex handler deadline_ns already expired at spawn"
                                        .to_string(),
                                );
                            }
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
                                    if !cancel_for_deadline.is_cancelled() {
                                        deadline_expired =
                                            Some("duplex handler deadline_ns exceeded".to_string());
                                    }
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
                    // CANCEL-wins ordering: if the cancellation
                    // token fired during execution, override the
                    // handler's terminal with Cancelled. C7 (Q3): a
                    // deadline expiry that no caller CANCEL preceded is
                    // typed `Timeout`.
                    let terminal = if let Some(message) = deadline_expired {
                        RpcResponsePayload {
                            status: RpcStatus::Timeout,
                            headers: vec![],
                            body: Bytes::from(message),
                        }
                    } else if cancel_probe.is_cancelled() {
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
                    emit(from_node, caller_origin, call_id, terminal).await;
                });
            }
            DISPATCH_RPC_REQUEST_CHUNK => {
                apply_request_chunk_to_senders(
                    from_node,
                    self.session_id,
                    frame.slice(RPC_FRAME_BODY_OFFSET..),
                    &meta,
                    &self.senders,
                    "duplex",
                );
            }
            DISPATCH_RPC_STREAM_GRANT => {
                // Response-direction credit (Stage 2 slices 2.2/2.3, C8):
                // the `flow_control` map + `STREAM_GRANT` arm "as SS". A
                // grant whose 4-tuple key misses the map is dropped
                // silently — a forged grant carrying another peer's /
                // session's coordinates cannot refill this call's window
                // (AV-1 item 1, C5), and the cross-direction grant kind
                // (`DISPATCH_RPC_REQUEST_GRANT`, server → caller) never
                // reaches this arm at all. A PROTECTED record's credit is
                // checked against its §2.6 state: it survives the
                // handler's return — it is what lets a drain finish — and
                // stops once the call is terminal or its output half has
                // ended.
                let amount = match decode_stream_grant(&frame[RPC_FRAME_BODY_OFFSET..]) {
                    Some(n) => n,
                    None => {
                        tracing::debug!(
                            caller_origin = format!("{:#x}", meta.origin_hash),
                            call_id = meta.seq_or_ts,
                            "rpc duplex server fold: malformed STREAM_GRANT payload",
                        );
                        return Ok(());
                    }
                };
                if amount == 0 {
                    return Ok(());
                }
                if let Some(sem) = self.flow_control.lock().get(&key).cloned() {
                    if let Some(call) = self.protected_calls.get(&key) {
                        if !call.record.lock().credit_grantable() {
                            return Ok(());
                        }
                    }
                    // Tokio's `Semaphore::add_permits` is bounded by
                    // `MAX_PERMITS`; cap defensively (the SS arm's clause).
                    let safe = (amount as usize).min(usize::MAX >> 4);
                    sem.add_permits(safe);
                }
            }
            DISPATCH_RPC_CANCEL => {
                // A PROTECTED record enters retirement through its
                // supervisor (§2.2/§2.6 — Stage 2 slice 2.2): the signal
                // reaches the supervisor's `select!`, and terminal
                // selection + map removal stay that one owner's work
                // (§2.4's single removal point).
                if let Some(call) = self.protected_calls.get(&key) {
                    if let Some(token) = self.in_flight.lock().get(&key).cloned() {
                        token.cancel();
                    }
                    call.retire(StreamTerminalReason::Cancelled);
                    return Ok(());
                }
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

impl RpcDuplexFold {
    /// The PROTECTED opening path (contract 4's fold seam — Stage 2 slice
    /// 2.2 extends it to duplex). The transaction shape is the SS seam's
    /// verbatim: §2.1's deadline resolution BEFORE any handler effect, the
    /// raw proof header stripped (E1.6), §3 step-5's ownership TRANSFER at
    /// the effect boundary — then the §2.2 supervisor
    /// ([`run_stream_call_supervisor`] with [`SupervisedHandler::Duplex`])
    /// owns the handler, the response pump, the flow semaphore and the one
    /// terminal. Every refusal is one typed
    /// [`AdmissionDenied`]
    /// the bridge routes through the unchanged `emit_admission_denial`;
    /// the fold emits NOTHING on refusal (zero handler effects).
    ///
    /// The caller's `nrpc-stream-window-initial` header installs the
    /// response-direction semaphore here (Stage 2 slices 2.2/2.3, C8) and
    /// `STREAM_GRANT`s refill it under the §2.6 record's credit rule.
    pub fn apply_inbound_admitted(
        &mut self,
        ev: &RpcInboundEvent,
        admitted: crate::adapter::net::behavior::org_admission::Admitted,
        lifetime: &StreamCallLifetime<'_>,
        mut lease: Option<ProtectedCallLease>,
    ) -> Result<
        Arc<ProtectedStreamCall>,
        crate::adapter::net::behavior::org_admission::AdmissionDenied,
    > {
        use crate::adapter::net::behavior::org_admission::AdmissionDenied;

        self.session_id = ev.session_id;
        let Some(meta) = (if ev.payload.len() >= EVENT_META_SIZE {
            EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])
        } else {
            None
        }) else {
            return Err(AdmissionDenied::MalformedProof);
        };
        if meta.dispatch != DISPATCH_RPC_REQUEST {
            return Err(AdmissionDenied::NotOrgProtected);
        }
        let key = (
            ev.from_node,
            self.session_id,
            meta.origin_hash,
            meta.seq_or_ts,
        );
        // §3: a duplicate while the key is live is refused as
        // `ActiveCallOwned` BEFORE the payload decode.
        if self.in_flight.lock().contains_key(&key) {
            return Err(AdmissionDenied::ActiveCallOwned);
        }
        if ev.payload.len() < RPC_FRAME_BODY_OFFSET {
            return Err(AdmissionDenied::MalformedProof);
        }
        let Ok(mut payload) = RpcRequestPayload::decode(ev.payload.slice(RPC_FRAME_BODY_OFFSET..))
        else {
            return Err(AdmissionDenied::MalformedProof);
        };
        // The DX REQUEST flag check (contract 4 / §1.5): flags whose
        // derived shape is not duplex are `ShapeMismatch`, never admitted.
        if !dx_request_flags_ok(payload.flags) {
            return Err(AdmissionDenied::ShapeMismatch);
        }
        // §2.1 — resolve before any handler effect (the SS seam's exact
        // resolution).
        let requested = (payload.deadline_ns != 0).then_some(payload.deadline_ns);
        let resolved = resolve_stream_deadline(
            lifetime.clock.wall_ns,
            requested,
            lifetime.credential_ends_ns,
            &lifetime.policy,
        )
        .map_err(|refusal| {
            tracing::warn!(
                caller_origin = format!("{:#x}", meta.origin_hash),
                call_id = meta.seq_or_ts,
                ?refusal,
                "rpc duplex server fold: protected opening refused before handler effects",
            );
            AdmissionDenied::DeadlineExceedsPolicy
        })?;
        // E1.6: verified attribution in, raw credential material out.
        payload.headers.retain(|(name, _)| {
            name != crate::adapter::net::behavior::org_call::ORG_ADMISSION_HEADER
        });

        // §3 step 5 — the registry's ownership TRANSFER before any fold
        // effect (in-flight insert, sender creation, handler spawn).
        let retire_signal = Arc::new(StreamRetireSignal::new());
        let mut call_ref: Option<RegistryCallRef> = None;
        if let Some(lease) = lease.as_mut() {
            let registry = Arc::clone(lease.registry());
            let ref_for_call = RegistryCallRef {
                registry: Arc::clone(&registry),
                key: lease.key.clone(),
                incarnation: lease.incarnation,
            };
            registry.confirm(lease, Arc::clone(&retire_signal), None)?;
            call_ref = Some(ref_for_call);
        }

        let record = Arc::new(Mutex::new(StreamCallRecord::new_duplex()));
        let cancellation = RpcCancellationToken::new();
        self.in_flight.lock().insert(key, cancellation.clone());
        // Response-direction flow control (Stage 2 slices 2.2/2.3, C8):
        // `Some(sem)` means the supervisor's pump must `acquire().await`
        // one permit per chunk before emitting; `STREAM_GRANT` events
        // refill it. Absent entry = unbounded credit.
        let flow_sem = parse_stream_window_initial(&payload.headers).map(|n| {
            let sem = Arc::new(tokio::sync::Semaphore::new(n as usize));
            self.flow_control.lock().insert(key, sem.clone());
            sem
        });
        // Per-call request-chunk mpsc with the §2.7 charge + §2.6 record
        // on the sender for protected uploads.
        let (tx, rx) = tokio::sync::mpsc::channel::<ChargedChunk>(STREAMING_REQUEST_PUMP_CAPACITY);
        let end_on_initial = payload.flags & FLAG_RPC_REQUEST_END != 0;
        let is_pure_terminator = end_on_initial && payload.body.is_empty();
        if !is_pure_terminator {
            // §2.7 ("refuse the call, never truncate it"): the OPENING
            // body reserves its bytes and is delivered under the same
            // check-and-commit as any chunk.
            match call_ref.as_ref() {
                Some(charge) => {
                    if !deliver_protected_body(charge, &tx, payload.body.clone()) {
                        charge.latch_exhausted();
                        // The CS seam's clause verbatim (Stage 2 §4.2,
                        // F-S2.2-5): the fold owns this pre-supervisor
                        // refusal's single removal.
                        self.in_flight.lock().remove(&key);
                        self.flow_control.lock().remove(&key);
                        charge.registry.complete(&charge.key, charge.incarnation);
                        return Err(AdmissionDenied::ResourceExhausted);
                    }
                }
                None => {
                    let _ = tx.try_send(ChargedChunk {
                        body: payload.body.clone(),
                        permit: None,
                    });
                }
            }
        }
        if end_on_initial {
            // §2.6: the opening already carried END — input starts `Ended`.
            record.lock().end_input();
        } else {
            self.senders.lock().insert(
                key,
                RequestChunkSender {
                    tx,
                    charge: call_ref.clone(),
                    record: Some(Arc::clone(&record)),
                },
            );
        }
        // Auto-grant (public arm's shape): opted-in uploads + a wired
        // emitter.
        let grant_emitter = if parse_request_window_initial(&payload.headers).is_some() {
            self.grant_emit.clone()
        } else {
            None
        };
        let request_stream = RequestStream::new(
            rx,
            grant_emitter,
            ev.from_node,
            meta.origin_hash,
            meta.seq_or_ts,
        );
        let trace_context = if payload.flags & FLAG_RPC_PROPAGATE_TRACE != 0 {
            extract_trace_context(&payload.headers)
        } else {
            None
        };
        let mut ctx = RpcStreamingContext::new(
            meta.origin_hash,
            meta.seq_or_ts,
            payload.deadline_ns,
            payload.headers,
            cancellation.clone(),
            trace_context,
        );
        ctx.org_admission = Some(admitted);

        let call = Arc::new(ProtectedStreamCall {
            record: Arc::clone(&record),
            retire: Arc::clone(&retire_signal),
            deadline: resolved,
            call_ref: call_ref.clone(),
        });
        self.protected_calls.insert(key, call.clone());
        // The deadline's monotonic end derives from the admission's ONE
        // clock sample (`monotonic_deadline_for`), so a wall-clock jump
        // cannot move it.
        let at =
            tokio::time::Instant::from_std(lifetime.clock.monotonic_deadline_for(resolved.end_ns));
        tokio::spawn(run_stream_call_supervisor(
            record,
            SupervisedHandler::Duplex(self.handler.clone(), ctx, request_stream),
            self.metrics.clone(),
            flow_sem,
            Some(Arc::new(StreamProducerGate::new())),
            Some(Arc::clone(&call.retire)),
            Some((at, resolved.expiry_reason())),
            false,
            self.emit.clone(),
            (ev.from_node, meta.origin_hash, meta.seq_or_ts),
            StreamCallRegistration {
                key,
                in_flight: self.in_flight.clone(),
                flow_control: Some(self.flow_control.clone()),
                protected: self.protected_calls.clone(),
                registry: call_ref,
                senders: Some(self.senders.clone()),
            },
        ));
        Ok(call)
    }
}

impl RedexFold<()> for RpcDuplexFold {
    /// Loopback / test shim: drives `apply_frame` with
    /// `from_node = 0` (AV-1 item 1). Production uses
    /// [`RpcDuplexFold::apply_inbound`].
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        self.apply_frame(0, &ev.payload)
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
                match RpcResponsePayload::decode(ev.payload.slice(RPC_FRAME_BODY_OFFSET..)) {
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
                match decode_request_grant(&ev.payload[RPC_FRAME_BODY_OFFSET..]) {
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
                match RpcResponsePayload::decode(ev.payload.slice(RPC_FRAME_BODY_OFFSET..)) {
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
                match decode_request_grant(&ev.payload[RPC_FRAME_BODY_OFFSET..]) {
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
    // OA2-E0.2 P0 — `peek_request_service` boundary contract.
    // --------------------------------------------------------------------

    /// Build a REQUEST frame `EventMeta ‖ RpcRouteV1(0) ‖
    /// RpcRequestPayload{service, ..}` for the peek unit tests.
    fn request_frame_for_peek(service: &str) -> Vec<u8> {
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 0, 1, 0);
        let req = RpcRequestPayload {
            service: service.to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![],
            body: Bytes::from_static(b"body"),
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, 0);
        req.encode_into(&mut buf);
        buf
    }

    /// The peek reads back exactly the `service` the full encoder
    /// wrote — the invariant the serve-bridge equality check relies
    /// on (peek and full decode agree on the service).
    #[test]
    fn peek_request_service_matches_full_decode() {
        for name in ["admin", "echo.v1", "x"] {
            let frame = request_frame_for_peek(name);
            assert_eq!(peek_request_service(&frame), Some(name));
            // And it agrees with the authoritative decoder.
            let decoded =
                RpcRequestPayload::decode(Bytes::from(frame[RPC_FRAME_BODY_OFFSET..].to_vec()))
                    .expect("decode");
            assert_eq!(decoded.service, name);
        }
    }

    /// Unreadable service fields return `None`, mirroring the `Err`
    /// arms of `RpcRequestPayload::decode` — the bridge then lets the
    /// fold's full decode reject the frame (`UnknownVersion`) rather
    /// than silently dropping it.
    #[test]
    fn peek_request_service_none_on_malformed() {
        // Frame with no room for the route/body at all.
        let short = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 0, 1, 0).to_bytes();
        assert_eq!(peek_request_service(&short), None);

        // Route present but zero-length service (decode rejects empty).
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST, 0, 0, 1, 0);
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, 0);
        buf.push(0u8); // svc_len = 0
        assert_eq!(peek_request_service(&buf), None);

        // Length byte claims more bytes than remain.
        let mut buf = Vec::new();
        buf.extend_from_slice(&meta.to_bytes());
        encode_rpc_route(&mut buf, 0);
        buf.push(5u8); // svc_len = 5 but no bytes follow
        assert_eq!(peek_request_service(&buf), None);
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
            (RpcStatus::AdmissionDenied, 0x0009),
        ] {
            assert_eq!(status.to_wire(), expected, "{status:?}");
            assert_eq!(RpcStatus::from_wire(expected), status);
        }
    }

    /// Reserved numeric range (`0x000A..=0x7FFF`) decodes as
    /// `Application(v)` for forward-compat with future canonical
    /// assignments. A future status numbered `0x000A` would round-
    /// trip via `from_wire(0x000A)` until that variant is added,
    /// at which point the variant takes precedence.
    #[test]
    fn reserved_status_range_decodes_as_application_for_forward_compat() {
        let decoded = RpcStatus::from_wire(0x000A);
        assert_eq!(decoded, RpcStatus::Application(0x000A));
        assert_eq!(decoded.to_wire(), 0x000A);
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

    /// R3-1: a `RequestStream` auto-grant carries the call's
    /// AEAD-authenticated `from_node`, so the upload grant is
    /// session-scoped — the fold binds each call to its own session, and
    /// two calls sharing one entity/origin + caller-chosen call_id
    /// produce grants with DISTINCT identities that cannot collapse and
    /// refill each other's request semaphore.
    ///
    /// Red-witness: the pre-R3-1 emit dropped `from_node` (fired
    /// `(origin, call_id, 1)`), so both sessions' grants collapsed to the
    /// same `(origin, call_id)` — reproduced here by hardcoding
    /// `from_node = 0` in the emit, which makes the two captured grants
    /// identical and fails the distinctness assertion.
    #[tokio::test]
    async fn request_stream_auto_grant_carries_the_authenticated_from_node() {
        use futures::StreamExt;

        const ORIGIN: u64 = 0xBEEF;
        const CALL: u64 = 7;
        const NODE_A: u64 = 0xAAAA;
        const NODE_B: u64 = 0xBBBB;

        // A capturing grant emitter (production `RpcRequestGrantEmitter`
        // signature) records every `(from_node, origin, call_id, credits)`.
        type Grants = Arc<Mutex<Vec<(u64, u64, u64, u32)>>>;
        let captured: Grants = Arc::new(Mutex::new(Vec::new()));
        let mk_emitter = |sink: Grants| -> RpcRequestGrantEmitter {
            Arc::new(move |from_node, origin, call_id, credits| {
                sink.lock().push((from_node, origin, call_id, credits));
            })
        };

        // Drive one poll of a RequestStream bound to `node`, over the same
        // ORIGIN + CALL, and return the grant it fired.
        async fn one_grant(node: u64, emit: RpcRequestGrantEmitter) {
            let (tx, rx) = tokio::sync::mpsc::channel::<ChargedChunk>(4);
            tx.send(ChargedChunk {
                body: Bytes::from_static(b"chunk"),
                permit: None,
            })
            .await
            .expect("queue chunk");
            drop(tx);
            let mut stream = RequestStream::new(rx, Some(emit), node, ORIGIN, CALL);
            assert_eq!(
                stream.next().await.as_deref(),
                Some(&b"chunk"[..]),
                "the stream must yield the queued chunk",
            );
        }

        one_grant(NODE_A, mk_emitter(captured.clone())).await;
        one_grant(NODE_B, mk_emitter(captured.clone())).await;

        let grants = captured.lock().clone();
        assert_eq!(
            grants,
            vec![(NODE_A, ORIGIN, CALL, 1), (NODE_B, ORIGIN, CALL, 1)],
            "each poll fires ONE grant carrying that call's from_node",
        );
        assert_ne!(
            grants[0], grants[1],
            "two sessions over the same origin+call_id must produce DISTINCT grant identities",
        );
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
        assert_eq!(request_wire_size(&p), RPC_FRAME_BODY_OFFSET + 17);
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
        assert_eq!(response_wire_size(&p), RPC_FRAME_BODY_OFFSET + 7);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
        buf.extend_from_slice(&payload.encode());
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    fn rpc_cancel_event(caller_origin: u64, call_id: u64) -> RedexEvent {
        let meta = EventMeta::new(DISPATCH_RPC_CANCEL, 0, caller_origin, call_id, 0);
        let mut buf = meta.to_bytes().to_vec();
        // OA2-E0.2: RpcRouteV1 route placeholder (folds skip it).
        encode_rpc_route(&mut buf, 0);
        RedexEvent {
            entry: RedexEntry::new_heap(0, 0, buf.len() as u32, 0, 0),
            payload: bytes::Bytes::from(buf),
        }
    }

    /// AV-1 item 1: wrap a synthetic frame as an inbound event from
    /// the AEAD-authenticated session peer `from_node`, so a test can
    /// drive the production `apply_inbound` seam directly. The folds
    /// read the caller origin + call_id from the frame's `EventMeta`;
    /// `channel_hash` / `origin_hash` are ignored, so only `from_node`
    /// and `payload` are load-bearing for the call-identity key.
    fn inbound(from_node: u64, frame: bytes::Bytes) -> RpcInboundEvent {
        RpcInboundEvent {
            session_id: 0,
            channel_hash: 0,
            origin_hash: 0,
            from_node,
            payload: frame,
        }
    }

    /// Captures responses emitted by the fold for assertion in tests.
    fn capturing_emitter() -> (RpcResponseEmitter, CapturedResponses) {
        let captured: CapturedResponses = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        let emit: RpcResponseEmitter =
            Arc::new(move |_from_node, _session_id, origin, call_id, resp| {
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

    /// `(source, body)` pairs recorded when a handler body starts.
    /// Test-local typedef for the same reason as
    /// [`CapturedResponses`] — it keeps the field and the local
    /// under `clippy::type_complexity`.
    type EntryLog = Arc<Mutex<Vec<(u64, String)>>>;

    /// Records the request body when the handler body starts,
    /// tagged with the source that delivered it, then parks on a
    /// semaphore the test controls.
    ///
    /// Parking means no call can complete until the test lets it,
    /// so `peak_parked` measures how many handlers were inside the
    /// handler body at once.
    struct ParkingHandler {
        entered: EntryLog,
        parked: Arc<AtomicUsize>,
        peak_parked: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl RpcHandler for ParkingHandler {
        async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
            let body = String::from_utf8_lossy(&ctx.payload.body).into_owned();
            self.entered.lock().push((ctx.session_peer, body));
            let live = self.parked.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_parked.fetch_max(live, Ordering::SeqCst);
            let permit = Arc::clone(&self.release)
                .acquire_owned()
                .await
                .expect("the release semaphore is never closed");
            self.parked.fetch_sub(1, Ordering::SeqCst);
            drop(permit);
            Ok(RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: vec![],
                body: ctx.payload.body,
            })
        }
    }

    /// **Where the fold's ordering is measured: the DISPATCH
    /// boundary.** Not at handler entry — the fold spawns one task
    /// per call and deliberately does not order those tasks against
    /// each other (see `RpcHandler::call`). What the fold owns is
    /// that it disposes of each delivered frame, in the order it
    /// was delivered, before it looks at the next one.
    ///
    /// The observable is the fold's own synchronous refusal: a
    /// REQUEST whose deadline has already passed (beyond the skew
    /// tolerance) is answered `Timeout` from *inside*
    /// `apply_inbound`, before it returns, with no handler and no
    /// task in the picture. So for one source the emitted sequence
    /// IS the order `apply_frame` processed that source's frames,
    /// and interleaving two sources cannot reorder either one.
    ///
    /// The other two thirds of the ordering contract are not this
    /// file's: the transport half (a `Reliable` stream releases in
    /// sequence order, and the native ingress holds an out-of-order
    /// frame rather than delivering past a gap) is witnessed in the
    /// wire and RTC layers, and the bridge half (one task drains
    /// the inbound receiver) is structural in `mesh_rpc.rs`. This
    /// pins the piece that lives here: the fold neither reorders,
    /// batches nor defers what it was handed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn server_fold_disposes_of_one_sources_requests_in_delivery_order() {
        const CALLS: u64 = 64;
        const NODE_A: u64 = 0xA1;
        const NODE_B: u64 = 0xB2;
        const ORIGIN: u64 = 0xD1A9;
        /// The fold's pinned "now". A deadline a minute behind it is
        /// past the 10 s skew tolerance, so every REQUEST below
        /// takes the synchronous refusal path.
        const NOW_NS: u64 = 1_000_000_000_000;

        type Disposed = Arc<Mutex<Vec<(u64, u64, RpcStatus)>>>;
        let disposed: Disposed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&disposed);
        let emit: RpcResponseEmitter = Arc::new(
            move |from_node, _session_id, _origin, call_id, resp: RpcResponsePayload| {
                sink.lock().push((from_node, call_id, resp.status));
            },
        );
        let mut fold = RpcServerFold::new(Arc::new(EchoHandler), emit).with_test_now_ns(NOW_NS);

        for i in 0..CALLS {
            for node in [NODE_A, NODE_B] {
                let req = RpcRequestPayload {
                    service: "ordered".to_string(),
                    deadline_ns: NOW_NS - 60_000_000_000,
                    flags: 0,
                    headers: vec![],
                    body: Bytes::from(format!("call-{i:04}")),
                };
                fold.apply_inbound(&inbound(node, rpc_request_event(ORIGIN, i, req).payload))
                    .expect("the fold accepts a well-formed REQUEST");
            }
        }

        let log = disposed.lock().clone();
        assert_eq!(
            log.len(),
            (CALLS * 2) as usize,
            "every frame must be disposed of before `apply_inbound` returns — \
             nothing here is allowed to wait on a task; log={log:?}"
        );
        let want: Vec<u64> = (0..CALLS).collect();
        for node in [NODE_A, NODE_B] {
            let seen: Vec<u64> = log
                .iter()
                .filter(|(n, _, _)| *n == node)
                .map(|(_, call_id, _)| *call_id)
                .collect();
            assert_eq!(
                seen, want,
                "source {node:#x} was dispatched out of delivery order"
            );
        }
        assert!(
            log.iter()
                .all(|(_, _, status)| *status == RpcStatus::Timeout),
            "the synchronous refusal is what this measures; log={log:?}"
        );
        assert!(
            fold.in_flight_keys().is_empty(),
            "an inline refusal registers no call"
        );
    }

    /// The other half, and the reason handler ENTRY is not ordered:
    /// one source's handlers run **concurrently**, so a slow or
    /// parked call never holds its successors.
    ///
    /// Every handler parks on a zero-permit semaphore after
    /// recording that it started, so a handler can only have
    /// started while all of its predecessors were still inside
    /// their bodies. All `2 × CALLS` are in there at once — which
    /// is also the statement that a handler which never completes
    /// cannot wedge later calls from its source. This is what the
    /// removed per-source entry chain put at risk: a chained
    /// successor could not be polled until its predecessor's first
    /// poll returned, and a ready `.await` is not a yield.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn server_fold_runs_one_sources_handlers_concurrently() {
        const CALLS: u64 = 64;
        const NODE_A: u64 = 0xA1;
        const NODE_B: u64 = 0xB2;
        const ORIGIN: u64 = 0xD1A9;

        let entered: EntryLog = Arc::new(Mutex::new(Vec::new()));
        let parked = Arc::new(AtomicUsize::new(0));
        let peak_parked = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(ParkingHandler {
                entered: Arc::clone(&entered),
                parked: Arc::clone(&parked),
                peak_parked: Arc::clone(&peak_parked),
                release: Arc::clone(&release),
            }),
            emit,
        );

        for i in 0..CALLS {
            for node in [NODE_A, NODE_B] {
                let req = RpcRequestPayload {
                    service: "ordered".to_string(),
                    deadline_ns: 0,
                    flags: 0,
                    headers: vec![],
                    body: Bytes::from(format!("call-{i:04}")),
                };
                fold.apply_inbound(&inbound(node, rpc_request_event(ORIGIN, i, req).payload))
                    .expect("the fold accepts a well-formed REQUEST");
            }
        }

        let want_entries = (CALLS * 2) as usize;
        assert!(
            wait_until(
                || entered.lock().len() == want_entries,
                Duration::from_secs(20)
            )
            .await,
            "only {} of {want_entries} handlers started",
            entered.lock().len()
        );
        assert_eq!(
            peak_parked.load(Ordering::SeqCst),
            want_entries,
            "handler execution was serialized: only {} handler(s) were ever \
             running at once, so a call started after a predecessor had \
             finished rather than alongside it",
            peak_parked.load(Ordering::SeqCst)
        );

        // Terminal disposition: every parked handler still answers.
        release.add_permits(want_entries);
        assert!(
            wait_until(
                || captured.lock().len() == want_entries,
                Duration::from_secs(20)
            )
            .await,
            "only {} of {want_entries} responses were emitted",
            captured.lock().len()
        );
        assert!(fold.in_flight_keys().is_empty());
    }

    /// **The hazard the removed entry chain created, as a test.** A
    /// handler whose FIRST POLL is long must not delay the next call
    /// from the same source.
    ///
    /// This is the case the parked-handler witness above cannot see.
    /// The chain passed its baton when the predecessor's first poll
    /// *returned*, and a handler that parks on a semaphore returns
    /// `Pending` almost immediately — so parked handlers looked
    /// concurrent while a handler that computes (or blocks) before
    /// its first await held every successor from its source for as
    /// long as it ran. A ready `.await` is not a yield, so "put the
    /// slow work after an await" was never a defence either.
    ///
    /// Handler A's body runs `std::ready(()).await` — a completed
    /// future, which does NOT return control to the executor — and
    /// then blocks its worker on a channel until the test releases
    /// it. So A's first poll is still in progress, on this runtime's
    /// worker, while the test waits for B. B must enter anyway. The
    /// release always fires, so a failure reports rather than hangs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_long_first_poll_does_not_delay_its_sources_successors() {
        const NODE: u64 = 0xA1;
        const ORIGIN: u64 = 0xD1A9;

        struct BlockingFirstPoll {
            entered: EntryLog,
            hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for BlockingFirstPoll {
            async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                let body = String::from_utf8_lossy(&ctx.payload.body).into_owned();
                self.entered.lock().push((ctx.session_peer, body.clone()));
                if body == "slow" {
                    // A ready await: polls through without yielding.
                    std::future::ready(()).await;
                    // Now occupy this worker, inside the first poll.
                    let hold = self.hold.lock().take().expect("one slow call");
                    let _ = hold.recv();
                }
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: ctx.payload.body,
                })
            }
        }

        let (release, hold) = std::sync::mpsc::channel::<()>();
        let entered: EntryLog = Arc::new(Mutex::new(Vec::new()));
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(BlockingFirstPoll {
                entered: Arc::clone(&entered),
                hold: Mutex::new(Some(hold)),
            }),
            emit,
        );

        for (call_id, body) in [(1u64, "slow"), (2, "after")] {
            let req = RpcRequestPayload {
                service: "ordered".to_string(),
                deadline_ns: 0,
                flags: 0,
                headers: vec![],
                body: Bytes::from(body),
            };
            fold.apply_inbound(&inbound(
                NODE,
                rpc_request_event(ORIGIN, call_id, req).payload,
            ))
            .expect("the fold accepts a well-formed REQUEST");
        }

        let successor_entered = wait_until(
            || {
                entered
                    .lock()
                    .iter()
                    .any(|(_, body)| body.as_str() == "after")
            },
            Duration::from_secs(10),
        )
        .await;
        // Unconditional: a failed assertion must not leave a worker
        // blocked for the rest of the suite.
        let _ = release.send(());
        assert!(
            successor_entered,
            "the successor never entered while its predecessor's first poll \
             was still running: one source's calls are serialized on their \
             synchronous prefix, which is the head-of-line effect the \
             per-source entry chain introduced; entered={:?}",
            entered.lock().clone()
        );
        assert!(
            wait_until(|| captured.lock().len() == 2, Duration::from_secs(20)).await,
            "only {} of 2 responses were emitted",
            captured.lock().len()
        );
        assert!(fold.in_flight_keys().is_empty());
    }

    /// E1.6: `apply_inbound_admitted` delivers the four-party `Admitted`
    /// to the handler via `RpcContext::org_admission` AND strips every
    /// `net-org-admission` proof header from the payload the handler
    /// sees, preserving the surrounding headers in order. (The public
    /// `apply_inbound` path leaves `org_admission` `None` — the other
    /// fold tests never set it, and their handlers keep their headers.)
    #[tokio::test]
    async fn admitted_request_delivers_attribution_and_strips_proof_header() {
        use crate::adapter::net::behavior::org::OrgKeypair;
        use crate::adapter::net::behavior::org_admission::Admitted;
        use crate::adapter::net::behavior::org_call::ORG_ADMISSION_HEADER;
        use crate::adapter::net::behavior::org_grant::CapabilityAuthorityId;
        use crate::adapter::net::identity::EntityId;

        type Seen = Arc<Mutex<Option<(Option<Admitted>, Vec<RpcHeader>)>>>;
        struct SpyHandler(Seen);
        #[async_trait::async_trait]
        impl RpcHandler for SpyHandler {
            async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                *self.0.lock() = Some((ctx.org_admission.clone(), ctx.payload.headers.clone()));
                Ok(RpcResponsePayload {
                    status: RpcStatus::Ok,
                    headers: vec![],
                    body: Bytes::new(),
                })
            }
        }

        let seen: Seen = Arc::new(Mutex::new(None));
        let (emit, _captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(Arc::new(SpyHandler(seen.clone())), emit);

        let admitted = Admitted {
            caller: EntityId::from_bytes([0x24u8; 32]),
            acting_org: OrgKeypair::from_bytes([0x77u8; 32]).org_id(),
            provider_org: OrgKeypair::from_bytes([0x42u8; 32]).org_id(),
            provider: EntityId::from_bytes([0x99u8; 32]),
            capability: CapabilityAuthorityId::for_tag("nrpc:oa2-echo"),
        };
        let req = RpcRequestPayload {
            service: "oa2-echo".to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: vec![
                ("x-keep".to_string(), b"1".to_vec()),
                (ORG_ADMISSION_HEADER.to_string(), b"opaque-proof".to_vec()),
                ("y-keep".to_string(), b"2".to_vec()),
            ],
            body: Bytes::from_static(b"hi"),
        };
        let frame = rpc_request_event(0xCAFE, 7, req).payload;
        fold.apply_inbound_admitted(&inbound(0x61, frame), admitted.clone(), None)
            .unwrap();

        assert!(
            wait_until(|| seen.lock().is_some(), Duration::from_secs(2)).await,
            "handler must run for an admitted request",
        );
        let (got_admission, got_headers) = seen.lock().clone().unwrap();
        assert_eq!(
            got_admission,
            Some(admitted),
            "the four-party Admitted must reach the handler",
        );
        assert_eq!(
            got_headers,
            vec![
                ("x-keep".to_string(), b"1".to_vec()),
                ("y-keep".to_string(), b"2".to_vec()),
            ],
            "the proof header is stripped; surrounding headers stay, in order",
        );
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
                || fold.in_flight_keys().contains(&(0, 0, 1, 42)),
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

    // ====================================================================
    // AV-1 item 1 — server-fold call/control identity is bound to the
    // AEAD-authenticated session peer `from_node`. Each witness drives
    // the production `apply_inbound` seam with a victim frame on one
    // session and an adversarial control frame that copies the victim's
    // origin + call_id but arrives on a DIFFERENT session. The forged
    // frame must miss the `(from_node, origin, call_id)` key and leave
    // the victim's call/control state untouched.
    // ====================================================================

    /// CANCEL hijack (unary). An attacker that copies the victim's
    /// origin + call_id onto a CANCEL, but sends it on its own session,
    /// must not cancel the victim's in-flight call. Only a CANCEL from
    /// the victim's own session cancels it.
    #[tokio::test]
    async fn unary_fold_foreign_session_cancel_cannot_hijack_a_call() {
        const VICTIM: u64 = 0xA;
        const ATTACKER: u64 = 0xB;
        const ORIGIN: u64 = 0x1111;
        const CALL_ID: u64 = 42;
        let resumed = Arc::new(AtomicBool::new(false));
        struct H {
            resumed: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl RpcHandler for H {
            async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
                tokio::select! {
                    _ = ctx.cancellation.cancelled() => {
                        self.resumed.store(true, Ordering::Release);
                        Err(RpcHandlerError::Internal("cancelled".to_string()))
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => Ok(RpcResponsePayload {
                        status: RpcStatus::Ok,
                        headers: vec![],
                        body: Bytes::from_static(b"slept"),
                    }),
                }
            }
        }
        let (emit, captured) = capturing_emitter();
        let mut fold = RpcServerFold::new(
            Arc::new(H {
                resumed: resumed.clone(),
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
        // Victim's REQUEST on the victim's session.
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_event(ORIGIN, CALL_ID, req).payload,
        ))
        .unwrap();
        assert!(
            wait_until(
                || fold
                    .in_flight_keys()
                    .contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
                Duration::from_secs(1)
            )
            .await
        );
        // Attacker's forged CANCEL on a DIFFERENT session — must miss.
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_cancel_event(ORIGIN, CALL_ID).payload,
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            fold.in_flight_keys()
                .contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
            "forged CANCEL from a foreign session must not remove the victim's entry",
        );
        assert!(
            !resumed.load(Ordering::Acquire),
            "victim handler must not observe the forged CANCEL",
        );
        assert!(
            captured.lock().is_empty(),
            "a hijacked CANCEL must not produce a terminal response",
        );
        // The victim's OWN CANCEL cancels the call.
        fold.apply_inbound(&inbound(VICTIM, rpc_cancel_event(ORIGIN, CALL_ID).payload))
            .unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "the victim's own CANCEL must cancel the call",
        );
        assert!(resumed.load(Ordering::Acquire));
        assert_eq!(captured.lock()[0].2.status, RpcStatus::Cancelled);
    }

    /// STREAM_GRANT + CANCEL hijack (server-streaming). A forged
    /// STREAM_GRANT from a foreign session must not refill the victim's
    /// flow-control window, and a forged CANCEL must not tear the call
    /// down.
    #[tokio::test]
    async fn streaming_fold_foreign_session_cannot_refill_or_cancel() {
        const VICTIM: u64 = 0xA;
        const ATTACKER: u64 = 0xB;
        const ORIGIN: u64 = 0x2222;
        const CALL_ID: u64 = 7;
        let release = Arc::new(Notify::new());
        struct Blocking {
            release: Arc<Notify>,
        }
        #[async_trait::async_trait]
        impl RpcStreamingHandler for Blocking {
            async fn call(
                &self,
                _ctx: RpcContext,
                _sink: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                // Never emit a chunk (so the pump consumes no permits);
                // hold the call open until released.
                self.release.notified().await;
                Ok(())
            }
        }
        let (emit, _captured) = capturing_async_emitter();
        let mut fold = RpcServerStreamingFold::new(
            Arc::new(Blocking {
                release: release.clone(),
            }),
            emit,
        );
        // Victim REQUEST opting into flow control with an initial
        // window of 2.
        let req = RpcRequestPayload {
            service: "s".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![(HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(), b"2".to_vec())],
            body: Bytes::new(),
        };
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_event(ORIGIN, CALL_ID, req).payload,
        ))
        .unwrap();
        assert!(
            wait_until(
                || fold
                    .in_flight_keys()
                    .contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
                Duration::from_secs(1)
            )
            .await
        );
        assert_eq!(
            fold.flow_control_permits((VICTIM, 0, ORIGIN, CALL_ID)),
            Some(2),
            "victim's initial window",
        );
        // Attacker STREAM_GRANT(5) on its own session — must miss.
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_stream_grant_event(ORIGIN, CALL_ID, 5).payload,
        ))
        .unwrap();
        assert_eq!(
            fold.flow_control_permits((VICTIM, 0, ORIGIN, CALL_ID)),
            Some(2),
            "a forged STREAM_GRANT from a foreign session must not refill the victim's window",
        );
        // Attacker CANCEL on its own session — must miss.
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_cancel_event(ORIGIN, CALL_ID).payload,
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            fold.in_flight_keys()
                .contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
            "a forged CANCEL from a foreign session must not tear down the victim's stream",
        );
        // The victim's own STREAM_GRANT(5) DOES refill.
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_stream_grant_event(ORIGIN, CALL_ID, 5).payload,
        ))
        .unwrap();
        assert_eq!(
            fold.flow_control_permits((VICTIM, 0, ORIGIN, CALL_ID)),
            Some(7),
            "the victim's own STREAM_GRANT must refill its window",
        );
        release.notify_one();
    }

    /// REQUEST_CHUNK + CANCEL hijack (client-streaming). A forged
    /// REQUEST_CHUNK from a foreign session must not enter the victim's
    /// upload stream, and a forged CANCEL must not close it.
    #[tokio::test]
    async fn client_streaming_fold_foreign_session_cannot_feed_or_cancel() {
        const VICTIM: u64 = 0xA;
        const ATTACKER: u64 = 0xB;
        const ORIGIN: u64 = 0xCAFE;
        const CALL_ID: u64 = 7;
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
        // Victim REQUEST (client-streaming flag), initial body "a".
        let req = RpcRequestPayload {
            service: "agg".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
            headers: vec![],
            body: Bytes::from_static(b"a"),
        };
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_event(ORIGIN, CALL_ID, req).payload,
        ))
        .unwrap();
        assert!(
            wait_until(
                || fold.sender_keys().contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
                Duration::from_secs(1)
            )
            .await
        );
        // Attacker forges a REQUEST_CHUNK and a CANCEL with the victim's
        // origin + call_id, both on its OWN session — both must miss.
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_request_chunk_event(ORIGIN, CALL_ID, 0, b"ATTACK".to_vec()).payload,
        ))
        .unwrap();
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_cancel_event(ORIGIN, CALL_ID).payload,
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            fold.sender_keys().contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
            "forged frames must not close the victim's upload stream",
        );
        // Victim feeds a legit chunk and ends its own stream.
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_chunk_event(ORIGIN, CALL_ID, 0, b"b".to_vec()).payload,
        ))
        .unwrap();
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_chunk_event(ORIGIN, CALL_ID, FLAG_RPC_REQUEST_END, b"c".to_vec()).payload,
        ))
        .unwrap();
        assert!(
            wait_until(|| !captured.lock().is_empty(), Duration::from_secs(2)).await,
            "expected terminal RESPONSE",
        );
        let bodies: Vec<Vec<u8>> = seen.lock().iter().map(|b| b.to_vec()).collect();
        assert_eq!(
            bodies,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            "the attacker's forged chunk must never enter the victim's stream",
        );
        assert!(
            !observed_cancel.load(Ordering::SeqCst),
            "the attacker's forged CANCEL must not cancel the victim's stream",
        );
    }

    /// REQUEST_CHUNK + CANCEL hijack (duplex). Same contract as the
    /// client-streaming witness, on the duplex fold.
    #[tokio::test]
    async fn duplex_fold_foreign_session_cannot_feed_or_cancel() {
        const VICTIM: u64 = 0xA;
        const ATTACKER: u64 = 0xB;
        const ORIGIN: u64 = 0xBEEF;
        const CALL_ID: u64 = 9;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed_cancel = Arc::new(AtomicBool::new(false));
        struct DuplexCollect {
            seen: Arc<Mutex<Vec<bytes::Bytes>>>,
            observed_cancel: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl RpcDuplexHandler for DuplexCollect {
            async fn call(
                &self,
                ctx: RpcStreamingContext,
                mut requests: RequestStream,
                _responses: RpcResponseSink,
            ) -> Result<(), RpcHandlerError> {
                use futures::StreamExt;
                while let Some(chunk) = requests.next().await {
                    self.seen.lock().push(chunk);
                }
                if ctx.cancellation.is_cancelled() {
                    self.observed_cancel.store(true, Ordering::SeqCst);
                }
                Ok(())
            }
        }
        let (emit, _captured) = capturing_async_emitter();
        let mut fold = RpcDuplexFold::new(
            Arc::new(DuplexCollect {
                seen: seen.clone(),
                observed_cancel: observed_cancel.clone(),
            }),
            emit,
        );
        let req = RpcRequestPayload {
            service: "dx".to_string(),
            deadline_ns: 0,
            flags: FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE,
            headers: vec![],
            body: Bytes::from_static(b"a"),
        };
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_event(ORIGIN, CALL_ID, req).payload,
        ))
        .unwrap();
        assert!(
            wait_until(
                || fold.sender_keys().contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
                Duration::from_secs(1)
            )
            .await
        );
        // Attacker forges a chunk + cancel on its own session — miss.
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_request_chunk_event(ORIGIN, CALL_ID, 0, b"ATTACK".to_vec()).payload,
        ))
        .unwrap();
        fold.apply_inbound(&inbound(
            ATTACKER,
            rpc_cancel_event(ORIGIN, CALL_ID).payload,
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            fold.sender_keys().contains(&(VICTIM, 0, ORIGIN, CALL_ID)),
            "forged frames must not close the victim's duplex upload stream",
        );
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_chunk_event(ORIGIN, CALL_ID, 0, b"b".to_vec()).payload,
        ))
        .unwrap();
        fold.apply_inbound(&inbound(
            VICTIM,
            rpc_request_chunk_event(ORIGIN, CALL_ID, FLAG_RPC_REQUEST_END, b"c".to_vec()).payload,
        ))
        .unwrap();
        assert!(
            wait_until(
                || !seen.lock().is_empty() && seen.lock().len() >= 3,
                Duration::from_secs(2)
            )
            .await,
            "victim's chunks must reach the handler",
        );
        let bodies: Vec<Vec<u8>> = seen.lock().iter().map(|b| b.to_vec()).collect();
        assert_eq!(
            bodies,
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()],
            "the attacker's forged chunk must never enter the victim's duplex stream",
        );
        assert!(
            !observed_cancel.load(Ordering::SeqCst),
            "the attacker's forged CANCEL must not cancel the victim's duplex stream",
        );
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
                || fold.in_flight_keys().contains(&(0, 0, 1, 99)),
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
                || fold.in_flight_keys().contains(&(0, 0, 7, 11)),
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        let emit: RpcAsyncResponseEmitter = Arc::new(move |_from_node, origin, call_id, resp| {
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
                || !captured.lock().is_empty() && fold.in_flight_keys().contains(&(0, 0, 7, 13)),
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
                || fold.in_flight_keys().contains(&(0, 0, 1, 99)),
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
        let (tx, _rx) = tokio::sync::mpsc::channel::<ChargedChunk>(2);
        let registry = RpcMetricsRegistry::new();
        let metrics: Arc<ServiceMetricsAtomic> = registry.for_service("drop_test");
        let sink = RpcResponseSink {
            inner: tx,
            metrics: Some(metrics.clone()),
            byte_charge: None,
            gate: None,
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
        // OA2-E0.2: RpcRouteV1 route placeholder — these test frames
        // feed the folds directly (no ingress select), and the folds
        // skip the route to reach the payload at RPC_FRAME_BODY_OFFSET.
        encode_rpc_route(&mut buf, 0);
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
                || fold.sender_keys().contains(&(0, 0, 0xCAFE, 7)),
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
                || fold.sender_keys().contains(&(0, 0, 2, 17)),
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
                || fold.in_flight_keys().contains(&(0, 0, 5, 99)),
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
    // =================================================================
    // Slice 1.4 — the §2.7 byte-accounting port (the production shape of
    // the Stage 0 `ByteBudgets`/`ItemPermit`/`SharedPermit` model) and
    // the request-direction charge at `apply_request_chunk_to_senders`.
    // =================================================================

    #[test]
    fn byte_reservation_rolls_back_in_order_and_releases_exactly_once() {
        let budgets = ByteBudgets::new(ByteLimits {
            per_call: 4,
            per_caller: 6,
            per_node: 8,
        })
        .expect("valid limits");
        let key1 = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 1,
        };
        let key2 = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 2,
        };
        let key3 = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 3,
        };

        // Unsatisfiable items fail PROMPTLY (§2.7: no waiting on permits
        // that can never exist).
        assert_eq!(
            budgets.validate_item(MAX_RPC_ITEM_BYTES + 1),
            Err(ByteRefusal::ItemTooLarge),
        );
        assert_eq!(
            budgets.validate_item(5),
            Err(ByteRefusal::ExceedsCallBudget),
        );
        assert!(!ByteRefusal::ItemTooLarge.is_satisfiable_by_waiting());
        assert!(ByteRefusal::CallerBudgetFull.is_satisfiable_by_waiting());

        // Order call → caller → node with rollback: the third item fits
        // its CALL budget but not the CALLER's — its per-call increment
        // must be rolled back, invisible in every counter.
        let p1 = budgets
            .reserve(key1.clone(), 1, ByteDirection::Response, 3)
            .expect("item 1 reserves");
        let p2 = budgets
            .reserve(key2.clone(), 1, ByteDirection::Response, 3)
            .expect("item 2 reserves");
        assert_eq!(
            budgets
                .reserve(key3.clone(), 1, ByteDirection::Response, 3)
                .expect_err("the caller budget is full"),
            ByteRefusal::CallerBudgetFull,
        );
        assert_eq!(
            budgets.call_bytes(&key3, 1, ByteDirection::Response),
            0,
            "the refused item's per-call increment is rolled back",
        );
        assert_eq!(
            budgets
                .reserve(key1.clone(), 1, ByteDirection::Response, 2)
                .expect_err("the call budget is full"),
            ByteRefusal::CallBudgetFull,
        );
        assert_eq!(budgets.call_bytes(&key1, 1, ByteDirection::Response), 3);
        assert_eq!(budgets.caller_bytes(&key1.caller), 6);
        assert_eq!(budgets.node_bytes(), 6);

        // Release-once: the shared permit is consumed exactly once.
        let shared = SharedPermit::new(p1);
        let first = shared.take().expect("the first take wins");
        assert!(shared.take().is_none(), "the second take gets nothing");
        first.release();
        assert_eq!(budgets.call_bytes(&key1, 1, ByteDirection::Response), 0);
        assert_eq!(budgets.caller_bytes(&key1.caller), 3);
        assert_eq!(budgets.node_bytes(), 3);
        p2.release();
        assert_eq!(budgets.caller_bytes(&key1.caller), 0);
        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);

        // A dropped, never-released permit settles exactly once and is
        // COUNTED as an ownership bug — the accounting never hides it.
        drop(
            budgets
                .reserve(key3.clone(), 1, ByteDirection::Response, 2)
                .expect("item reserves"),
        );
        assert_eq!(budgets.unsettled_drops(), 1);
        assert_eq!(budgets.node_bytes(), 0, "the drop still settles the charge");
    }

    /// S1_R Row 3 (F-3's closure) — §2.7's NODE-level rollback: a node
    /// refusal arriving AFTER both earlier level increments succeeded must
    /// roll BOTH back exactly (the call counter AND the caller counter).
    /// The sibling of
    /// [`byte_reservation_rolls_back_in_order_and_releases_exactly_once`]
    /// (which pins the caller-level leg), and the production twin of the S0
    /// model's `node_refusal_rolls_back_call_and_caller_reservations`.
    ///
    /// Red witness (the S1_R brief Appendix inverse): **A3** (both counter
    /// restorations deleted from [`ByteBudgets::reserve`]'s `NodeBudgetFull`
    /// arm) reddens both exact-value asserts below.
    #[test]
    fn node_budget_refusal_rolls_back_call_and_caller_reservations() {
        // Node-binding budgets: the node is the scarcest scope.
        let budgets = ByteBudgets::new(ByteLimits {
            per_call: 1_000,
            per_caller: 1_500,
            per_node: 2_000,
        })
        .expect("valid limits");
        let a = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 10,
        };
        let b = ProtectedCallKey {
            caller: EntityId::from_bytes([2u8; 32]),
            call_id: 11,
        };

        // Live charges set the node total just below its ceiling.
        let b_response = budgets
            .reserve(b.clone(), 1, ByteDirection::Response, 1_000)
            .expect("b charges 1000");
        let b_request = budgets
            .reserve(b.clone(), 1, ByteDirection::Request, 500)
            .expect("b charges 500 more");
        let a_first = budgets
            .reserve(a.clone(), 1, ByteDirection::Response, 400)
            .expect("a charges 400");
        assert_eq!(budgets.node_bytes(), 1_900);

        // The refused item passes the CALL level (400 + 200 ≤ 1_000) and
        // the CALLER level (400 + 200 ≤ 1_500) — two successful level
        // increments — then hits the NODE level (1_900 + 200 > 2_000).
        let refusal = budgets
            .reserve(a.clone(), 1, ByteDirection::Response, 200)
            .expect_err("refused at the node level");
        assert_eq!(refusal, ByteRefusal::NodeBudgetFull);
        assert_eq!(
            budgets.call_bytes(&a, 1, ByteDirection::Response),
            400,
            "the call counter was rolled back",
        );
        assert_eq!(
            budgets.caller_bytes(&a.caller),
            400,
            "the caller counter was rolled back",
        );
        assert_eq!(budgets.node_bytes(), 1_900, "the node counter never moved",);

        // Release-once through the untouched charges, exactly to zero.
        b_response.release();
        b_request.release();
        a_first.release();
        assert_eq!(budgets.node_bytes(), 0);
        assert_eq!(budgets.unsettled_drops(), 0);
    }

    /// S1_R Row 6 (F-6's Main ruling) — [`ItemPermit::transfer`]'s handoff
    /// semantics at the PRODUCTION permit: the source permit is CONSUMED,
    /// the target owns the same charge (handoff is not memory reclamation),
    /// exactly ONE release settles the pair, and another call's live bytes
    /// stay charged throughout. The production twin of the S0 model's
    /// `cancel_dequeue_handoff_consumes_one_permit` (whose registry dequeue
    /// race maps onto `transfer`'s contract here; Stage 2's request-direction
    /// queues are `transfer`'s named consumer).
    ///
    /// Red witness (the S1_R brief Appendix inverse): **A3b** (`self.settled
    /// = true;` deleted from [`ItemPermit::transfer`]) reddens "the bytes
    /// stay charged across the handoff" below — the source's drop would
    /// settle the charge early and count an unsettled drop.
    #[test]
    fn item_permit_transfer_consumes_once_across_the_handoff() {
        let budgets = ByteBudgets::new(ByteLimits {
            per_call: 1_000,
            per_caller: 1_500,
            per_node: 2_000,
        })
        .expect("valid limits");
        let a = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 10,
        };
        let b = ProtectedCallKey {
            caller: EntityId::from_bytes([2u8; 32]),
            call_id: 11,
        };

        // Another call's live bytes, which must stay charged throughout.
        let b_permit = budgets
            .reserve(b, 1, ByteDirection::Response, 700)
            .expect("b reserves");

        // The handed-over item.
        let source = budgets
            .reserve(a.clone(), 1, ByteDirection::Response, 100)
            .expect("a reserves");
        assert_eq!(budgets.call_bytes(&a, 1, ByteDirection::Response), 100);

        let target = source.transfer();
        assert_eq!(
            budgets.call_bytes(&a, 1, ByteDirection::Response),
            100,
            "transfer is not memory reclamation — the bytes stay charged across the handoff",
        );
        assert_eq!(
            budgets.unsettled_drops(),
            0,
            "the source permit is consumed by the handoff, not dropped unsettled",
        );

        // Exactly one release across the pair.
        target.release();
        assert_eq!(
            budgets.call_bytes(&a, 1, ByteDirection::Response),
            0,
            "exactly one release settles the pair",
        );
        assert_eq!(budgets.caller_bytes(&a.caller), 0);
        assert_eq!(
            budgets.node_bytes(),
            700,
            "another call's live bytes stay charged",
        );
        assert_eq!(budgets.unsettled_drops(), 0);
        b_permit.release();
        assert_eq!(budgets.node_bytes(), 0);
    }

    #[test]
    fn request_chunk_accounting_retires_the_call_and_stops_delivery() {
        // A real store gives the registry its live view (the model's
        // abstract authority); AV-9: the scratch dir is left behind.
        let dir = std::env::temp_dir().join(format!(
            "net-s14-unit-req-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = crate::adapter::net::behavior::org_revocation::OrgRevocationStore::init(
            &dir,
            crate::adapter::net::behavior::org_revocation::ProvisioningExpectation::MayBeFresh,
        )
        .expect("real store");
        let registry =
            ProtectedCallRegistry::with_limits(tiny_byte_call_limits(), tiny_byte_byte_limits())
                .expect("limits");
        registry.bind_store(None, Arc::new(store));
        let key = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 7,
        };
        let mut reservation = registry
            .reserve(OpeningRequest {
                key: key.clone(),
                session: SessionIdentity {
                    peer: 2,
                    session_id: 3,
                    establishment: Some([4u8; 32]),
                },
                session_generation: Some(1),
                registration: 1,
                shape: RpcCallShape::ClientStreaming,
                now_ns: 0,
            })
            .expect("reserve");
        let lease = registry
            .install(
                &mut reservation,
                VerifiedCallFacts {
                    acting_org: crate::adapter::net::behavior::org::OrgId::from_bytes([9u8; 32]),
                    member: EntityId::from_bytes([1u8; 32]),
                    member_generation: 1,
                    deadline: None,
                },
                0,
            )
            .expect("install");
        let charge = RegistryCallRef {
            registry: Arc::clone(&registry),
            key: key.clone(),
            incarnation: lease.incarnation,
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChargedChunk>(4);
        let senders: RequestChunkSenders = Arc::new(Mutex::new(HashMap::from([(
            (2u64, 3u64, 0x1111u64, 7u64),
            RequestChunkSender {
                tx,
                charge: Some(charge.clone()),
                record: None,
            },
        )])));
        let meta = EventMeta::new(DISPATCH_RPC_REQUEST_CHUNK, 0, 0x1111, 7, 0);

        // The happy path: a 4-byte chunk reserves, delivers, and its
        // permit releases at the yield (RequestStream hands the body on).
        let mut small = Vec::new();
        RpcRequestChunkPayload {
            call_id: 7,
            flags: 0,
            headers: vec![],
            body: bytes::Bytes::from_static(b"1234"),
        }
        .encode_into(&mut small);
        apply_request_chunk_to_senders(2, 3, Bytes::from(small), &meta, &senders, "unit");
        {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            rt.block_on(async {
                let chunk = rx.recv().await.expect("the chunk delivers");
                assert_eq!(chunk.body.as_ref(), b"1234".as_slice());
                release_chunk_permit(&chunk);
            });
        }
        assert_eq!(
            registry
                .bytes()
                .call_bytes(&key, lease.incarnation, ByteDirection::Request),
            0,
            "the yield releases the reservation",
        );

        // A chunk over the tiny per-call budget retires the call with
        // `ResourceExhausted` and STOPS all further delivery (§2.7: never
        // a silent drop followed by `Ok`).
        let mut big = Vec::new();
        RpcRequestChunkPayload {
            call_id: 7,
            flags: 0,
            headers: vec![],
            body: bytes::Bytes::from(vec![0u8; 32]),
        }
        .encode_into(&mut big);
        apply_request_chunk_to_senders(2, 3, Bytes::from(big), &meta, &senders, "unit");
        assert_eq!(
            registry.terminal_reason(&key),
            Some(StreamTerminalReason::ResourceExhausted),
            "an undeliverable chunk latches ResourceExhausted and retires the call",
        );
        assert!(
            senders.lock().is_empty(),
            "the sender is removed — zero further delivery",
        );
        // …and nothing more is delivered.
        let mut small2 = Vec::new();
        RpcRequestChunkPayload {
            call_id: 7,
            flags: 0,
            headers: vec![],
            body: bytes::Bytes::from_static(b"5678"),
        }
        .encode_into(&mut small2);
        apply_request_chunk_to_senders(2, 3, Bytes::from(small2), &meta, &senders, "unit");
        {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            rt.block_on(async {
                assert!(
                    rx.try_recv().is_err(),
                    "no further delivery after the retire"
                );
            });
        }
        drop(lease);
    }

    /// S1_R Row 4 (F-4's closure) — §2.4's incarnation fencing at retire: a
    /// LATE retire/complete carrying the OLD incarnation is a no-op for the
    /// SUCCESSOR on a reused `(caller, call_id)`. The production twin of the
    /// S0 model's
    /// `late_operations_with_a_stale_incarnation_cannot_touch_the_successor`:
    /// the first record is retired and its supervisor-side cleanup completes
    /// it (the unit drives that single removal directly — there is no
    /// `record_count()` wait anywhere), the SAME key is reused while the
    /// first incarnation's cleanup-owner handles stay ARMED across the
    /// reuse, and every late op carrying the stale incarnation must be
    /// inert while the successor stays live end to end.
    ///
    /// Red witness (the S1_R brief Appendix inverse): **A4** (the
    /// `record.incarnation != incarnation ||` clause dropped from
    /// [`ProtectedCallRegistry::retire`]'s guard) reddens the late-retire
    /// no-op assert below — the stale op would settle the successor.
    #[test]
    fn late_retire_against_a_reused_key_is_a_no_op_for_the_successor() {
        // A real store gives the registry its live view (the model's
        // abstract authority); AV-9: the scratch dir is left behind.
        let dir = std::env::temp_dir().join(format!(
            "net-s1r-late-retire-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let store = crate::adapter::net::behavior::org_revocation::OrgRevocationStore::init(
            &dir,
            crate::adapter::net::behavior::org_revocation::ProvisioningExpectation::MayBeFresh,
        )
        .expect("real store");
        let registry = ProtectedCallRegistry::with_q1_defaults().expect("limits validate");
        registry.bind_store(None, Arc::new(store));
        let key = ProtectedCallKey {
            caller: EntityId::from_bytes([1u8; 32]),
            call_id: 10,
        };
        let opening = || OpeningRequest {
            key: key.clone(),
            session: SessionIdentity {
                peer: 2,
                session_id: 3,
                establishment: Some([4u8; 32]),
            },
            session_generation: Some(1),
            registration: 1,
            shape: RpcCallShape::ServerStreaming,
            now_ns: 0,
        };
        let facts = || VerifiedCallFacts {
            acting_org: crate::adapter::net::behavior::org::OrgId::from_bytes([9u8; 32]),
            member: EntityId::from_bytes([1u8; 32]),
            member_generation: 1,
            deadline: None,
        };

        // The first call — transferred to its supervisor, whose async
        // cleanup owns the record's single removal.
        let mut first_guard = registry.reserve(opening()).expect("reserve");
        let first_incarnation = first_guard.incarnation;
        let mut first_lease = registry
            .install(&mut first_guard, facts(), 0)
            .expect("install");
        let first_signal = Arc::new(StreamRetireSignal::new());
        registry
            .confirm(&mut first_lease, Arc::clone(&first_signal), None)
            .expect("the first call transfers to its supervisor");
        assert!(registry.retire(&key, first_incarnation, StreamTerminalReason::Cancelled));
        assert!(
            registry.complete(&key, first_incarnation),
            "the supervisor's cleanup removes the record exactly once",
        );

        // The key is reused by a fresh call — immediately, while the first
        // incarnation's cleanup-owner handles (its supervisor-side ref, the
        // armed late-op source) are still alive and can fire at any time.
        let mut second_guard = registry
            .reserve(opening())
            .expect("the reused key reserves");
        let second_incarnation = second_guard.incarnation;
        assert_ne!(second_incarnation, first_incarnation);
        let mut second_lease = registry
            .install(&mut second_guard, facts(), 0)
            .expect("the successor installs");
        let second_signal = Arc::new(StreamRetireSignal::new());
        let hook_fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook_count = Arc::clone(&hook_fired);
        registry
            .confirm(
                &mut second_lease,
                Arc::clone(&second_signal),
                Some(Arc::new(move |_reason| {
                    hook_count.fetch_add(1, Ordering::SeqCst);
                })),
            )
            .expect("the successor transfers to its supervisor");

        // Everything late from the first incarnation is inert.
        assert!(
            !registry.retire(&key, first_incarnation, StreamTerminalReason::Timeout),
            "a LATE retire carrying the old incarnation must be a no-op for the successor (§2.4)",
        );
        assert!(
            !registry.complete(&key, first_incarnation),
            "a LATE complete carrying the old incarnation must be a no-op",
        );
        assert!(
            !registry.release(&key, first_incarnation),
            "a LATE release carrying the old incarnation must be a no-op",
        );
        assert_eq!(
            registry.commit_check(&key, first_incarnation),
            CommitVerdict::Unknown,
            "the stale incarnation commits nothing",
        );

        // The successor survives, untouched.
        assert_eq!(
            registry.phase(&key),
            Some(RegistryPhase::Running),
            "the successor survives every late op",
        );
        assert_eq!(
            registry.terminal_reason(&key),
            None,
            "the successor is unsettled"
        );
        assert_eq!(
            second_signal.taken(),
            None,
            "the successor's owner was never signaled"
        );
        assert_eq!(
            hook_fired.load(Ordering::SeqCst),
            0,
            "the successor's on-retire hook never fired",
        );
        assert_eq!(
            registry.commit_check(&key, second_incarnation),
            CommitVerdict::Proceed,
        );
        assert_eq!(registry.removals(&key, second_incarnation), 0);

        // …and its own lifecycle still completes exactly once.
        assert!(registry.retire(&key, second_incarnation, StreamTerminalReason::Timeout));
        assert!(registry.complete(&key, second_incarnation));
        assert_eq!(registry.removals(&key, second_incarnation), 1);
    }
}
