//! Net wire protocol definitions.
//!
//! This module defines the packet format for the Net L0 Transport Protocol (Net).
//! All packets use a fixed 68-byte header (8-byte aligned for natural u64 reads).

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Magic bytes: "NE" (0x4E45)
pub const MAGIC: u16 = 0x4E45;

/// Current protocol version
pub const VERSION: u8 = 1;

/// Header size in bytes. Widened to 68 in the
/// `WIRE_ORIGIN_HASH_64BIT` cutover when `origin_hash` grew from
/// u32 to u64 — see `docs/plans/WIRE_ORIGIN_HASH_64BIT.md`. The
/// struct is `align(8)` (natural u64 alignment); the prior
/// 64-byte cache-line alignment was reclaimed when the size left
/// the cache-line boundary.
pub const HEADER_SIZE: usize = 68;

/// Poly1305 authentication tag size
pub const TAG_SIZE: usize = 16;

/// ChaCha20-Poly1305 nonce size (counter-based)
pub const NONCE_SIZE: usize = 12;

/// Maximum packet size (fits in jumbo frame with headroom)
pub const MAX_PACKET_SIZE: usize = 8192;

/// Maximum payload size (packet - header - tag)
pub const MAX_PAYLOAD_SIZE: usize = MAX_PACKET_SIZE - HEADER_SIZE - TAG_SIZE;

/// Packet flags for protocol control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct PacketFlags(u8);

impl PacketFlags {
    /// No flags set
    pub const NONE: Self = Self(0);
    /// Packet requires acknowledgment
    pub const RELIABLE: Self = Self(0b0000_0001);
    /// This is a NACK/retransmit request
    pub const NACK: Self = Self(0b0000_0010);
    /// High priority (bypass normal queuing)
    pub const PRIORITY: Self = Self(0b0000_0100);
    /// Final packet in batch
    pub const FIN: Self = Self(0b0000_1000);
    /// Handshake packet
    pub const HANDSHAKE: Self = Self(0b0001_0000);
    /// Heartbeat/keepalive
    pub const HEARTBEAT: Self = Self(0b0010_0000);

    /// Create flags from raw bits
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Get raw bits
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Check if a flag is set
    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Set a flag
    #[inline]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Clear a flag
    #[inline]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Check if this is a handshake packet
    #[inline]
    pub const fn is_handshake(self) -> bool {
        self.contains(Self::HANDSHAKE)
    }

    /// Check if this is a heartbeat packet
    #[inline]
    pub const fn is_heartbeat(self) -> bool {
        self.contains(Self::HEARTBEAT)
    }

    /// Check if reliability is requested
    #[inline]
    pub const fn is_reliable(self) -> bool {
        self.contains(Self::RELIABLE)
    }

    /// Check if this is a NACK packet
    #[inline]
    pub const fn is_nack(self) -> bool {
        self.contains(Self::NACK)
    }
}

/// Net packet header - 68 bytes, 8-byte aligned.
///
/// Wire format:
/// ```text
///  0                   1                   2                   3
///  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |         MAGIC (0x4E45)        |     VER       |     FLAGS     |  4
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |   PRIORITY    |    HOP_TTL    |   HOP_COUNT   |  FRAG_FLAGS   |  8
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |       SUBPROTOCOL_ID          |        CHANNEL_HASH           | 12
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                                                               |
/// +                         NONCE (12 bytes)                      + 24
/// |                                                               |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                       SESSION_ID (8 bytes)                    | 32
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                       STREAM_ID (8 bytes)                     | 40
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                       SEQUENCE (8 bytes)                      | 48
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                                                               |
/// +                      ORIGIN_HASH (8 bytes)                    + 56
/// |                                                               |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                       SUBNET_ID (4 bytes)                     | 60
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |       FRAGMENT_ID             |        FRAGMENT_OFFSET        | 64
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |       PAYLOAD_LEN             |        EVENT_COUNT            | 68
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
///
/// `ORIGIN_HASH` precedes `SUBNET_ID` so the u64 sits at offset
/// 48 (naturally 8-aligned) — putting `SUBNET_ID` first would
/// force a 4-byte padding gap and grow the struct to 72 bytes.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(8))]
pub struct NetHeader {
    // — routing fast-path (0-11) —
    /// Magic: "NE" (0x4E45)
    pub magic: u16,
    /// Protocol version (1)
    pub version: u8,
    /// Flags (reliability, priority, etc.)
    pub flags: PacketFlags,
    /// Priority level (0 = lowest, 255 = highest)
    pub priority: u8,
    /// Maximum hops before packet is dropped
    pub hop_ttl: u8,
    /// Current hop count (incremented by forwarding nodes)
    pub hop_count: u8,
    /// Fragmentation flags
    pub frag_flags: u8,
    /// Subprotocol identifier for capability-aware routing
    pub subprotocol_id: u16,
    /// Truncated channel name hash for wire-speed filtering
    pub channel_hash: u16,

