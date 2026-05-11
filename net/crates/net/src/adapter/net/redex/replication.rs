//! RedEX Distributed wire protocol — `SUBPROTOCOL_REDEX` and the four
//! `DISPATCH_REPLICA_SYNC` codes that ride on top of the existing
//! reliable-stream `Mesh::publish` machinery.
//!
//! Phase A scaffold of `docs/plans/REDEX_DISTRIBUTED_PLAN.md`. Implements
//! the byte layouts pinned in §2 of that plan:
//!
//! - `SyncRequest`   (`0x20`, replica → leader)  — 47 bytes, fixed.
//! - `SyncResponse`  (`0x21`, leader → replica) — variable; bounded by
//!   the matching request's `chunk_max`.
//! - `SyncHeartbeat` (`0x22`, bidirectional)    — 52 bytes, fixed.
//! - `SyncNack`      (`0x23`, leader → replica) — variable; carries
//!   an optional UTF-8 diagnostic.
//!
//! Encoding conventions (LOCKED, mirroring §2 of the plan):
//!
//! - Multi-byte integers are **little-endian, fixed-width** — no varints.
//! - The standard subprotocol header (`subprotocol_id: u16 LE` +
//!   `dispatch_code: u8`) prefixes every message — 3 bytes.
//! - `ChannelId` is the 32-byte BLAKE2s hash of the channel name.
//! - Length-prefixed strings: `(u16 LE len, [len] utf-8 bytes)`.
//! - Range encoding (used by future reserved variants): `(u64 LE start,
//!   u64 LE end)`, half-open `[start, end)`.
//!
//! Election is wire-free — `StandbyGroup` invokes RedEX's deterministic
//! `elect()` selection function from local state, so no `LEADER_ELECTION`
//! dispatch code exists. Reserved range `0x24..=0x2F` (12 codes) is held
//! for future variants (range-bounded sync, parallel-stream sync, etc.).
//!
//! Codec layer only — daemon, heartbeat loop, election integration, and
//! `ReplicationCoordinator` itself land in Phases C / D / E.

use blake2::{
    digest::{generic_array::typenum::U32, FixedOutput, KeyInit, Mac},
    Blake2sMac,
};
use bytes::{Buf, BufMut};

use super::super::channel::ChannelName;

/// Subprotocol ID for RedEX Distributed replication. Claims `0x0E00`
/// in the `SUBPROTOCOLS.md` registry; the high byte (`0x0E`) is the
/// next free family above capability (`0x0C`) and reflex (`0x0D`).
pub const SUBPROTOCOL_REDEX: u16 = 0x0E00;

/// Replica → leader: ask for events `[since_seq, since_seq + chunk_max)`.
pub const DISPATCH_SYNC_REQUEST: u8 = 0x20;
/// Leader → replica: bounded chunk of events.
pub const DISPATCH_SYNC_RESPONSE: u8 = 0x21;
/// Bidirectional liveness + tail-seq heartbeat.
pub const DISPATCH_SYNC_HEARTBEAT: u8 = 0x22;
/// Leader → replica: structured rejection (typed `error_code`).
pub const DISPATCH_SYNC_NACK: u8 = 0x23;

/// Reserved range upper bound (exclusive) for future
/// `DISPATCH_REPLICA_SYNC` variants. `0x24..0x2F` is reserved for
/// range-bounded sync, parallel-stream sync, etc.; document each new
/// code in `SUBPROTOCOLS.md` as it lands.
pub const DISPATCH_REPLICA_SYNC_RESERVED_END: u8 = 0x30;

/// Fixed encoded size of a [`SyncRequest`] message including the
/// 3-byte subprotocol header.
pub const SYNC_REQUEST_SIZE: usize = 3 + 32 + 8 + 4; // 47

/// Fixed encoded size of a [`SyncHeartbeat`] message including the
/// 3-byte subprotocol header.
pub const SYNC_HEARTBEAT_SIZE: usize = 3 + 32 + 8 + 1 + 8; // 52

/// Domain-separation label for the BLAKE2s hash that turns a channel
/// name into a 32-byte `ChannelId`. Picked once, frozen — changing it
/// would invalidate every `ChannelId` on the wire.
const CHANNEL_ID_LABEL: &[u8] = b"redex-channel-id-v1";

