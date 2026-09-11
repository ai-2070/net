//! The routing envelope codec: `RoutingHeader` and its flags.
//!
//! Extracted from `net::adapter::net::route` in Stage 2 of
//! `BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md` (§7). Only the **codec**
//! travels: `ROUTING_MAGIC`, `ROUTING_HEADER_SIZE`, `RouteFlags`,
//! `RoutingHeader` and its `to_bytes` / `from_bytes` / `write_to` /
//! `write_at` / `read_from` / `is_expired` / `forward`. The route
//! *table* (`RouteEntry`, `RoutingTable`, `next_hop`, the metrics and
//! `SchedulerStreamStats`) stays in the core: it is routing policy,
//! not wire format, and a non-forwarding leaf never consults it.
//!
//! Every routed-session packet is `routing_bytes ++ net_packet`, and
//! Layer 2 is a browser leaf's only pre-direct path, so the envelope
//! is part of the wire surface even though a leaf never forwards.

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Routing header size in bytes.
///
/// Layout: `magic(2) | ttl(1) | hop_count(1) | flags(1) | _reserved(1) | src_id(4) | dest_id(8)`
/// — 18 bytes total. The magic tag at bytes 0-1 unambiguously
/// distinguishes routing headers from direct Net packets (whose
/// own magic is `0x4E45`), so the receive-loop discriminator
/// doesn't depend on `dest_id` happening to not collide with it.
pub const ROUTING_HEADER_SIZE: usize = 18;

/// Magic bytes identifying a routing header: `[0x52, 0x54]` on the
/// wire — ASCII "RT" in read order, for "routing". Stored as a u16
/// little-endian value, that's `0x5452`. Chosen disjoint from the
/// Net packet magic (`0x4E45`) so the receive-loop can discriminate
/// on the first two bytes alone.
pub const ROUTING_MAGIC: u16 = 0x5452;

/// Maximum TTL for multi-hop routing
pub const _MAX_TTL: u8 = 16;

/// Route flags (bitflags — multiple flags can be set simultaneously)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct RouteFlags(u8);

impl RouteFlags {
    /// No special flags
    pub const NONE: Self = Self(0x00);
    /// Control packet (pingwave, capability update)
    pub const CONTROL: Self = Self(0x01);
    /// Requires acknowledgment
    pub const REQUIRES_ACK: Self = Self(0x02);
    /// Priority packet (skip fairness queue)
    pub const PRIORITY: Self = Self(0x04);
    /// Last packet in stream
    pub const END_OF_STREAM: Self = Self(0x08);

    /// Parse flags from u8.
    ///
    /// The `& 0x0F` mask drops the high nibble. Today the defined
    /// flags fit in the low nibble (`CONTROL`, `REQUIRES_ACK`,
    /// `PRIORITY`, `END_OF_STREAM`), so 16 distinct wire bytes
    /// alias to the same `RouteFlags`. **The high nibble is
    /// reserved**: any future flag added there will be silently
    /// stripped by old peers running this codepath. When a new flag
    /// is introduced:
    ///
    /// 1. Allocate it in the **low nibble** if any bit is still
    ///    free, OR
    /// 2. Widen this mask in the same release that defines the new
    ///    flag, in lock-step across every peer that decodes routing
    ///    headers (Rust + cross-language bindings). A skew where
    ///    one peer reads the bit and another masks it off silently
    ///    diverges on routing semantics.
    pub fn from_u8(v: u8) -> Self {
        // Emit a warn when the high nibble is set so a future
        // flag's silent strip doesn't go invisible. The doc-
        // comment above documents the constraint; this log makes
        // the skew observable in production rather than only
        // visible via post-mortem code review.
        if v & 0xF0 != 0 {
            tracing::warn!(
                wire_byte = format_args!("0x{:02x}", v),
                high_nibble = format_args!("0x{:02x}", v & 0xF0),
                "route flags: high-nibble bits set on inbound wire byte and \
                 silently stripped — peer may be running a newer schema. \
                 Widen RouteFlags::from_u8's mask in lock-step before any \
                 production peer relies on a high-nibble bit."
            );
        }
        Self(v & 0x0F)
    }