    // — crypto (12-23) —
    /// ChaCha20-Poly1305 nonce (12 bytes) - counter-based
    pub nonce: [u8; NONCE_SIZE],

    // — session (24-47) —
    /// Session identifier (from handshake)
    pub session_id: u64,
    /// Stream identifier (for multiplexing)
    pub stream_id: u64,
    /// Per-stream sequence number (monotonic)
    pub sequence: u64,

    // — mesh topology (48-63) —
    /// Full 64-bit blake2 hash of the origin node identity, matching
    /// `EntityKeypair::origin_hash()`. Widened from u32 in the
    /// `WIRE_ORIGIN_HASH_64BIT` cutover so the reverse index
    /// (`mesh.rs::origin_hash_to_node`) maps `origin_hash → NodeId`
    /// unambiguously even under adversarial collision-grinding.
    /// Declared before `subnet_id` so the u64 sits at a naturally
    /// 8-aligned offset.
    pub origin_hash: u64,
    /// Subnet identifier for gateway routing
    pub subnet_id: u32,
    /// Fragment group identifier
    pub fragment_id: u16,
    /// Byte offset within original packet
    pub fragment_offset: u16,

    // — payload (64-67) —
    /// Payload length (after encryption, before tag)
    pub payload_len: u16,
    /// Number of events in payload
    pub event_count: u16,
}

// Verify the in-memory size hasn't regressed past 72 bytes — the
// natural u64-aligned size with the current field set. `HEADER_SIZE`
// is the WIRE size (68 bytes, what to_bytes / from_bytes serialize);
// `align(8)` rounds the in-memory `size_of` up to a multiple of 8,
// so the assertion targets 72 directly rather than tying it to
// `HEADER_SIZE`.
const _: () = assert!(std::mem::size_of::<NetHeader>() == 72);