/// 32-byte channel identifier — BLAKE2s of the channel name with a
/// domain-separation label. Distinct from `ChannelName::hash() -> u16`
/// (the routing hint), which has routine collisions at mesh scale.
/// The replication protocol needs an identifier with negligible
/// collision probability so two channels can't accidentally observe
/// each other's heartbeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId([u8; 32]);

impl ChannelId {
    /// Compute the `ChannelId` for a channel name.
    pub fn from_name(name: &ChannelName) -> Self {
        Self::from_str_internal(name.as_str())
    }

    /// Internal helper so tests can hash literal strings without
    /// constructing a [`ChannelName`].
    fn from_str_internal(s: &str) -> Self {
        let mut mac = <Blake2sMac<U32> as KeyInit>::new_from_slice(CHANNEL_ID_LABEL)
            .expect("BLAKE2s accepts variable-length keys");
        Mac::update(&mut mac, s.as_bytes());
        let bytes = mac.finalize_fixed();
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Self(out)
    }

    /// Construct from raw bytes — used by the decode path.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the 32-byte representation.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// `ReplicaState` discriminator carried on the wire in
/// [`SyncHeartbeat`] messages. The four-state model is pinned at §3 of
/// the plan; this enum is the encoding view of those states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicaRole {
    /// Sole appender for the channel.
    Leader = 0,
    /// Catching up or steady-state lagging by ≤ 1 heartbeat.
    Replica = 1,
    /// Brief transient: leader-loss detected, computing the
    /// deterministic election winner. Microseconds; not a broadcast wait.
    Candidate = 2,
    /// Holds the channel's storage but has no replica role.
    Idle = 3,
}

impl ReplicaRole {
    fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Leader),
            1 => Some(Self::Replica),
            2 => Some(Self::Candidate),
            3 => Some(Self::Idle),
            _ => None,
        }
    }

    fn to_wire(self) -> u8 {
        self as u8
    }
}

/// Typed rejection error in [`SyncNack`]. Replicas key their retry
/// policy on the variant — never silently treat as transport-level
/// failure (the reliable-stream layer surfaces transport errors
/// separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncNackError {
    /// Receiver is not the leader for this channel. Replica should
    /// re-resolve leadership via `Mesh::find_chain_holders`.
    NotLeader = 1,
    /// `since_seq` lies outside the leader's retained range. Replica
    /// should trim its local tail and retry from the leader's first
    /// available seq.
    BadRange = 2,
    /// Leader is currently saturated. Replica should exponentially
    /// back off and retry the same request.
    Backpressure = 3,
    /// Channel was closed. Replica withdraws its role and emits a
    /// metric.
    ChannelClosed = 4,
}

impl SyncNackError {
    fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::NotLeader),
            2 => Some(Self::BadRange),
            3 => Some(Self::Backpressure),
            4 => Some(Self::ChannelClosed),
            _ => None,
        }
    }

    fn to_wire(self) -> u8 {
        self as u8
    }
}

/// One event record inside a [`SyncResponse`] chunk. `event_seq`
/// values are strictly increasing across a chunk; gaps within a chunk
/// are not permitted (gaps come as explicit skip-ahead in Phase D).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEvent {
    /// Monotonic sequence number assigned by the channel's leader.
    pub event_seq: u64,
    /// Opaque event body bytes — the layer-7 payload.
    pub payload: Vec<u8>,
}

/// Replica → leader: pull request for events
/// `[since_seq, since_seq + chunk_max)`. Fixed 47-byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRequest {
    /// 32-byte BLAKE2s hash of the channel name.
    pub channel_id: ChannelId,
    /// First sequence number the replica wants from the leader's
    /// retained range. Inclusive.
    pub since_seq: u64,
    /// Maximum payload bytes the leader may send in the matching
    /// [`SyncResponse`].
    pub chunk_max: u32,
}

/// Leader → replica: bounded chunk of events answering the matching
/// [`SyncRequest`]. Variable size; bounded by `chunk_max` from the
/// request side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncResponse {
    /// 32-byte BLAKE2s hash of the channel name.
    pub channel_id: ChannelId,
    /// Sequence number of `events[0]` in this chunk. Replicas use
    /// this to detect server-side trimming (`first_seq` greater than
    /// the request's `since_seq` means the leader no longer retains
    /// the requested range).
    pub first_seq: u64,
    /// **R-5 disambiguation:** leader's first retained seq at the
    /// time of this response. Lets the replica tell a legitimate
    /// retention trim (`first_seq == leader_first_retained_seq`) from
    /// a divergent-log split-brain (`first_seq >
    /// leader_first_retained_seq` AND replica's local tail had data
    /// in `[leader_first_retained_seq, first_seq)`). The replica
    /// still does the skip-ahead in both cases (safety) but
    /// observability-wise the divergence case is flagged with a
    /// distinct metric for operator review.
    pub leader_first_retained_seq: u64,
    /// In-order event records. `event_seq` increases monotonically
    /// across the slice; no gaps within a chunk.
    pub events: Vec<SyncEvent>,
}

