//! The nRPC frame codec — a named second copy of the core's,
//! byte-for-byte, pinned from both directions.
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
//! Stage 4 grew the copy from the client half to the **full frame
//! codec** — a real browser leaf interoperates with a real native
//! core node in all four call shapes. Still core-only: the
//! checksum-verifying read path, the per-channel dispatcher map, and
//! the four streaming folds (lifecycle — this file is a codec).
//!
//! | | here | not here |
//! |---|---|---|
//! | [`EventMeta`] | 24-byte prefix | the checksum-verifying read path |
//! | route | the 8-byte `RpcRouteV1` discriminator | the dispatcher map |
//! | request | encode **and** decode | — |
//! | response | decode **and** encode | — |
//! | [`RpcStatus`] | both directions (it is 11 values) | — |
//! | streaming | chunks, both grants, markers, [`stream_terminal_payload`] | the four folds |
//!
//! **The pin, both directions.** A leaf-generated frame lives at
//! `net/crates/net/tests/cross_lang_wire/nrpc_frame.json`, and the
//! core's `cross_lang_wire` test decodes it with the production
//! `RpcRequestPayload::decode`, re-encodes it, and asserts the bytes
//! are identical — so a drift on either side reddens the other
//! side's suite. `tests/fixture_parity.rs` is this side of the same
//! fixture; `tests/nrpc_streaming_parity.rs` pins the streaming half
//! (chunks, grants, markers, terminals) byte for byte.
//!
//! Everything below is little-endian **except the two grant credit
//! fields**, which are big-endian `u32`s — the file's only
//! endianness exceptions.

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

/// Caller → server: response-direction flow-control credit for a
/// streaming call. `seq_or_ts` is the `call_id`; the payload is one
/// 4-byte **big-endian** `u32` credit count.
pub const DISPATCH_RPC_STREAM_GRANT: u8 = 0x14;

/// Caller → server: one upload chunk of a client-streaming request.
/// `seq_or_ts` is the `call_id`.
pub const DISPATCH_RPC_REQUEST_CHUNK: u8 = 0x15;

/// Server → caller: upload-direction flow-control credit. `seq_or_ts`
/// is the `call_id`; the 12-byte payload repeats it so the codec is
/// self-contained.
pub const DISPATCH_RPC_REQUEST_GRANT: u8 = 0x16;

/// `RpcRequestPayload::flags` bit 1: many `DISPATCH_RPC_RESPONSE`
/// frames are allowed (server-streaming / duplex).
pub const FLAG_RPC_STREAMING_RESPONSE: u16 = 1 << 1;

/// `RpcRequestPayload::flags` bit 2: `traceparent`/`tracestate`
/// headers are present.
pub const FLAG_RPC_PROPAGATE_TRACE: u16 = 1 << 2;

/// `RpcRequestPayload::flags` bit 4: `DISPATCH_RPC_REQUEST_CHUNK`
/// frames follow (client-streaming / duplex).
pub const FLAG_RPC_CLIENT_STREAMING_REQUEST: u16 = 1 << 4;

/// Request-flags bit 5: the terminal upload frame — on a chunk, or
/// on the initial REQUEST for the degenerate one-item client stream.
pub const FLAG_RPC_REQUEST_END: u16 = 1 << 5;

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

/// Streaming marker header name. Emitters write exactly this
/// lowercase spelling; receivers compare ASCII-case-insensitively.
pub const HEADER_NRPC_STREAMING: &str = "nrpc-streaming";

/// Marker value: more chunks follow. Byte-exact lowercase.
pub const HEADER_NRPC_STREAMING_CONTINUE: &[u8] = b"continue";

/// Marker value: the terminal chunk. Byte-exact lowercase.
pub const HEADER_NRPC_STREAMING_END: &[u8] = b"end";

/// Initial-REQUEST header carrying the response-direction window as
/// an ASCII-decimal `u32`. Absent ⇒ unbounded credit.
pub const HEADER_NRPC_STREAM_WINDOW_INITIAL: &str = "nrpc-stream-window-initial";

/// Initial-REQUEST header carrying the upload-direction window as an
/// ASCII-decimal `u32`. Absent ⇒ unbounded credit.
pub const HEADER_NRPC_REQUEST_WINDOW_INITIAL: &str = "nrpc-request-window-initial";