impl NetHeader {
    /// Create a new header with default values.
    ///
    /// New routing/mesh fields default to zero. Use the `with_*` methods
    /// to set them when needed.
    #[inline]
    pub fn new(
        session_id: u64,
        stream_id: u64,
        sequence: u64,
        nonce: [u8; NONCE_SIZE],
        payload_len: u16,
        event_count: u16,
        flags: PacketFlags,
    ) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            flags,
            priority: 0,
            hop_ttl: 0,
            hop_count: 0,
            frag_flags: 0,
            subprotocol_id: 0,
            channel_hash: 0,
            nonce,
            session_id,
            stream_id,
            sequence,
            subnet_id: 0,
            origin_hash: 0,
            fragment_id: 0,
            fragment_offset: 0,
            payload_len,
            event_count,
        }
    }

    /// Create a handshake header
    #[inline]
    pub fn handshake(payload_len: u16) -> Self {
        Self::new(
            0,
            0,
            0,
            [0u8; NONCE_SIZE],
            payload_len,
            0,
            PacketFlags::HANDSHAKE,
        )
    }

    /// Create a heartbeat header
    #[inline]
    pub fn heartbeat(session_id: u64) -> Self {
        Self::new(
            session_id,
            0,
            0,
            [0u8; NONCE_SIZE],
            0,
            0,
            PacketFlags::HEARTBEAT,
        )
    }

    /// Set priority level
    #[inline]
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set hop TTL and initial hop count
    #[inline]
    pub fn with_hops(mut self, ttl: u8) -> Self {
        self.hop_ttl = ttl;
        self.hop_count = 0;
        self
    }

    /// Set subprotocol identifier
    #[inline]
    pub fn with_subprotocol(mut self, id: u16) -> Self {
        self.subprotocol_id = id;
        self
    }

    /// Set channel hash
    #[inline]
    pub fn with_channel_hash(mut self, hash: u16) -> Self {
        self.channel_hash = hash;
        self
    }

    /// Set subnet identifier
    #[inline]
    pub fn with_subnet(mut self, subnet_id: u32) -> Self {
        self.subnet_id = subnet_id;
        self
    }

    /// Set origin node hash. Carries the full u64 from
    /// `EntityKeypair::origin_hash()` — the per-packet wire field
    /// matches the application-layer width post-`WIRE_ORIGIN_HASH_64BIT`.
    #[inline]
    pub fn with_origin(mut self, origin_hash: u64) -> Self {
        self.origin_hash = origin_hash;
        self
    }

    /// Set fragmentation fields
    #[inline]
    pub fn with_fragment(mut self, id: u16, offset: u16, flags: u8) -> Self {
        self.fragment_id = id;
        self.fragment_offset = offset;
        self.frag_flags = flags;
        self
    }

    /// Get AAD (Additional Authenticated Data) for AEAD construction.
    ///
    /// Authenticates all header fields except:
    /// - nonce (used as the AEAD IV)
    /// - hop_count (mutable in transit — incremented by forwarding nodes)
    ///
    /// This binds the encrypted payload to the immutable header fields, preventing
    /// an attacker from modifying any field without breaking AEAD verification.
    ///
    /// 56 bytes after the `WIRE_ORIGIN_HASH_64BIT` cutover (was 52
    /// when `origin_hash` was u32). The trailing fragment / payload
    /// slots all shifted 4 bytes later in lockstep.
    #[inline]
    pub fn aad(&self) -> [u8; 56] {
        let mut aad = [0u8; 56];
        // routing fast-path (hop_count excluded: mutable in transit)
        aad[0..2].copy_from_slice(&self.magic.to_le_bytes());
        aad[2] = self.version;
        aad[3] = self.flags.bits();
        aad[4] = self.priority;
        aad[5] = self.hop_ttl;
        // aad[6] = 0: hop_count excluded from AAD
        aad[7] = self.frag_flags;
        aad[8..10].copy_from_slice(&self.subprotocol_id.to_le_bytes());
        aad[10..12].copy_from_slice(&self.channel_hash.to_le_bytes());
        // session
        aad[12..20].copy_from_slice(&self.session_id.to_le_bytes());
        aad[20..28].copy_from_slice(&self.stream_id.to_le_bytes());
        aad[28..36].copy_from_slice(&self.sequence.to_le_bytes());
        // mesh topology — origin_hash before subnet_id to mirror
        // the in-memory layout (origin_hash sits at 8-aligned
        // offset 48; subnet_id follows at 56).
        aad[36..44].copy_from_slice(&self.origin_hash.to_le_bytes());
        aad[44..48].copy_from_slice(&self.subnet_id.to_le_bytes());
        aad[48..50].copy_from_slice(&self.fragment_id.to_le_bytes());
        aad[50..52].copy_from_slice(&self.fragment_offset.to_le_bytes());
        // payload metadata
        aad[52..54].copy_from_slice(&self.payload_len.to_le_bytes());
        aad[54..56].copy_from_slice(&self.event_count.to_le_bytes());
        aad
    }

    /// Serialize header to bytes
    #[inline]
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        let mut cursor = &mut buf[..];

        // routing fast-path
        cursor.put_u16_le(self.magic);
        cursor.put_u8(self.version);
        cursor.put_u8(self.flags.bits());
        cursor.put_u8(self.priority);
        cursor.put_u8(self.hop_ttl);
        cursor.put_u8(self.hop_count);
        cursor.put_u8(self.frag_flags);
        cursor.put_u16_le(self.subprotocol_id);
        cursor.put_u16_le(self.channel_hash);
        // crypto
        cursor.put_slice(&self.nonce);
        // session
        cursor.put_u64_le(self.session_id);
        cursor.put_u64_le(self.stream_id);
        cursor.put_u64_le(self.sequence);
        // mesh topology — origin_hash before subnet_id to match
        // the in-memory layout post-`WIRE_ORIGIN_HASH_64BIT`.
        cursor.put_u64_le(self.origin_hash);
        cursor.put_u32_le(self.subnet_id);
        cursor.put_u16_le(self.fragment_id);
        cursor.put_u16_le(self.fragment_offset);
        // payload
        cursor.put_u16_le(self.payload_len);
        cursor.put_u16_le(self.event_count);

        buf
    }

    /// Parse header from bytes
    #[inline]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }

        let mut cursor = &data[..HEADER_SIZE];

        let magic = cursor.get_u16_le();
        if magic != MAGIC {
            return None;
        }

        let version = cursor.get_u8();
        let flags = PacketFlags::from_bits(cursor.get_u8());
        let priority = cursor.get_u8();
        let hop_ttl = cursor.get_u8();
        let hop_count = cursor.get_u8();
        let frag_flags = cursor.get_u8();
        let subprotocol_id = cursor.get_u16_le();
        let channel_hash = cursor.get_u16_le();

        let mut nonce = [0u8; NONCE_SIZE];
        cursor.copy_to_slice(&mut nonce);

        let session_id = cursor.get_u64_le();
        let stream_id = cursor.get_u64_le();
        let sequence = cursor.get_u64_le();

        let origin_hash = cursor.get_u64_le();
        let subnet_id = cursor.get_u32_le();
        let fragment_id = cursor.get_u16_le();
        let fragment_offset = cursor.get_u16_le();

        let payload_len = cursor.get_u16_le();
        let event_count = cursor.get_u16_le();

        Some(Self {
            magic,
            version,
            flags,
            priority,
            hop_ttl,
            hop_count,
            frag_flags,
            subprotocol_id,
            channel_hash,
            nonce,
            session_id,
            stream_id,
            sequence,
            subnet_id,
            origin_hash,
            fragment_id,
            fragment_offset,
            payload_len,
            event_count,
        })
    }

    /// Maximum events per packet. Each event needs at least a 4-byte length
    /// prefix, so this is bounded by MAX_PAYLOAD_SIZE / LEN_SIZE.
    pub const MAX_EVENTS_PER_PACKET: u16 = (MAX_PAYLOAD_SIZE / EventFrame::LEN_SIZE) as u16;

    /// Validate the header
    #[inline]
    pub fn validate(&self) -> bool {
        self.magic == MAGIC
            && self.version == VERSION
            && (self.payload_len as usize) <= MAX_PAYLOAD_SIZE
            && self.event_count <= Self::MAX_EVENTS_PER_PACKET
    }
}