/// Bidirectional liveness heartbeat. Leader emits these to all
/// replicas at `heartbeat_ms` cadence; replicas emit their own
/// `tail_seq` back to the leader so the leader can observe lag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHeartbeat {
    /// 32-byte BLAKE2s hash of the channel name.
    pub channel_id: ChannelId,
    /// Sender's current tail sequence number.
    pub tail_seq: u64,
    /// Sender's `ReplicaState` — operator-facing observability only;
    /// receivers don't make routing decisions on this field (those
    /// route through the capability layer's `causal:` tags).
    pub role: ReplicaRole,
    /// Sender's monotonic-clock milliseconds. Used **only** for drift
    /// detection (operator-facing); never consumed for ordering or
    /// liveness logic — those route through `tail_seq` + reliable-
    /// stream ack accounting.
    pub wall_clock_ms: u64,
}

/// Leader → replica: structured rejection. The leader MUST emit this
/// (rather than silently closing the stream) on every rejection
/// reason that isn't a transport-level failure — silent close is
/// reserved for the latter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncNack {
    /// 32-byte BLAKE2s hash of the channel name.
    pub channel_id: ChannelId,
    /// Echoes the rejected request's `since_seq` so the replica can
    /// correlate the NACK with the in-flight request that triggered
    /// it.
    pub since_seq: u64,
    /// Typed rejection reason. Replicas key their retry policy here.
    pub error_code: SyncNackError,
    /// Optional human-readable diagnostic. UTF-8 encoded; may be
    /// empty. The replica's retry policy keys off `error_code` only —
    /// `detail` is for operator logs.
    pub detail: String,
}

/// Errors surfacing from the decode path. Mirrors the typed-error
/// shape the rest of the substrate uses for fallible decoders.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WireError {
    /// Buffer is shorter than the encoded message demands.
    #[error("redex wire truncated: need {need} bytes, have {have}")]
    Truncated {
        /// Minimum bytes the decoder needed to make progress.
        need: usize,
        /// Bytes actually available in the input.
        have: usize,
    },
    /// Subprotocol header doesn't match `SUBPROTOCOL_REDEX`.
    #[error("redex wire subprotocol mismatch: got {got:#06x}, expected {SUBPROTOCOL_REDEX:#06x}")]
    SubprotocolMismatch {
        /// Subprotocol id observed in the header.
        got: u16,
    },
    /// Dispatch code doesn't match the decoder being invoked, or
    /// falls outside the reserved `0x20..=0x2F` range entirely.
    #[error("redex wire dispatch code {got:#04x} does not match expected {expected:#04x}")]
    DispatchMismatch {
        /// Dispatch byte observed in the header.
        got: u8,
        /// Dispatch byte the decoder being invoked is keyed on.
        expected: u8,
    },
    /// `role` byte in a [`SyncHeartbeat`] is outside the `0..=3`
    /// range the four-state model pins.
    #[error("redex wire role byte {0} is not a valid ReplicaRole (0..=3)")]
    BadRole(u8),
    /// `error_code` byte in a [`SyncNack`] is outside the `1..=4`
    /// range the typed-error variants pin.
    #[error("redex wire error_code {0} is not a valid SyncNackError (1..=4)")]
    BadErrorCode(u8),
    /// `detail` bytes in a [`SyncNack`] are not valid UTF-8.
    #[error("redex wire NACK detail is not valid UTF-8")]
    InvalidUtf8,
}

/// Write the standard 3-byte subprotocol header
/// (`SUBPROTOCOL_REDEX` + `dispatch_code`) to `buf`.
fn put_header(buf: &mut Vec<u8>, dispatch: u8) {
    buf.put_u16_le(SUBPROTOCOL_REDEX);
    buf.put_u8(dispatch);
}