/// The 24-byte prefix every nRPC frame starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventMeta {
    /// Event classifier — one of the `DISPATCH_RPC_*` bytes.
    pub dispatch: u8,
    /// The *EventMeta* flag space — NOT [`RpcRequestPayload::flags`].
    /// Every nRPC publish site writes 0, so a leaf does too.
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

    /// Refuse anything core's `validate_wire_bounds` refuses: every
    /// `u8` / `u16` / `u32` length prefix, checked before a cast
    /// could truncate it. The production encoder `debug_assert`s
    /// these and truncates in release; the mirror refuses in all
    /// builds, because the caller is an application and an ambiguous
    /// encoding is not a programmer bug it can see.
    pub fn validate_wire_bounds(&self) -> Result<()> {
        check("service", self.service.len(), MAX_RPC_SERVICE_NAME_LEN)?;
        validate_headers_wire_bounds(&self.headers)?;
        check("body", self.body.len(), MAX_RPC_BODY_LEN)?;
        Ok(())
    }

    /// The encode gate: [`Self::validate_wire_bounds`] plus
    /// everything core's *decode* refuses (§12.3/§12.4 of the wire
    /// spec) — an empty service or header name would only travel the
    /// wire to be rejected there.
    pub fn validate(&self) -> Result<()> {
        self.validate_wire_bounds()?;
        if self.service.is_empty() {
            return Err(malformed("empty service name"));
        }
        validate_header_names(&self.headers)?;
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

    /// Decode the bytes that follow the `EventMeta` prefix and the
    /// route discriminator — the mirror of [`Self::encode_into`] and
    /// of core's `RpcRequestPayload::decode`, refusing exactly what
    /// it refuses (§12.3/§12.4 included).
    pub fn decode(data: Bytes) -> Result<Self> {
        let mut cur = std::io::Cursor::new(data.as_ref());
        // service
        if cur.remaining() < 1 {
            return Err(malformed("service length"));
        }
        let svc_len = cur.get_u8() as usize;
        if svc_len == 0 {
            return Err(malformed("empty service name"));
        }
        // Dead for a `u8` length prefix (0..=255 is exactly the cap),
        // but core carries the check and so does the mirror.
        if svc_len > MAX_RPC_SERVICE_NAME_LEN {
            return Err(malformed("service exceeds MAX_RPC_SERVICE_NAME_LEN"));
        }
        if cur.remaining() < svc_len {
            return Err(malformed("service bytes"));
        }
        let svc_start = cur.position() as usize;
        let svc_end = svc_start + svc_len;
        let service = std::str::from_utf8(&data[svc_start..svc_end])
            .map_err(|_| malformed("service is not UTF-8"))?
            .to_string();
        cur.set_position(svc_end as u64);
        // deadline_ns
        if cur.remaining() < 8 {
            return Err(malformed("deadline_ns"));
        }
        let deadline_ns = cur.get_u64_le();
        // flags
        if cur.remaining() < 2 {
            return Err(malformed("flags"));
        }
        let flags = cur.get_u16_le();
        // headers
        let headers = decode_headers(&mut cur)?;
        // body
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
        // Zero-copy slice over the input — refcount bump only.
        Ok(Self {
            service,
            deadline_ns,
            flags,
            headers,
            body: data.slice(start..start + body_len),
        })
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
    /// Encoded length, without encoding.
    pub fn encoded_len(&self) -> usize {
        2 + headers_len(&self.headers) + 4 + self.body.len()
    }

    /// Refuse anything core's `validate_wire_bounds` refuses: the
    /// length-prefix caps.
    pub fn validate_wire_bounds(&self) -> Result<()> {
        validate_headers_wire_bounds(&self.headers)?;
        check("body", self.body.len(), MAX_RPC_BODY_LEN)?;
        Ok(())
    }

    /// Append the wire bytes that follow the `EventMeta` prefix and
    /// the route discriminator. Refuses rather than truncates, like
    /// [`RpcRequestPayload::encode_into`].
    pub fn encode_into(&self, buf: &mut Vec<u8>) -> Result<()> {
        self.validate_wire_bounds()?;
        validate_header_names(&self.headers)?;
        buf.put_u16_le(self.status.to_wire());
        encode_headers(&self.headers, buf);
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
        Ok(())
    }

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

/// One upload chunk of a client-streaming (or duplex) call — the
/// [`DISPATCH_RPC_REQUEST_CHUNK`] payload. No `service` (the server
/// routed on the initial REQUEST) and no `deadline_ns` (the initial
/// REQUEST's deadline covers the call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcRequestChunkPayload {
    /// Matches the call's `call_id` — redundant with
    /// `EventMeta::seq_or_ts`, so the codec is self-contained.
    pub call_id: u64,
    /// Only [`FLAG_RPC_REQUEST_END`] is meaningful here.
    pub flags: u16,
    /// Same caps as [`RpcRequestPayload::headers`].
    pub headers: Vec<RpcHeader>,
    /// At most [`MAX_RPC_BODY_LEN`] per chunk.
    pub body: Bytes,
}

impl RpcRequestChunkPayload {
    /// Encoded length, without encoding.
    pub fn encoded_len(&self) -> usize {
        8 + 2 + headers_len(&self.headers) + 4 + self.body.len()
    }

    /// Refuse anything core's `validate_wire_bounds` refuses.
    pub fn validate_wire_bounds(&self) -> Result<()> {
        validate_headers_wire_bounds(&self.headers)?;
        check("body", self.body.len(), MAX_RPC_BODY_LEN)?;
        Ok(())
    }

    /// Append the wire bytes that follow the `EventMeta` prefix and
    /// the route discriminator. Refuses rather than truncates, like
    /// [`RpcRequestPayload::encode_into`].
    pub fn encode_into(&self, buf: &mut Vec<u8>) -> Result<()> {
        self.validate_wire_bounds()?;
        validate_header_names(&self.headers)?;
        buf.put_u64_le(self.call_id);
        buf.put_u16_le(self.flags);
        encode_headers(&self.headers, buf);
        buf.put_u32_le(self.body.len() as u32);
        buf.extend_from_slice(&self.body);
        Ok(())
    }

    /// Decode the bytes that follow the `EventMeta` prefix and the
    /// route discriminator.
    pub fn decode(data: Bytes) -> Result<Self> {
        let mut cur = std::io::Cursor::new(data.as_ref());
        // call_id
        if cur.remaining() < 8 {
            return Err(malformed("call_id"));
        }
        let call_id = cur.get_u64_le();
        // flags
        if cur.remaining() < 2 {
            return Err(malformed("flags"));
        }
        let flags = cur.get_u16_le();
        // headers
        let headers = decode_headers(&mut cur)?;
        // body
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
        // Zero-copy slice over the input — refcount bump only.
        Ok(Self {
            call_id,
            flags,
            headers,
            body: data.slice(start..start + body_len),
        })
    }
}

/// The [`DISPATCH_RPC_REQUEST_GRANT`] payload (server → caller):
/// upload-direction credit. Carries `call_id` redundantly with
/// `EventMeta::seq_or_ts` so the codec is self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcRequestGrantPayload {
    /// Matches the call's `call_id`.
    pub call_id: u64,
    /// Additional `DISPATCH_RPC_REQUEST_CHUNK` frames the server
    /// will admit beyond the current credit.
    pub credits: u32,
}