/// Event frame format for packing multiple events in a single packet.
///
/// Format: `[len: u32][data: [u8; len]]...`
///
/// Events are concatenated with 4-byte length prefixes. No additional framing.
pub struct EventFrame;

impl EventFrame {
    /// Size of the length prefix
    pub const LEN_SIZE: usize = 4;

    /// Write events to a buffer, returning the total bytes written.
    ///
    /// Panics if any event exceeds `u32::MAX` bytes. Under normal
    /// production paths every event is well below `MAX_PAYLOAD_SIZE`
    /// (~8 KiB), but without the assertion an event larger than 4 GiB
    /// would silently truncate the length prefix and corrupt the
    /// framed stream — a panic is far preferable to silent data
    /// corruption on a framing boundary.
    #[inline]
    #[expect(
        clippy::expect_used,
        reason = "events larger than u32::MAX (~4 GiB) are an invariant violation upstream — a panic on encode is better than a silent length-prefix truncation that would corrupt the framed stream"
    )]
    pub fn write_events(events: &[Bytes], buf: &mut BytesMut) -> usize {
        let start = buf.len();
        for event in events {
            let len = u32::try_from(event.len())
                .expect("event length exceeds u32::MAX — cannot encode in 4-byte length prefix");
            buf.put_u32_le(len);
            buf.put_slice(event);
        }
        buf.len() - start
    }

    /// Read events from a buffer
    #[inline]
    pub fn read_events(mut data: Bytes, count: u16) -> Vec<Bytes> {
        // Cap pre-allocation to what the data can actually hold
        let max_events = data.remaining() / Self::LEN_SIZE;
        let mut events = Vec::with_capacity((count as usize).min(max_events));

        for _ in 0..count {
            if data.remaining() < Self::LEN_SIZE {
                break;
            }

            let len = data.get_u32_le() as usize;
            if data.remaining() < len {
                break;
            }

            events.push(data.split_to(len));
        }

        events
    }

    /// Calculate total size for events
    #[inline]
    pub fn calculate_size(events: &[Bytes]) -> usize {
        events.iter().map(|e| Self::LEN_SIZE + e.len()).sum()
    }
}