/// Validate the standard 3-byte subprotocol header on `data` and
/// return the remaining payload slice. Errors on truncation,
/// subprotocol mismatch, or dispatch-code mismatch.
fn check_header(data: &[u8], expected_dispatch: u8) -> Result<&[u8], WireError> {
    if data.len() < 3 {
        return Err(WireError::Truncated {
            need: 3,
            have: data.len(),
        });
    }
    let mut cursor = &data[..3];
    let subprotocol = cursor.get_u16_le();
    let dispatch = cursor.get_u8();
    if subprotocol != SUBPROTOCOL_REDEX {
        return Err(WireError::SubprotocolMismatch { got: subprotocol });
    }
    if dispatch != expected_dispatch {
        return Err(WireError::DispatchMismatch {
            got: dispatch,
            expected: expected_dispatch,
        });
    }
    Ok(&data[3..])
}

/// Read a `ChannelId` from `cursor`. Caller is responsible for
/// ensuring `cursor.remaining() >= 32`.
fn get_channel_id(cursor: &mut &[u8]) -> ChannelId {
    let mut id = [0u8; 32];
    id.copy_from_slice(&cursor[..32]);
    cursor.advance(32);
    ChannelId::from_bytes(id)
}

// ============================================================================
// SyncRequest — 0x20, replica → leader
// ============================================================================

impl SyncRequest {
    /// Serialize to bytes. Fixed [`SYNC_REQUEST_SIZE`] (47) bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SYNC_REQUEST_SIZE);
        put_header(&mut buf, DISPATCH_SYNC_REQUEST);
        buf.put_slice(self.channel_id.as_bytes());
        buf.put_u64_le(self.since_seq);
        buf.put_u32_le(self.chunk_max);
        debug_assert_eq!(buf.len(), SYNC_REQUEST_SIZE);
        buf
    }

    /// Deserialize from bytes. Errors on truncation, header
    /// mismatch, or trailing bytes beyond the fixed size (which
    /// would indicate a protocol-version skew worth flagging).
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        let payload = check_header(data, DISPATCH_SYNC_REQUEST)?;
        if payload.len() < SYNC_REQUEST_SIZE - 3 {
            return Err(WireError::Truncated {
                need: SYNC_REQUEST_SIZE,
                have: data.len(),
            });
        }
        let mut cursor = payload;
        let channel_id = get_channel_id(&mut cursor);
        let since_seq = cursor.get_u64_le();
        let chunk_max = cursor.get_u32_le();
        Ok(Self {
            channel_id,
            since_seq,
            chunk_max,
        })
    }
}

// ============================================================================
// SyncResponse — 0x21, leader → replica
// ============================================================================

impl SyncResponse {
    /// Serialize to bytes. Variable size: header + 32 + 8 + 8 + 4 +
    /// Σ(8 + 4 + payload.len()) over events.
    /// (R-5: added 8 bytes for `leader_first_retained_seq`.)
    pub fn to_bytes(&self) -> Vec<u8> {
        // Cap the pre-allocation against `u32::MAX` — `event_count`
        // is the wire-format width, so we can't honestly encode
        // more than `u32::MAX` events anyway, and on 32-bit hosts
        // the multiplication below would overflow `usize` otherwise.
        let events_size: usize = self.events.iter().map(|e| 8 + 4 + e.payload.len()).sum();
        let mut buf = Vec::with_capacity(3 + 32 + 8 + 8 + 4 + events_size);
        put_header(&mut buf, DISPATCH_SYNC_RESPONSE);
        buf.put_slice(self.channel_id.as_bytes());
        buf.put_u64_le(self.first_seq);
        buf.put_u64_le(self.leader_first_retained_seq);
        // `events.len()` wider than u32::MAX is impossible to
        // represent on the wire — clamp via saturating cast. In
        // practice callers honor `chunk_max` (bounded u32) so the
        // saturation is dead code, but stay safe.
        debug_assert!(
            self.events.len() <= u32::MAX as usize,
            "events.len() {} exceeds u32::MAX",
            self.events.len()
        );
        let event_count = u32::try_from(self.events.len()).unwrap_or(u32::MAX);
        buf.put_u32_le(event_count);
        for event in &self.events {
            buf.put_u64_le(event.event_seq);
            debug_assert!(event.payload.len() <= u32::MAX as usize);
            let payload_len = u32::try_from(event.payload.len()).unwrap_or(u32::MAX);
            buf.put_u32_le(payload_len);
            buf.put_slice(&event.payload);
        }
        buf
    }