/// Encode a stream-grant payload — 4 bytes big-endian `u32`
/// representing additional credit. Pair with [`decode_stream_grant`]
/// on the server side. Big-endian is load-bearing: one of this
/// file's two endianness exceptions.
pub fn encode_stream_grant(amount: u32) -> Vec<u8> {
    amount.to_be_bytes().to_vec()
}

/// Decode a stream-grant payload. Returns `None` if the slice is not
/// exactly 4 bytes — defends the fold against malformed grants
/// without killing the adapter.
pub fn decode_stream_grant(payload: &[u8]) -> Option<u32> {
    if payload.len() != 4 {
        return None;
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(payload);
    Some(u32::from_be_bytes(bytes))
}

/// Encode a request-grant payload — `call_id` (u64 little-endian)
/// followed by additional credit (u32 **big-endian**). Big-endian on
/// the credit field matches [`encode_stream_grant`]; little-endian on
/// `call_id` matches the rest of the RPC codec's u64 fields.
pub fn encode_request_grant(call_id: u64, credits: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12);
    buf.put_u64_le(call_id);
    buf.extend_from_slice(&credits.to_be_bytes());
    buf
}

/// Decode a request-grant payload. Returns `None` if the slice is not
/// exactly 12 bytes.
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

/// Build a complete outbound RESPONSE frame:
/// `EventMeta ‖ RpcRouteV1 ‖ RpcResponsePayload`, the same envelope
/// [`encode_request_frame`] writes. `route` is the canonical `u64`
/// reply-channel hash (`<service>.replies.<caller_origin:016x>`).
pub fn encode_response_frame(
    origin_hash: u64,
    call_id: u64,
    route: u64,
    payload: &RpcResponsePayload,
) -> Result<Vec<u8>> {
    let meta = EventMeta::new(DISPATCH_RPC_RESPONSE, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + payload.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    payload.encode_into(&mut buf)?;
    Ok(buf)
}

/// Build a complete outbound REQUEST_CHUNK frame:
/// `EventMeta ‖ RpcRouteV1 ‖ RpcRequestChunkPayload`.
/// `EventMeta::seq_or_ts` is `payload.call_id`; `route` is the
/// request-channel hash, like [`encode_request_frame`].
pub fn encode_chunk_frame(
    origin_hash: u64,
    route: u64,
    payload: &RpcRequestChunkPayload,
) -> Result<Vec<u8>> {
    let meta = EventMeta::new(
        DISPATCH_RPC_REQUEST_CHUNK,
        0,
        origin_hash,
        payload.call_id,
        0,
    );
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + payload.encoded_len());
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    payload.encode_into(&mut buf)?;
    Ok(buf)
}