/// NACK payload for reliable streams.
///
/// A NACK is sent by the receiver when there is at least one gap in the
/// received-sequence range. It tells the sender:
///
/// - `next_expected`: the next sequence the receiver is waiting for.
///   By construction, this sequence is missing (otherwise the receiver
///   would have advanced past it).
/// - `missing_bitmap`: bit `i` set iff sequence `next_expected + 1 + i`
///   is missing (up to 64 future sequences).
pub struct NackPayload {
    /// Next sequence the receiver expects. All seqs `< next_expected`
    /// have been received contiguously.
    pub next_expected: u64,
    /// Bitmap of missing sequences after `next_expected`.
    pub missing_bitmap: u64,
}

impl std::fmt::Debug for NackPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NackPayload")
            .field("next_expected", &self.next_expected)
            .field(
                "missing_bitmap",
                &format_args!("{:#b}", self.missing_bitmap),
            )
            .finish()
    }
}

impl Clone for NackPayload {
    fn clone(&self) -> Self {
        *self
    }
}

impl Copy for NackPayload {}

impl NackPayload {
    /// Size of NACK payload
    pub const SIZE: usize = 16;

    /// Serialize to bytes
    #[inline]
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.next_expected.to_le_bytes());
        buf[8..16].copy_from_slice(&self.missing_bitmap.to_le_bytes());
        buf
    }

    /// Parse from bytes. Rejects buffers whose length is anything other
    /// than exactly [`Self::SIZE`] so trailing garbage isn't silently
    /// accepted.
    #[inline]
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != Self::SIZE {
            return None;
        }

        let next_expected = u64::from_le_bytes(data[0..8].try_into().ok()?);
        let missing_bitmap = u64::from_le_bytes(data[8..16].try_into().ok()?);

        Some(Self {
            next_expected,
            missing_bitmap,
        })
    }

    /// Get missing sequence numbers.
    ///
    /// Emits `next_expected` (always missing by construction when a NACK
    /// is sent), followed by every future seq whose bit is set in the
    /// missing bitmap.
    #[inline]
    pub fn missing_sequences(&self) -> impl Iterator<Item = u64> + '_ {
        let base = self.next_expected;
        std::iter::once(base).chain((0..64).filter_map(move |i| {
            if (self.missing_bitmap >> i) & 1 != 0 {
                // `base + 1 + i` cannot overflow in any realistic
                // stream (2^64 packets is far beyond any deployment).
                Some(base.saturating_add(1).saturating_add(i))
            } else {
                None
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_size() {
        // Wire size — what `to_bytes` serializes and `from_bytes`
        // expects on the input slice.
        assert_eq!(HEADER_SIZE, 68);
        // In-memory size — `align(8)` rounds the natural 68-byte
        // field layout up to a multiple of 8.
        assert_eq!(std::mem::size_of::<NetHeader>(), 72);
    }

    #[test]
    fn test_header_roundtrip() {
        let nonce = [0x42u8; NONCE_SIZE];
        let header = NetHeader::new(
            0x1234567890ABCDEF,
            0xFEDCBA0987654321,
            42,
            nonce,
            1024,
            10,
            PacketFlags::RELIABLE,
        )
        .with_priority(7)
        .with_hops(32)
        .with_subprotocol(0x0100)
        .with_channel_hash(0xABCD)
        .with_subnet(0x12345678)
        .with_origin(0xDEADBEEF)
        .with_fragment(1, 512, 0x01);

        let bytes = header.to_bytes();
        let parsed = NetHeader::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.magic, MAGIC);
        assert_eq!(parsed.version, VERSION);
        assert_eq!(parsed.flags, PacketFlags::RELIABLE);
        assert_eq!(parsed.priority, 7);
        assert_eq!(parsed.hop_ttl, 32);
        assert_eq!(parsed.hop_count, 0);
        assert_eq!(parsed.frag_flags, 0x01);
        assert_eq!(parsed.subprotocol_id, 0x0100);
        assert_eq!(parsed.channel_hash, 0xABCD);
        assert_eq!(parsed.nonce, nonce);
        assert_eq!(parsed.session_id, 0x1234567890ABCDEF);
        assert_eq!(parsed.stream_id, 0xFEDCBA0987654321);
        assert_eq!(parsed.sequence, 42);
        assert_eq!(parsed.subnet_id, 0x12345678);
        assert_eq!(parsed.origin_hash, 0xDEADBEEF);
        assert_eq!(parsed.fragment_id, 1);
        assert_eq!(parsed.fragment_offset, 512);
        assert_eq!(parsed.payload_len, 1024);
        assert_eq!(parsed.event_count, 10);
    }

    #[test]
    fn origin_hash_preserves_high_bits_across_wire_roundtrip() {
        // Regression for the `WIRE_ORIGIN_HASH_64BIT` cutover.
        // The pre-cutover wire field was u32, so the high 32 bits
        // of `EntityId::origin_hash()` were silently truncated on
        // every packet. Pin that a hash with bits set above 2^32
        // survives serialize → deserialize intact.
        const HIGH_BITS_HASH: u64 = 0xCAFEBABE_DEADBEEF;
        debug_assert!(
            HIGH_BITS_HASH > u32::MAX as u64,
            "test hash must exercise the high 32 bits"
        );

        let header = NetHeader::new(
            0xAAAA_BBBB_CCCC_DDDD,
            0,
            1,
            [0u8; NONCE_SIZE],
            64,
            1,
            PacketFlags::NONE,
        )
        .with_origin(HIGH_BITS_HASH);
        assert_eq!(header.origin_hash, HIGH_BITS_HASH);

        let bytes = header.to_bytes();
        let parsed = NetHeader::from_bytes(&bytes).expect("parse");
        assert_eq!(parsed.origin_hash, HIGH_BITS_HASH);

        // The AAD covers origin_hash (offsets 36..44 post-cutover); a
        // single bit flipped anywhere in the 8-byte slice must change
        // the AAD output — proves the field is fully authenticated,
        // not just the low 32 bits.
        let aad_full = header.aad();
        let mut tampered = header;
        tampered.origin_hash ^= 1u64 << 33; // bit only present in the high half
        let aad_tampered = tampered.aad();
        assert_ne!(
            aad_full, aad_tampered,
            "AAD must authenticate all 64 bits of origin_hash"
        );
    }

    #[test]
    fn header_validation_field_isolation() {
        // Adversarial pair: two `origin_hash` values whose low 32
        // bits collide (`u32::MAX & both = 0xDEADBEEF`) but whose
        // full u64 values differ. Pre-cutover the wire would have
        // collapsed both to the same routing key; post-cutover the
        // distinct high bits survive and the receiver sees the
        // correct full value.
        const LOW_COMMON: u32 = 0xDEAD_BEEF;
        let a: u64 = LOW_COMMON as u64;
        let b: u64 = (0x4242_4242u64 << 32) | (LOW_COMMON as u64);
        assert_eq!(a as u32, b as u32);
        assert_ne!(a, b);

        let mk = |h: u64| -> NetHeader {
            NetHeader::new(1, 0, 1, [0u8; NONCE_SIZE], 0, 0, PacketFlags::NONE).with_origin(h)
        };
        let ha = NetHeader::from_bytes(&mk(a).to_bytes()).unwrap();
        let hb = NetHeader::from_bytes(&mk(b).to_bytes()).unwrap();
        assert_ne!(
            ha.origin_hash, hb.origin_hash,
            "low-32-bit collision must not collapse on the wire"
        );
    }

    #[test]
    fn test_header_validation() {
        let header = NetHeader::new(0, 0, 0, [0u8; NONCE_SIZE], 1024, 0, PacketFlags::NONE);
        assert!(header.validate());

        // Invalid magic
        let mut bytes = header.to_bytes();
        bytes[0] = 0xFF;
        let invalid = NetHeader::from_bytes(&bytes);
        assert!(invalid.is_none());
    }

    #[test]
    fn test_packet_flags() {
        let flags = PacketFlags::NONE
            .with(PacketFlags::RELIABLE)
            .with(PacketFlags::PRIORITY);

        assert!(flags.is_reliable());
        assert!(flags.contains(PacketFlags::PRIORITY));
        assert!(!flags.is_handshake());

        let cleared = flags.without(PacketFlags::RELIABLE);
        assert!(!cleared.is_reliable());
        assert!(cleared.contains(PacketFlags::PRIORITY));
    }

    #[test]
    fn test_aad() {
        let header = NetHeader::new(
            0x1234567890ABCDEF,
            0xFEDCBA0987654321,
            42,
            [0u8; NONCE_SIZE],
            1024,
            10,
            PacketFlags::RELIABLE,
        )
        .with_priority(5)
        .with_subnet(0x42);

        let aad = header.aad();
        // AAD widened from 52 → 56 in `WIRE_ORIGIN_HASH_64BIT` when
        // origin_hash grew u32 → u64.
        assert_eq!(aad.len(), 56);

        // Verify magic
        assert_eq!(u16::from_le_bytes([aad[0], aad[1]]), MAGIC);
        // Verify version
        assert_eq!(aad[2], VERSION);
        // Verify flags
        assert_eq!(aad[3], PacketFlags::RELIABLE.bits());
        // Verify priority
        assert_eq!(aad[4], 5);
    }

    #[test]
    fn test_event_frame_roundtrip() {
        let events = vec![
            Bytes::from_static(b"event1"),
            Bytes::from_static(b"event2"),
            Bytes::from_static(b"event3"),
        ];

        let mut buf = BytesMut::with_capacity(256);
        let size = EventFrame::write_events(&events, &mut buf);

        assert_eq!(size, 3 * 4 + 6 + 6 + 6); // 3 length prefixes + event data

        let parsed = EventFrame::read_events(buf.freeze(), 3);
        assert_eq!(parsed.len(), 3);
        assert_eq!(&parsed[0][..], b"event1");
        assert_eq!(&parsed[1][..], b"event2");
        assert_eq!(&parsed[2][..], b"event3");
    }

    /// Regression: `write_events` used to write the length prefix as
    /// `event.len() as u32`, which silently truncates for events
    /// larger than `u32::MAX`. On 64-bit platforms this could corrupt
    /// the framed stream. The write site now panics rather than
    /// writing a truncated length, so a bug anywhere upstream that
    /// bypasses the payload-size cap surfaces loudly instead of as
    /// silent data corruption.
    ///
    /// We cannot actually allocate a 4GiB event in a unit test, but
    /// we can verify the guard path compiles and is wired up by
    /// asserting the normal path still succeeds — the oversize branch
    /// is exercised by the `expect` in `write_events`.
    #[test]
    fn test_event_frame_length_prefix_fits_u32() {
        // A 64KB event is well under u32::MAX and must write fine.
        let big = Bytes::from(vec![0xABu8; 64 * 1024]);
        let events = vec![big.clone()];
        let mut buf = BytesMut::with_capacity(64 * 1024 + 8);
        let size = EventFrame::write_events(&events, &mut buf);
        assert_eq!(size, EventFrame::LEN_SIZE + big.len());
        // Verify the length prefix actually encodes the full length —
        // catches any future accidental re-truncation.
        let prefix = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        assert_eq!(prefix as usize, big.len());
    }

    #[test]
    fn test_nack_payload_roundtrip() {
        // With `next_expected = 100` and bits 0,2,5,7 set, the missing
        // sequences are: 100 (always, implicit) plus 101, 103, 106, 108.
        let nack = NackPayload {
            next_expected: 100,
            missing_bitmap: 0b1010_0101,
        };

        let bytes = nack.to_bytes();
        let parsed = NackPayload::from_bytes(&bytes).unwrap();

        assert_eq!(parsed.next_expected, 100);
        assert_eq!(parsed.missing_bitmap, 0b1010_0101);

        let missing: Vec<_> = parsed.missing_sequences().collect();
        assert_eq!(missing, vec![100, 101, 103, 106, 108]);
    }

    #[test]
    fn test_nack_payload_rejects_trailing_bytes() {
        // Regression: from_bytes used `< SIZE` so trailing garbage was
        // silently accepted. Now we require exactly SIZE bytes.
        let nack = NackPayload {
            next_expected: 1,
            missing_bitmap: 0b10,
        };
        let mut bytes = nack.to_bytes().to_vec();
        bytes.push(0xFF); // one byte of trailing garbage

        assert!(
            NackPayload::from_bytes(&bytes).is_none(),
            "NackPayload::from_bytes must reject buffers longer than SIZE"
        );
    }

    #[test]
    fn test_validate_rejects_excessive_event_count() {
        let header = NetHeader::new(0, 0, 0, [0u8; NONCE_SIZE], 100, 10, PacketFlags::NONE);
        assert!(header.validate());

        // event_count exceeding MAX_EVENTS_PER_PACKET must be rejected
        let header = NetHeader::new(
            0,
            0,
            0,
            [0u8; NONCE_SIZE],
            100,
            NetHeader::MAX_EVENTS_PER_PACKET + 1,
            PacketFlags::NONE,
        );
        assert!(!header.validate());
    }

    #[test]
    fn test_read_events_caps_allocation() {
        // Attacker-controlled count=65535 with tiny payload should not
        // allocate 65535 slots
        let data = Bytes::from_static(b"");
        let events = EventFrame::read_events(data, u16::MAX);
        assert!(events.is_empty());
        assert!(events.capacity() <= 1);
    }

    // ---- Regression tests for Cubic AI findings ----

    #[test]
    fn test_regression_hop_count_excluded_from_aad() {
        // Regression: hop_count was included in AAD, but forwarding nodes
        // increment it in transit. Including it would break AEAD verification
        // on multi-hop paths.
        let header1 = NetHeader::new(
            0x1234,
            0x5678,
            42,
            [0u8; NONCE_SIZE],
            100,
            5,
            PacketFlags::NONE,
        );
        let mut header2 = header1;
        header2.hop_count = 99; // forwarding node incremented hop_count

        assert_eq!(
            header1.aad(),
            header2.aad(),
            "AAD must be identical regardless of hop_count (mutable in transit)"
        );
    }
}