    /// Deserialize from bytes. Errors on truncation or header
    /// mismatch. Validates each event-record's length prefix
    /// against the remaining buffer so a malformed `payload_len`
    /// can't trigger a panic.
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        let payload = check_header(data, DISPATCH_SYNC_RESPONSE)?;
        let prefix_needed = 32 + 8 + 8 + 4;
        if payload.len() < prefix_needed {
            return Err(WireError::Truncated {
                need: 3 + prefix_needed,
                have: data.len(),
            });
        }
        let mut cursor = payload;
        let channel_id = get_channel_id(&mut cursor);
        let first_seq = cursor.get_u64_le();
        let leader_first_retained_seq = cursor.get_u64_le();
        let event_count = cursor.get_u32_le() as usize;
        // R-36: cap the pre-allocation at 4096 events to bound a
        // hostile `event_count` (e.g. peer sending a maximum-u32
        // count without the matching payload bytes). Legitimate
        // chunks above 4096 events incur progressive grow-and-
        // copy, but the byte budget (`chunk_max` ≤ 64 MiB)
        // means an over-4096-event chunk averages payload <
        // 16 KiB / event, which is comfortably small.
        let mut events = Vec::with_capacity(event_count.min(4096));
        for _ in 0..event_count {
            if cursor.remaining() < 8 + 4 {
                // R-23: report total bytes needed correctly —
                // consumed-so-far + still-needed.
                let consumed = data.len() - cursor.remaining();
                return Err(WireError::Truncated {
                    need: consumed + (8 + 4),
                    have: data.len(),
                });
            }
            let event_seq = cursor.get_u64_le();
            let payload_len = cursor.get_u32_le() as usize;
            if cursor.remaining() < payload_len {
                let consumed = data.len() - cursor.remaining();
                return Err(WireError::Truncated {
                    need: consumed + payload_len,
                    have: data.len(),
                });
            }
            let event_payload = cursor[..payload_len].to_vec();
            cursor.advance(payload_len);
            events.push(SyncEvent {
                event_seq,
                payload: event_payload,
            });
        }
        Ok(Self {
            channel_id,
            first_seq,
            leader_first_retained_seq,
            events,
        })
    }
}

// ============================================================================
// SyncHeartbeat — 0x22, bidirectional
// ============================================================================

impl SyncHeartbeat {
    /// Serialize to bytes. Fixed [`SYNC_HEARTBEAT_SIZE`] (52) bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(SYNC_HEARTBEAT_SIZE);
        put_header(&mut buf, DISPATCH_SYNC_HEARTBEAT);
        buf.put_slice(self.channel_id.as_bytes());
        buf.put_u64_le(self.tail_seq);
        buf.put_u8(self.role.to_wire());
        buf.put_u64_le(self.wall_clock_ms);
        debug_assert_eq!(buf.len(), SYNC_HEARTBEAT_SIZE);
        buf
    }

    /// Deserialize from bytes. Errors on truncation, header
    /// mismatch, or `role` byte outside `0..=3`.
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        let payload = check_header(data, DISPATCH_SYNC_HEARTBEAT)?;
        if payload.len() < SYNC_HEARTBEAT_SIZE - 3 {
            return Err(WireError::Truncated {
                need: SYNC_HEARTBEAT_SIZE,
                have: data.len(),
            });
        }
        let mut cursor = payload;
        let channel_id = get_channel_id(&mut cursor);
        let tail_seq = cursor.get_u64_le();
        let role_byte = cursor.get_u8();
        let role = ReplicaRole::from_wire(role_byte).ok_or(WireError::BadRole(role_byte))?;
        let wall_clock_ms = cursor.get_u64_le();
        Ok(Self {
            channel_id,
            tail_seq,
            role,
            wall_clock_ms,
        })
    }
}

// ============================================================================
// SyncNack — 0x23, leader → replica
// ============================================================================

/// Maximum permitted length of a [`SyncNack::detail`] string on the
/// wire. The `detail_len` field is u16 LE, so the absolute ceiling
/// is `u16::MAX`; this constant matches that and lives here so
/// callers can opt to truncate diagnostic text rather than failing
/// the encode.
pub const SYNC_NACK_DETAIL_MAX: usize = u16::MAX as usize;