/// Build a complete outbound STREAM_GRANT frame — `EventMeta ‖
/// RpcRouteV1 ‖` 4-byte big-endian credit (36 bytes). Caller →
/// server on the request channel: response-direction credit.
pub fn encode_stream_grant_frame(
    origin_hash: u64,
    call_id: u64,
    route: u64,
    amount: u32,
) -> Vec<u8> {
    let meta = EventMeta::new(DISPATCH_RPC_STREAM_GRANT, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + 4);
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    buf.extend_from_slice(&encode_stream_grant(amount));
    buf
}

/// Build a complete outbound REQUEST_GRANT frame — `EventMeta ‖
/// RpcRouteV1 ‖` 12-byte grant (44 bytes). Server → caller on the
/// reply channel: upload-direction credit.
pub fn encode_request_grant_frame(
    origin_hash: u64,
    call_id: u64,
    route: u64,
    credits: u32,
) -> Vec<u8> {
    let meta = EventMeta::new(DISPATCH_RPC_REQUEST_GRANT, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET + 12);
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    buf.extend_from_slice(&encode_request_grant(call_id, credits));
    buf
}

/// Build a DEADLINE_EXCEEDED frame for `call_id` — the same 32-byte
/// `EventMeta ‖ route` envelope as [`encode_cancel_frame`], on the
/// reply channel. Empty payload by contract.
pub fn encode_deadline_exceeded_frame(origin_hash: u64, call_id: u64, route: u64) -> Vec<u8> {
    let meta = EventMeta::new(DISPATCH_RPC_DEADLINE_EXCEEDED, 0, origin_hash, call_id, 0);
    let mut buf = Vec::with_capacity(RPC_FRAME_BODY_OFFSET);
    buf.extend_from_slice(&meta.to_bytes());
    buf.extend_from_slice(&route.to_le_bytes());
    buf
}

/// One decoded inbound nRPC frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcFrame {
    /// A REQUEST for `payload.service` (the serve half).
    Request(RpcRequestPayload),
    /// A reply for `call_id` — one chunk of a streaming call is a
    /// `Response` too; [`classify_streaming_chunk`] tells them apart.
    Response {
        /// Correlation id, from `EventMeta::seq_or_ts`.
        call_id: u64,
        /// The decoded reply.
        payload: RpcResponsePayload,
    },
    /// The caller cancelled `call_id`. Empty payload.
    Cancel {
        /// Correlation id.
        call_id: u64,
    },
    /// The server gave up on `call_id`'s deadline. No payload.
    DeadlineExceeded {
        /// Correlation id.
        call_id: u64,
    },
    /// Response-direction flow-control credit for `call_id`.
    StreamGrant {
        /// Correlation id.
        call_id: u64,
        /// Additional response chunks the caller will admit
        /// (big-endian `u32` on the wire).
        credits: u32,
    },
    /// One upload chunk of a client-streaming request.
    RequestChunk(RpcRequestChunkPayload),
    /// Upload-direction flow-control credit.
    RequestGrant(RpcRequestGrantPayload),
}