    /// Convert to u8
    pub fn as_u8(self) -> u8 {
        self.0
    }

    /// Check if a flag is set
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Check if this is a control packet
    pub fn is_control(self) -> bool {
        self.contains(Self::CONTROL)
    }

    /// Check if this is a priority packet
    pub fn is_priority(self) -> bool {
        self.contains(Self::PRIORITY)
    }
}

/// Routing header for multi-hop Net packets.
///
/// Layout (18 bytes):
/// ```text
/// ┌───────────────────────────────────────────────────────────────────┐
/// │ magic (2) │ ttl │ hops │ flags │ rsvd │ src_id (4) │ dest_id (8) │
/// └───────────────────────────────────────────────────────────────────┘
/// ```
///
/// `magic` is always `ROUTING_MAGIC` (ASCII `"RT"` on the wire —
/// `0x5452` as a little-endian `u16`), distinct from the direct-
/// packet magic `0x4E45`. The receive-loop discriminator reads bytes
/// 0-1 alone and dispatches unambiguously — the previous 16-byte
/// layout put `dest_id` at bytes 0-7, and any recipient whose
/// `node_id` had low-16-bits equal to the direct-packet magic
/// (~1 in 65 536) silently mis-classified its own incoming routed
/// packets as Net packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct RoutingHeader {
    /// Final destination node ID (64-bit)
    pub dest_id: u64,
    /// Source node ID (truncated to 32-bit for space)
    pub src_id: u32,
    /// Time-to-live (decremented at each hop)
    pub ttl: u8,
    /// Hop count so far
    pub hop_count: u8,
    /// Route flags
    pub flags: RouteFlags,
    /// Reserved for future use
    pub _reserved: u8,
}

impl RoutingHeader {
    /// Create a new routing header
    pub fn new(dest_id: u64, src_id: u32, ttl: u8) -> Self {
        Self {
            dest_id,
            src_id,
            ttl,
            hop_count: 0,
            flags: RouteFlags::NONE,
            _reserved: 0,
        }
    }

    /// Create a control packet header
    pub fn control(dest_id: u64, src_id: u32, ttl: u8) -> Self {
        Self {
            dest_id,
            src_id,
            ttl,
            hop_count: 0,
            flags: RouteFlags::CONTROL,
            _reserved: 0,
        }
    }

    /// Create a priority packet header
    pub fn priority(dest_id: u64, src_id: u32, ttl: u8) -> Self {
        Self {
            dest_id,
            src_id,
            ttl,
            hop_count: 0,
            flags: RouteFlags::PRIORITY,
            _reserved: 0,
        }
    }

    /// Serialize to bytes.
    ///
    /// The magic tag rides at bytes 0-1 so the receive-loop
    /// discriminator reads it directly — see `ROUTING_MAGIC`.
    pub fn to_bytes(&self) -> [u8; ROUTING_HEADER_SIZE] {
        let mut buf = [0u8; ROUTING_HEADER_SIZE];
        buf[0..2].copy_from_slice(&ROUTING_MAGIC.to_le_bytes());
        buf[2] = self.ttl;
        buf[3] = self.hop_count;
        buf[4] = self.flags.as_u8();
        buf[5] = self._reserved;
        buf[6..10].copy_from_slice(&self.src_id.to_le_bytes());
        buf[10..18].copy_from_slice(&self.dest_id.to_le_bytes());
        buf
    }