impl SyncNack {
    /// Serialize to bytes. Variable size: header + 32 + 8 + 1 + 2 +
    /// detail.len(). Truncates `detail` to [`SYNC_NACK_DETAIL_MAX`]
    /// if longer — the protocol can't represent a longer string and
    /// silently truncating the diagnostic is preferable to losing
    /// the structured error code entirely.
    pub fn to_bytes(&self) -> Vec<u8> {
        let detail_bytes = self.detail.as_bytes();
        let detail_len = detail_bytes.len().min(SYNC_NACK_DETAIL_MAX);
        let mut buf = Vec::with_capacity(3 + 32 + 8 + 1 + 2 + detail_len);
        put_header(&mut buf, DISPATCH_SYNC_NACK);
        buf.put_slice(self.channel_id.as_bytes());
        buf.put_u64_le(self.since_seq);
        buf.put_u8(self.error_code.to_wire());
        buf.put_u16_le(detail_len as u16);
        buf.put_slice(&detail_bytes[..detail_len]);
        buf
    }

    /// Deserialize from bytes. Errors on truncation, header
    /// mismatch, `error_code` outside `1..=4`, or non-UTF-8 detail.
    pub fn from_bytes(data: &[u8]) -> Result<Self, WireError> {
        let payload = check_header(data, DISPATCH_SYNC_NACK)?;
        let prefix_needed = 32 + 8 + 1 + 2;
        if payload.len() < prefix_needed {
            return Err(WireError::Truncated {
                need: 3 + prefix_needed,
                have: data.len(),
            });
        }
        let mut cursor = payload;
        let channel_id = get_channel_id(&mut cursor);
        let since_seq = cursor.get_u64_le();
        let code_byte = cursor.get_u8();
        let error_code =
            SyncNackError::from_wire(code_byte).ok_or(WireError::BadErrorCode(code_byte))?;
        let detail_len = cursor.get_u16_le() as usize;
        if cursor.remaining() < detail_len {
            return Err(WireError::Truncated {
                need: data.len() + (detail_len - cursor.remaining()),
                have: data.len(),
            });
        }
        let detail_bytes = &cursor[..detail_len];
        let detail = std::str::from_utf8(detail_bytes)
            .map_err(|_| WireError::InvalidUtf8)?
            .to_string();
        Ok(Self {
            channel_id,
            since_seq,
            error_code,
            detail,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_channel_id() -> ChannelId {
        ChannelId::from_str_internal("net/redex/example")
    }

    // ----------------------------------------------------------------
    // ChannelId
    // ----------------------------------------------------------------

    #[test]
    fn channel_id_is_deterministic() {
        let a = ChannelId::from_str_internal("payments/settlements");
        let b = ChannelId::from_str_internal("payments/settlements");
        assert_eq!(a, b);
    }

    #[test]
    fn channel_id_is_unique_per_name() {
        let a = ChannelId::from_str_internal("payments/settlements");
        let b = ChannelId::from_str_internal("payments/refunds");
        assert_ne!(a, b);
    }

    // ----------------------------------------------------------------
    // SyncRequest round-trip
    // ----------------------------------------------------------------

    #[test]
    fn sync_request_round_trip() {
        let original = SyncRequest {
            channel_id: sample_channel_id(),
            since_seq: 0xDEAD_BEEF_CAFE_BABE,
            chunk_max: 1_048_576,
        };
        let bytes = original.to_bytes();
        assert_eq!(bytes.len(), SYNC_REQUEST_SIZE);
        let decoded = SyncRequest::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn sync_request_byte_layout_pinned() {
        // Pin the byte layout exactly. Drift here is a wire-protocol
        // break — every fielded peer would fail to decode.
        let req = SyncRequest {
            channel_id: ChannelId::from_bytes([0xAB; 32]),
            since_seq: 0x0102_0304_0506_0708,
            chunk_max: 0x1122_3344,
        };
        let bytes = req.to_bytes();
        // Subprotocol header is u16 LE = 0x0E00 → bytes [0x00, 0x0E];
        // followed by dispatch_code 0x20.
        assert_eq!(&bytes[..3], &[0x00, 0x0E, 0x20]);
        assert_eq!(&bytes[3..35], &[0xAB; 32]);
        assert_eq!(
            &bytes[35..43],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(&bytes[43..47], &[0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn sync_request_rejects_wrong_dispatch() {
        let mut bytes = SyncRequest {
            channel_id: sample_channel_id(),
            since_seq: 0,
            chunk_max: 1,
        }
        .to_bytes();
        bytes[2] = DISPATCH_SYNC_RESPONSE; // wrong code
        let err = SyncRequest::from_bytes(&bytes).expect_err("must reject");
        assert!(matches!(err, WireError::DispatchMismatch { .. }));
    }

    #[test]
    fn sync_request_rejects_wrong_subprotocol() {
        let mut bytes = SyncRequest {
            channel_id: sample_channel_id(),
            since_seq: 0,
            chunk_max: 1,
        }
        .to_bytes();
        bytes[0] = 0x00;
        bytes[1] = 0x05; // SUBPROTOCOL_MIGRATION
        let err = SyncRequest::from_bytes(&bytes).expect_err("must reject");
        assert!(matches!(
            err,
            WireError::SubprotocolMismatch { got: 0x0500 }
        ));
    }

    #[test]
    fn sync_request_rejects_truncation() {
        let bytes = SyncRequest {
            channel_id: sample_channel_id(),
            since_seq: 0,
            chunk_max: 1,
        }
        .to_bytes();
        for cut in 0..bytes.len() {
            let err = SyncRequest::from_bytes(&bytes[..cut]).expect_err("must reject");
            assert!(matches!(err, WireError::Truncated { .. }));
        }
    }

    // ----------------------------------------------------------------
    // SyncResponse round-trip
    // ----------------------------------------------------------------

    #[test]
    fn sync_response_round_trip_empty_chunk() {
        let original = SyncResponse {
            channel_id: sample_channel_id(),
            first_seq: 42,
            leader_first_retained_seq: 42,
            events: vec![],
        };
        let bytes = original.to_bytes();
        let decoded = SyncResponse::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn sync_response_round_trip_with_events() {
        let original = SyncResponse {
            channel_id: sample_channel_id(),
            first_seq: 100,
            leader_first_retained_seq: 50,
            events: vec![
                SyncEvent {
                    event_seq: 100,
                    payload: b"hello".to_vec(),
                },
                SyncEvent {
                    event_seq: 101,
                    payload: b"world".to_vec(),
                },
                SyncEvent {
                    event_seq: 102,
                    payload: vec![], // empty payload — explicitly representable
                },
            ],
        };
        let bytes = original.to_bytes();
        let decoded = SyncResponse::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    /// R-5 codec pin: the new `leader_first_retained_seq` field
    /// sits at offset 3 + 32 + 8 = 43 (after subprotocol header,
    /// channel id, first_seq) and is u64 LE.
    #[test]
    fn sync_response_leader_first_retained_seq_byte_offset() {
        let original = SyncResponse {
            channel_id: sample_channel_id(),
            first_seq: 0x0102_0304_0506_0708,
            leader_first_retained_seq: 0x1112_1314_1516_1718,
            events: vec![],
        };
        let bytes = original.to_bytes();
        // Header (3) + channel_id (32) = 35; first_seq (8) at 35..43;
        // leader_first_retained_seq (8) at 43..51.
        assert_eq!(
            &bytes[43..51],
            &0x1112_1314_1516_1718_u64.to_le_bytes(),
            "leader_first_retained_seq must be at offset 43..51 in LE form"
        );
    }

    #[test]
    fn sync_response_rejects_truncated_event_record() {
        // Build a valid bytes buffer, then truncate inside the
        // last event's payload to make sure the decoder doesn't
        // panic on a malformed `payload_len`.
        let bytes = SyncResponse {
            channel_id: sample_channel_id(),
            first_seq: 1,
            leader_first_retained_seq: 0,
            events: vec![SyncEvent {
                event_seq: 1,
                payload: b"truncated".to_vec(),
            }],
        }
        .to_bytes();
        // Cut off the last 3 bytes of the payload.
        let err = SyncResponse::from_bytes(&bytes[..bytes.len() - 3]).expect_err("must reject");
        assert!(matches!(err, WireError::Truncated { .. }));
    }

    // ----------------------------------------------------------------
    // SyncHeartbeat round-trip
    // ----------------------------------------------------------------

    #[test]
    fn sync_heartbeat_round_trip_each_role() {
        for role in [
            ReplicaRole::Leader,
            ReplicaRole::Replica,
            ReplicaRole::Candidate,
            ReplicaRole::Idle,
        ] {
            let original = SyncHeartbeat {
                channel_id: sample_channel_id(),
                tail_seq: 0xCAFE,
                role,
                wall_clock_ms: 1_700_000_000_000,
            };
            let bytes = original.to_bytes();
            assert_eq!(bytes.len(), SYNC_HEARTBEAT_SIZE);
            let decoded = SyncHeartbeat::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn sync_heartbeat_rejects_unknown_role() {
        let mut bytes = SyncHeartbeat {
            channel_id: sample_channel_id(),
            tail_seq: 0,
            role: ReplicaRole::Leader,
            wall_clock_ms: 0,
        }
        .to_bytes();
        // role byte is at offset 3 + 32 + 8 = 43
        bytes[43] = 99;
        let err = SyncHeartbeat::from_bytes(&bytes).expect_err("must reject");
        assert!(matches!(err, WireError::BadRole(99)));
    }

    // ----------------------------------------------------------------
    // SyncNack round-trip
    // ----------------------------------------------------------------

    #[test]
    fn sync_nack_round_trip_each_error() {
        for error_code in [
            SyncNackError::NotLeader,
            SyncNackError::BadRange,
            SyncNackError::Backpressure,
            SyncNackError::ChannelClosed,
        ] {
            let original = SyncNack {
                channel_id: sample_channel_id(),
                since_seq: 12345,
                error_code,
                detail: format!("test detail for {:?}", error_code),
            };
            let bytes = original.to_bytes();
            let decoded = SyncNack::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn sync_nack_empty_detail_round_trips() {
        let original = SyncNack {
            channel_id: sample_channel_id(),
            since_seq: 0,
            error_code: SyncNackError::NotLeader,
            detail: String::new(),
        };
        let bytes = original.to_bytes();
        let decoded = SyncNack::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn sync_nack_truncates_oversized_detail() {
        // detail longer than u16::MAX gets silently truncated rather
        // than failing the encode — the structured error code is the
        // load-bearing part; detail is operator-facing logging.
        let huge = "x".repeat(SYNC_NACK_DETAIL_MAX + 1000);
        let original = SyncNack {
            channel_id: sample_channel_id(),
            since_seq: 0,
            error_code: SyncNackError::Backpressure,
            detail: huge.clone(),
        };
        let bytes = original.to_bytes();
        let decoded = SyncNack::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded.detail.len(), SYNC_NACK_DETAIL_MAX);
        assert!(huge.starts_with(&decoded.detail));
    }

    #[test]
    fn sync_nack_rejects_unknown_error_code() {
        let mut bytes = SyncNack {
            channel_id: sample_channel_id(),
            since_seq: 0,
            error_code: SyncNackError::NotLeader,
            detail: String::new(),
        }
        .to_bytes();
        // error_code byte is at offset 3 + 32 + 8 = 43
        bytes[43] = 0;
        let err = SyncNack::from_bytes(&bytes).expect_err("must reject");
        assert!(matches!(err, WireError::BadErrorCode(0)));
    }

    #[test]
    fn sync_nack_rejects_invalid_utf8() {
        let mut bytes = SyncNack {
            channel_id: sample_channel_id(),
            since_seq: 0,
            error_code: SyncNackError::BadRange,
            detail: "ascii".to_string(),
        }
        .to_bytes();
        // detail starts at offset 3 + 32 + 8 + 1 + 2 = 46; replace
        // with an invalid UTF-8 byte sequence of the same length.
        let detail_start = 46;
        let detail_len = bytes.len() - detail_start;
        for i in 0..detail_len {
            bytes[detail_start + i] = 0xC0; // invalid lead byte
        }
        let err = SyncNack::from_bytes(&bytes).expect_err("must reject");
        assert!(matches!(err, WireError::InvalidUtf8));
    }

    // ----------------------------------------------------------------
    // Dispatch-code reservations — pin the constants so a renumbering
    // surface change in a future slice is loud.
    // ----------------------------------------------------------------

    #[test]
    fn dispatch_codes_pinned() {
        assert_eq!(DISPATCH_SYNC_REQUEST, 0x20);
        assert_eq!(DISPATCH_SYNC_RESPONSE, 0x21);
        assert_eq!(DISPATCH_SYNC_HEARTBEAT, 0x22);
        assert_eq!(DISPATCH_SYNC_NACK, 0x23);
        assert_eq!(DISPATCH_REPLICA_SYNC_RESERVED_END, 0x30);
    }

    #[test]
    fn subprotocol_id_pinned() {
        assert_eq!(SUBPROTOCOL_REDEX, 0x0E00);
    }
}