/// Decode an inbound event-plane frame of **any** of the seven RPC
/// dispatches.
///
/// `Ok(None)` is a frame whose dispatch byte is none of the seven
/// `DISPATCH_RPC_*` values — an unknown dispatch is tolerated, not
/// refused. A known dispatch with a malformed payload is an `Err`.
///
/// Route rule: the five payload-carrying dispatches require the
/// `RpcRouteV1` discriminator; the two empty-payload ones (`CANCEL`,
/// `DEADLINE_EXCEEDED`) decode from `EventMeta` alone — the leaf's
/// documented decode-side leniency. Every builder in this file emits
/// the route on all seven.
pub fn decode_frame(frame: Bytes) -> Result<Option<RpcFrame>> {
    let meta = EventMeta::from_bytes(&frame).ok_or_else(|| malformed("frame shorter than 24"))?;
    let call_id = meta.seq_or_ts;
    let decoded = match meta.dispatch {
        DISPATCH_RPC_REQUEST => RpcFrame::Request(RpcRequestPayload::decode(frame_body(&frame)?)?),
        DISPATCH_RPC_RESPONSE => RpcFrame::Response {
            call_id,
            payload: RpcResponsePayload::decode(frame_body(&frame)?)?,
        },
        DISPATCH_RPC_CANCEL => RpcFrame::Cancel { call_id },
        DISPATCH_RPC_DEADLINE_EXCEEDED => RpcFrame::DeadlineExceeded { call_id },
        DISPATCH_RPC_STREAM_GRANT => {
            let credits = decode_stream_grant(&frame_body(&frame)?)
                .ok_or_else(|| malformed("stream grant payload is not 4 bytes"))?;
            RpcFrame::StreamGrant { call_id, credits }
        }
        DISPATCH_RPC_REQUEST_CHUNK => {
            RpcFrame::RequestChunk(RpcRequestChunkPayload::decode(frame_body(&frame)?)?)
        }
        DISPATCH_RPC_REQUEST_GRANT => {
            let grant = decode_request_grant(&frame_body(&frame)?)
                .ok_or_else(|| malformed("request grant payload is not 12 bytes"))?;
            RpcFrame::RequestGrant(grant)
        }
        _ => return Ok(None),
    };
    Ok(Some(decoded))
}

/// Decode an inbound event-plane frame if it is one of the three
/// server → caller dispatches: [`DISPATCH_RPC_RESPONSE`],
/// [`DISPATCH_RPC_DEADLINE_EXCEEDED`], [`DISPATCH_RPC_REQUEST_GRANT`].
///
/// `Ok(None)` is a frame whose dispatch heads the other direction or
/// is unknown — a REQUEST, CANCEL, STREAM_GRANT, REQUEST_CHUNK or
/// unknown byte. Their payloads are NOT inspected: this wrapper's
/// contract is to answer "is this for the client half?" and nothing
/// else; [`decode_frame`] is the strict all-dispatch decoder
/// underneath it.
pub fn decode_reply_frame(frame: Bytes) -> Result<Option<RpcFrame>> {
    if let Some(meta) = EventMeta::from_bytes(&frame) {
        if !matches!(
            meta.dispatch,
            DISPATCH_RPC_RESPONSE | DISPATCH_RPC_DEADLINE_EXCEEDED | DISPATCH_RPC_REQUEST_GRANT
        ) {
            return Ok(None);
        }
    }
    decode_frame(frame)
}

/// The `RpcRouteV1` discriminator of a frame, when it carries one.
pub fn decode_route(frame: &[u8]) -> Option<u64> {
    let bytes = frame.get(EVENT_META_SIZE..RPC_FRAME_BODY_OFFSET)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// What one response payload means for a streaming call's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingChunkKind {
    /// `nrpc-streaming: continue` — more chunks follow.
    Continue,
    /// The terminal frame: `nrpc-streaming: end`, an unknown marker
    /// value (defensive — a misbehaving server must not keep a
    /// stream open forever), or a non-`Ok` status.
    Terminal,
    /// No streaming marker at all: unary semantics (the caller used
    /// `call`, not `call_streaming`).
    Unary,
}

/// Classify one response against the marker rules.
///
/// Non-`Ok` is always terminal. Otherwise the marker header decides,
/// with the §12.11 mirror rule applied: header names compare
/// ASCII-case-**in**sensitively (core's two classifiers disagree on
/// case; the mirror accepts the union), marker **values** compare
/// byte-exactly, and emitters write the lowercase constants above.
pub fn classify_streaming_chunk(resp: &RpcResponsePayload) -> StreamingChunkKind {
    // Non-Ok status is always terminal regardless of header — the
    // stream surfaces the error and closes.
    if !resp.status.is_ok() {
        return StreamingChunkKind::Terminal;
    }
    // Walk headers for the streaming marker. Absence = unary
    // semantics.
    for (name, value) in &resp.headers {
        if name.eq_ignore_ascii_case(HEADER_NRPC_STREAMING) {
            return if value.as_slice() == HEADER_NRPC_STREAMING_END {
                StreamingChunkKind::Terminal
            } else if value.as_slice() == HEADER_NRPC_STREAMING_CONTINUE {
                StreamingChunkKind::Continue
            } else {
                // Unknown marker value — be defensive, treat as
                // terminal so a misbehaving server doesn't keep a
                // stream open forever.
                StreamingChunkKind::Terminal
            };
        }
    }
    StreamingChunkKind::Unary
}