    /// Deserialize from bytes. Returns `None` on short input, wrong
    /// magic, or malformed numeric fields.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < ROUTING_HEADER_SIZE {
            return None;
        }
        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != ROUTING_MAGIC {
            return None;
        }
        Some(Self {
            ttl: buf[2],
            hop_count: buf[3],
            flags: RouteFlags::from_u8(buf[4]),
            _reserved: buf[5],
            src_id: u32::from_le_bytes(buf[6..10].try_into().ok()?),
            dest_id: u64::from_le_bytes(buf[10..18].try_into().ok()?),
        })
    }

    /// Write to a buffer
    pub fn write_to(&self, buf: &mut BytesMut) {
        buf.put_u16_le(ROUTING_MAGIC);
        buf.put_u8(self.ttl);
        buf.put_u8(self.hop_count);
        buf.put_u8(self.flags.as_u8());
        buf.put_u8(self._reserved);
        buf.put_u32_le(self.src_id);
        buf.put_u64_le(self.dest_id);
    }

    /// Overwrite an existing 18-byte slice with this header, in place.
    ///
    /// Distinct from [`Self::write_to`] which appends to the tail of a
    /// `BytesMut`: this targets the head of an existing buffer (an
    /// inbound packet's routing-header prefix) so the forwarder can
    /// flip TTL / increment hop_count without allocating a fresh
    /// packet. Used by `Router::route_packet`'s `Bytes::try_into_mut`
    /// fast path — perf #18.
    ///
    /// # Panics
    ///
    /// Panics if `dst.len() < ROUTING_HEADER_SIZE`. The caller is
    /// expected to have already validated the slice length via the
    /// same check that decoded the header.
    pub fn write_at(&self, dst: &mut [u8]) {
        assert!(
            dst.len() >= ROUTING_HEADER_SIZE,
            "write_at: dst is {} bytes, need {}",
            dst.len(),
            ROUTING_HEADER_SIZE,
        );
        dst[0..2].copy_from_slice(&ROUTING_MAGIC.to_le_bytes());
        dst[2] = self.ttl;
        dst[3] = self.hop_count;
        dst[4] = self.flags.as_u8();
        dst[5] = self._reserved;
        dst[6..10].copy_from_slice(&self.src_id.to_le_bytes());
        dst[10..18].copy_from_slice(&self.dest_id.to_le_bytes());
    }

    /// Read from a buffer. Returns `None` on short input or wrong
    /// magic; fields are consumed only on successful parse.
    pub fn read_from(buf: &mut Bytes) -> Option<Self> {
        if buf.remaining() < ROUTING_HEADER_SIZE {
            return None;
        }
        // Peek at magic without advancing so a bad prefix leaves
        // the cursor intact for callers that want to try another
        // decoder.
        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != ROUTING_MAGIC {
            return None;
        }
        let _ = buf.get_u16_le();
        let ttl = buf.get_u8();
        let hop_count = buf.get_u8();
        let flags = RouteFlags::from_u8(buf.get_u8());
        let _reserved = buf.get_u8();
        let src_id = buf.get_u32_le();
        let dest_id = buf.get_u64_le();
        Some(Self {
            dest_id,
            src_id,
            ttl,
            hop_count,
            flags,
            _reserved,
        })
    }

    /// Check if TTL is expired
    #[inline]
    pub fn is_expired(&self) -> bool {
        self.ttl == 0
    }

    /// Decrement TTL and increment hop count (for forwarding)
    ///
    /// `hop_count` is `u8`, so on a 256+-hop path the saturating_add
    /// pins it at 255 and the `hop_count + 2` indirect-route metric
    /// used downstream undercounts the true distance. Routing
    /// correctness is preserved — `ttl` (separate, larger) still
    /// bounds loops — but best-route selection may pick a path with
    /// bogus metrics. Log once at saturation so an operator can
    /// notice and reconfigure path lengths or upgrade `hop_count` to
    /// `u16`. (Changing the wire format is a breaking change held
    /// off until consumers migrate.)
    #[inline]
    pub fn forward(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        if self.hop_count == u8::MAX {
            tracing::warn!(
                "RoutingHeader::forward: hop_count saturated at {}; \
                 indirect-route metrics on this packet are inaccurate",
                u8::MAX
            );
        } else {
            self.hop_count = self.hop_count.saturating_add(1);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_routing_header_roundtrip() {
        let header = RoutingHeader::new(0x123456789ABCDEF0, 0xDEADBEEF, 8);
        let bytes = header.to_bytes();
        let parsed = RoutingHeader::from_bytes(&bytes).unwrap();
        assert_eq!(header, parsed);
    }

    /// Pin perf #18: `write_at` writes the same 18 bytes as
    /// `write_to`, byte-for-byte. The router's
    /// `Bytes::try_into_mut` fast path uses `write_at` to overwrite
    /// the inbound packet's header in place; if the two paths
    /// diverged (e.g. one swapped two fields), forwarded packets
    /// would carry a malformed header — observable only as a
    /// silent receive-side drop on the next hop.
    #[test]
    fn write_at_matches_write_to_byte_for_byte() {
        let header = RoutingHeader::new(0xABCD_EF01_2345_6789, 0xDEAD_BEEF, 7);

        // Path A: write_to into a fresh BytesMut.
        let mut via_write_to = BytesMut::with_capacity(ROUTING_HEADER_SIZE);
        header.write_to(&mut via_write_to);

        // Path B: write_at into an existing 18-byte slice. Pre-fill
        // with a sentinel pattern so an under-write would surface.
        let mut via_write_at = [0xCC; ROUTING_HEADER_SIZE];
        header.write_at(&mut via_write_at);

        assert_eq!(
            &via_write_to[..],
            &via_write_at[..],
            "write_at must produce the same wire bytes as write_to; \
             a divergence would silently malform every forwarded packet",
        );
    }

    /// Pin: `write_at` panics rather than silently truncates when
    /// the destination slice is too short. A regression that turned
    /// the assert into a saturating-write would let the router
    /// emit an underwritten header into the forward path.
    #[test]
    #[should_panic(expected = "write_at")]
    fn write_at_panics_on_short_slice() {
        let header = RoutingHeader::new(1, 2, 1);
        let mut short = [0u8; ROUTING_HEADER_SIZE - 1];
        header.write_at(&mut short);
    }

    #[test]
    fn test_routing_header_magic_at_offset_zero() {
        // ROUTING_MAGIC must appear at bytes 0-1 regardless of
        // dest_id / src_id values. The receive-loop discriminator
        // peeks at bytes 0-1 and relies on this.
        let header = RoutingHeader::new(0x4E45_4E45_4E45_4E45, 0x4E45_4E45, 8);
        let bytes = header.to_bytes();
        assert_eq!(
            u16::from_le_bytes([bytes[0], bytes[1]]),
            ROUTING_MAGIC,
            "magic must live at bytes 0-1 independent of dest_id's own byte pattern",
        );
    }

    #[test]
    fn test_routing_header_rejects_wrong_magic() {
        // from_bytes must refuse buffers whose bytes 0-1 aren't
        // ROUTING_MAGIC — this is what lets the receive-loop
        // discriminator short-circuit cleanly without parsing the
        // rest of the header.
        let mut bytes = RoutingHeader::new(0x1234, 0x5678, 4).to_bytes();
        // Overwrite magic with direct-packet MAGIC.
        bytes[0..2].copy_from_slice(&0x4E45_u16.to_le_bytes());
        assert!(RoutingHeader::from_bytes(&bytes).is_none());

        // Overwrite with arbitrary garbage.
        bytes[0..2].copy_from_slice(&0xFFFF_u16.to_le_bytes());
        assert!(RoutingHeader::from_bytes(&bytes).is_none());
    }

    #[test]
    fn test_regression_routing_discriminator_survives_magic_collision_node_id() {
        // Regression (LOW, BUGS.md): the old 16-byte layout put
        // `dest_id` at bytes 0-7. When a recipient's own node_id
        // had low-16-bits equal to 0x4E45 (the direct Net-packet
        // magic), routed packets to that node were
        // mis-discriminated as direct packets and silently dropped
        // at the AEAD layer — 1-in-65 536 node_ids affected.
        //
        // The new layout puts ROUTING_MAGIC at bytes 0-1 and
        // shifts dest_id to bytes 10-17, so the discriminator is
        // unambiguous for every possible dest_id value.
        //
        // This test constructs a header whose dest_id has low-16
        // bits equal to the old ambiguous value and verifies that
        // the header still serializes with ROUTING_MAGIC at the
        // front and round-trips correctly.
        let ambiguous_dest: u64 = 0xDEAD_BEEF_FFFF_4E45;
        let header = RoutingHeader::new(ambiguous_dest, 0x1111_2222, 8);
        let bytes = header.to_bytes();
        assert_eq!(
            u16::from_le_bytes([bytes[0], bytes[1]]),
            ROUTING_MAGIC,
            "magic at offset 0 must be independent of dest_id",
        );
        let parsed = RoutingHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.dest_id, ambiguous_dest);
        assert_eq!(parsed.src_id, 0x1111_2222);
        assert_eq!(parsed.ttl, 8);
    }

    #[test]
    fn test_routing_header_forward() {
        let mut header = RoutingHeader::new(0x1234, 0x5678, 3);
        assert_eq!(header.ttl, 3);
        assert_eq!(header.hop_count, 0);

        assert!(header.forward());
        assert_eq!(header.ttl, 2);
        assert_eq!(header.hop_count, 1);

        assert!(header.forward());
        assert!(header.forward());
        assert_eq!(header.ttl, 0);
        assert_eq!(header.hop_count, 3);

        // Can't forward with TTL=0
        assert!(!header.forward());
    }

    #[test]
    fn test_routing_header_flags() {
        let control = RoutingHeader::control(0x1234, 0x5678, 2);
        assert!(control.flags.is_control());

        let priority = RoutingHeader::priority(0x1234, 0x5678, 2);
        assert!(priority.flags.is_priority());
    }

    #[test]
    fn test_route_flags_combined() {
        // Regression: from_u8 used to match only single-flag values.
        // Combined flags (e.g., Control | RequiresAck) mapped to None.
        let combined = RouteFlags::CONTROL.as_u8() | RouteFlags::REQUIRES_ACK.as_u8();
        let parsed = RouteFlags::from_u8(combined);
        assert!(
            parsed.is_control(),
            "Control bit must survive combined parse"
        );
        assert!(
            parsed.contains(RouteFlags::REQUIRES_ACK),
            "RequiresAck bit must survive combined parse"
        );

        let all = RouteFlags::CONTROL.as_u8()
            | RouteFlags::REQUIRES_ACK.as_u8()
            | RouteFlags::PRIORITY.as_u8()
            | RouteFlags::END_OF_STREAM.as_u8();
        let parsed_all = RouteFlags::from_u8(all);
        assert!(parsed_all.is_control());
        assert!(parsed_all.is_priority());
        assert!(parsed_all.contains(RouteFlags::REQUIRES_ACK));
        assert!(parsed_all.contains(RouteFlags::END_OF_STREAM));
    }

    #[test]
    fn test_route_flags_roundtrip() {
        // Verify combined flags survive to_bytes/from_bytes roundtrip
        let mut header = RoutingHeader::new(0x1234, 0x5678, 4);
        header.flags =
            RouteFlags::from_u8(RouteFlags::PRIORITY.as_u8() | RouteFlags::REQUIRES_ACK.as_u8());

        let bytes = header.to_bytes();
        let parsed = RoutingHeader::from_bytes(&bytes).unwrap();
        assert!(parsed.flags.is_priority());
        assert!(parsed.flags.contains(RouteFlags::REQUIRES_ACK));
    }
}