/// A handler's own terminal result, carried verbatim by
/// [`StreamTerminalReason::Completed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamHandlerResult {
    /// Clean completion: `Ok` + `nrpc-streaming: end` + empty body.
    Ok,
    /// The handler's typed failure: its status and UTF-8 diagnostic,
    /// emitted verbatim.
    Err(RpcStatus, String),
}

/// Why a streaming call ended — core's `StreamTerminalReason`, named
/// verbatim. The mapping onto the frozen wire vocabulary lives in
/// [`stream_terminal_payload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTerminalReason {
    /// The handler finished and the pump drained. Carries the
    /// handler's own result: an `Err` handler yields
    /// `Completed(Err(..))`, never `Ok`.
    Completed(StreamHandlerResult),
    /// Caller CANCEL, or the caller handle dropped.
    Cancelled,
    /// The effective deadline fired under the deadline bound.
    Timeout,
    /// The effective deadline fired under the credential bound —
    /// authority lapsed rather than time running out.
    CredentialExpired,
    /// A revocation floor rose past this call's member generation.
    Revoked,
    /// The authority or revocation store moved, was removed, or is
    /// poisoned: fail closed.
    AuthorityUnavailable,
    /// An admitted item could neither be reserved nor delivered.
    ResourceExhausted,
    /// The peer's session was replaced or the peer disconnected.
    SessionReplaced,
    /// The registration's `ServeHandle` dropped, or the node shut
    /// down.
    ServeHandleDropped,
    /// The pump stopped without the handler returning — a typed
    /// failure, never successful completion.
    PumpFailed,
}

impl StreamTerminalReason {
    /// Whether already-queued response items are published before the
    /// terminal. Only a genuine completion drains; every retirement
    /// discards ("no synthetic success").
    pub fn drains_queued_output(&self) -> bool {
        matches!(self, StreamTerminalReason::Completed(_))
    }
}

/// The terminal frame one selected reason emits — core's
/// `stream_terminal_payload`, mirrored byte for byte.
///
/// `Completed` carries the handler's own result verbatim; every
/// retirement maps onto the frozen wire vocabulary. `AdmissionDenied`
/// bodies are the single **coarse byte** (`0` = `Denied`, `2` =
/// `Unavailable` — the coarse enum itself is the org lane's). Error
/// and retirement terminals are terminal by non-`Ok` status and carry
/// NO streaming header; only `Completed(Ok)` carries
/// `nrpc-streaming: end`.
pub fn stream_terminal_payload(reason: &StreamTerminalReason) -> RpcResponsePayload {
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

/// Core's `validate_wire_bounds` header pass: the length-prefix caps
/// only.
fn validate_headers_wire_bounds(headers: &[RpcHeader]) -> Result<()> {
    check("headers", headers.len(), MAX_RPC_HEADERS)?;
    for (name, value) in headers {
        check("header name", name.len(), MAX_RPC_HEADER_NAME_LEN)?;
        check("header value", value.len(), MAX_RPC_HEADER_VALUE_LEN)?;
    }
    Ok(())
}

/// §12.3's encode-side half: core's decode rejects an empty header
/// name, so no encoder here may emit one.
fn validate_header_names(headers: &[RpcHeader]) -> Result<()> {
    for (name, _) in headers {
        if name.is_empty() {
            return Err(malformed("empty header name"));
        }
    }
    Ok(())
}

/// The payload region of `frame`: past the `EventMeta` prefix AND the
/// `RpcRouteV1` discriminator.
fn frame_body(frame: &Bytes) -> Result<Bytes> {
    if frame.len() < RPC_FRAME_BODY_OFFSET {
        return Err(malformed("frame carries no route discriminator"));
    }
    Ok(frame.slice(RPC_FRAME_BODY_OFFSET..))
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
        // §12.3: core's decode rejects `name_len == 0`
        // (`Truncated("empty header name")`); the mirror must refuse
        // exactly what core refuses.
        if name_len == 0 {
            return Err(malformed("empty header name"));
        }
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
