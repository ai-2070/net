//! Authenticated route-hop envelope — S4A of
//! `docs/internal/plans/SUBNET_AUTH_PLAN.md` (D6).
//!
//! The production relay is pre-AEAD and header-only: it forwards
//! another node's ciphertext with no key for the inner packet. Nothing
//! it can read there authenticates the ingress peer — `RoutingHeader.
//! src_id` is a 32-bit unauthenticated claim, the UDP source address
//! is spoofable, `NetHeader.subnet_id` is covered by an end-to-end AAD
//! the relay cannot verify, and `peer_subnets` is self-declared. A
//! `VerifiedSubnetContext` selected by any of those would be
//! unauthenticated forwarding wearing a cryptographic costume.
//!
//! There is no way to have all three of: blind transparent UDP relay,
//! cryptographic ingress attribution, and zero per-packet
//! authentication. This module moves the third by the smallest amount
//! that works — one fixed symmetric MAC per hop:
//!
//! ```text
//! route-hop-v1
//!   hop_session_id: u64        the adjacent Noise session
//!   hop_sequence:   u64        per-edge, independent of packet AEAD
//!   RoutingHeader              mutable, 18 bytes
//!   inner Net packet           untouched, byte for byte
//!   tag: 16 bytes              keyed BLAKE2s over all of the above
//! ```
//!
//! Each gateway verifies and strips the incoming tag, mutates only the
//! outer routing header, and generates a fresh tag for the next
//! authenticated hop. The inner `NetHeader`, ciphertext, and
//! end-to-end AEAD tag are never rewritten, so the final endpoint
//! still authenticates the original sender through its own session.
//! No signature, credential-chain walk, or online lookup enters
//! forwarding.

use blake2::{
    digest::{consts::U32, Mac},
    Blake2sMac,
};

use crate::adapter::net::route::{RoutingHeader, ROUTING_HEADER_SIZE};

/// Domain separator for the route-hop MAC transcript.
pub const ROUTE_HOP_MAC_DOMAIN: &[u8] = b"net.subnet.route-hop.v1";

/// Envelope prefix: `hop_session_id` (8) + `hop_sequence` (8).
pub const ROUTE_HOP_PREFIX_SIZE: usize = 16;

/// Truncated MAC length. 16 bytes of a 256-bit keyed BLAKE2s: forgery
/// costs 2^128 online attempts against a key that dies with the
/// session, while the tag stays cheap on every hop.
pub const ROUTE_HOP_TAG_SIZE: usize = 16;

/// Bytes the envelope adds around an inner packet.
pub const ROUTE_HOP_OVERHEAD: usize = ROUTE_HOP_PREFIX_SIZE + ROUTE_HOP_TAG_SIZE;

/// How far behind the highest accepted sequence a hop packet may
/// arrive and still be considered. Reordering is normal on a UDP
/// edge; unbounded tolerance would be a replay window.
pub const ROUTE_HOP_REPLAY_WINDOW: u64 = 1024;

/// An identity-qualified next hop (SUBNET_AUTH_PLAN.md D6).
///
/// `RoutingTable::lookup` answers with a `SocketAddr` alone, which
/// cannot select a cryptographic context: an address is not an
/// identity. Protected forwarding resolves the hop to the peer
/// `node_id` whose authenticated session actually terminates there,
/// and carries the address only to send to.
///
/// The identity is the stable half. A NAT rebind moves `addr` while
/// `node_id` — and therefore the egress context and its attachment —
/// is unchanged; conversely a different identity at the same address
/// is a different hop, however familiar the address looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedNextHop {
    /// The authenticated peer this hop terminates at.
    pub node_id: u64,
    /// That peer's current address.
    pub addr: std::net::SocketAddr,
}

/// Why a route-hop envelope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteHopError {
    /// Too short to hold prefix + routing header + tag.
    Malformed,
    /// The routing header did not decode.
    BadRoutingHeader,
    /// Tag mismatch — wrong key, or any byte of the transcript
    /// altered.
    BadTag,
    /// Sequence already seen, or older than the replay window.
    Replay,
}

impl std::fmt::Display for RouteHopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "route_hop_malformed",
            Self::BadRoutingHeader => "route_hop_bad_routing_header",
            Self::BadTag => "route_hop_bad_tag",
            Self::Replay => "route_hop_replay",
        })
    }
}

impl std::error::Error for RouteHopError {}

/// Compute the 16-byte hop tag.
///
/// The transcript covers the domain, hop session id, hop sequence, the
/// complete routing header as it appears on the wire, and every byte
/// of the inner packet — so a relay cannot alter the destination,
/// resurrect TTL, swap the inner packet, or replay the envelope onto a
/// different edge without invalidating it.
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; the key parameter is [u8; 32]"
)]
pub fn compute_tag(
    key: &[u8; 32],
    hop_session_id: u64,
    hop_sequence: u64,
    routing_header_bytes: &[u8],
    inner: &[u8],
) -> [u8; ROUTE_HOP_TAG_SIZE] {
    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(key)
        .expect("BLAKE2s accepts variable-length keys up to 32 bytes");
    Mac::update(&mut mac, ROUTE_HOP_MAC_DOMAIN);
    Mac::update(&mut mac, &hop_session_id.to_le_bytes());
    Mac::update(&mut mac, &hop_sequence.to_le_bytes());
    Mac::update(&mut mac, routing_header_bytes);
    Mac::update(&mut mac, inner);
    let full = mac.finalize().into_bytes();
    let mut tag = [0u8; ROUTE_HOP_TAG_SIZE];
    tag.copy_from_slice(&full[..ROUTE_HOP_TAG_SIZE]);
    tag
}

/// Serialize an authenticated hop envelope.
///
/// Layout: `hop_session_id ‖ hop_sequence ‖ routing_header ‖ inner ‖ tag`.
pub fn seal(
    key: &[u8; 32],
    hop_session_id: u64,
    hop_sequence: u64,
    header: &RoutingHeader,
    inner: &[u8],
) -> Vec<u8> {
    let mut header_bytes = [0u8; ROUTING_HEADER_SIZE];
    header.write_at(&mut header_bytes);
    let mut out = Vec::with_capacity(ROUTE_HOP_OVERHEAD + ROUTING_HEADER_SIZE + inner.len());
    out.extend_from_slice(&hop_session_id.to_le_bytes());
    out.extend_from_slice(&hop_sequence.to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(inner);
    let tag = compute_tag(key, hop_session_id, hop_sequence, &header_bytes, inner);
    out.extend_from_slice(&tag);
    out
}

/// A verified inbound hop envelope. Borrows the inner packet rather
/// than copying it — the relay re-emits those exact bytes.
#[derive(Debug, Clone, Copy)]
pub struct OpenedHop<'a> {
    /// Adjacent session the tag verified under.
    pub hop_session_id: u64,
    /// Per-edge sequence, already replay-checked by the caller.
    pub hop_sequence: u64,
    /// The mutable outer routing header.
    pub header: RoutingHeader,
    /// The untouched inner Net packet.
    pub inner: &'a [u8],
}

/// Parse the envelope WITHOUT verifying the tag.
///
/// Split from [`open`] only so a caller can read `hop_session_id` to
/// select the right key before verification. Nothing downstream of a
/// bare `parse` may be trusted; `open` is the authenticating entry
/// point.
pub fn parse(buf: &[u8]) -> Result<OpenedHop<'_>, RouteHopError> {
    if buf.len() < ROUTE_HOP_OVERHEAD + ROUTING_HEADER_SIZE {
        return Err(RouteHopError::Malformed);
    }
    let hop_session_id =
        u64::from_le_bytes(buf[0..8].try_into().map_err(|_| RouteHopError::Malformed)?);
    let hop_sequence = u64::from_le_bytes(
        buf[8..16]
            .try_into()
            .map_err(|_| RouteHopError::Malformed)?,
    );
    let header_start = ROUTE_HOP_PREFIX_SIZE;
    let header_end = header_start + ROUTING_HEADER_SIZE;
    let header = RoutingHeader::from_bytes(&buf[header_start..header_end])
        .ok_or(RouteHopError::BadRoutingHeader)?;
    let inner = &buf[header_end..buf.len() - ROUTE_HOP_TAG_SIZE];
    Ok(OpenedHop {
        hop_session_id,
        hop_sequence,
        header,
        inner,
    })
}

/// Verify and open an inbound hop envelope.
///
/// Tag comparison is constant time: a timing oracle on the tag would
/// let an attacker forge one byte at a time.
pub fn open<'a>(key: &[u8; 32], buf: &'a [u8]) -> Result<OpenedHop<'a>, RouteHopError> {
    let opened = parse(buf)?;
    let header_start = ROUTE_HOP_PREFIX_SIZE;
    let header_end = header_start + ROUTING_HEADER_SIZE;
    let expected = compute_tag(
        key,
        opened.hop_session_id,
        opened.hop_sequence,
        &buf[header_start..header_end],
        opened.inner,
    );
    let received = &buf[buf.len() - ROUTE_HOP_TAG_SIZE..];
    if !bool::from(subtle::ConstantTimeEq::ct_eq(&expected[..], received)) {
        return Err(RouteHopError::BadTag);
    }
    Ok(opened)
}

/// Sliding replay window over one edge's hop sequence space.
///
/// Independent of the packet-AEAD sequence space by construction —
/// route-hop keys and sequences are separate, so a relay's hop
/// accounting can never disturb the end-to-end session it is carrying.
#[derive(Debug, Clone)]
pub struct HopReplayWindow {
    highest: u64,
    /// Bit `i` marks `highest - 1 - i` as seen.
    seen: u128,
    started: bool,
}

impl Default for HopReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl HopReplayWindow {
    /// Empty window.
    pub fn new() -> Self {
        Self {
            highest: 0,
            seen: 0,
            started: false,
        }
    }

    /// Admit `sequence` exactly once. Returns `Err(Replay)` for a
    /// duplicate or for anything older than the window.
    pub fn admit(&mut self, sequence: u64) -> Result<(), RouteHopError> {
        if !self.started {
            self.started = true;
            self.highest = sequence;
            return Ok(());
        }
        if sequence > self.highest {
            let advance = sequence - self.highest;
            // Mark the previous highest as seen, then shift.
            self.seen = if advance >= 128 {
                0
            } else {
                ((self.seen << 1) | 1) << (advance - 1)
            };
            self.highest = sequence;
            return Ok(());
        }
        if sequence == self.highest {
            return Err(RouteHopError::Replay);
        }
        let behind = self.highest - sequence;
        if behind > ROUTE_HOP_REPLAY_WINDOW || behind > 128 {
            return Err(RouteHopError::Replay);
        }
        let bit = 1u128 << (behind - 1);
        if self.seen & bit != 0 {
            return Err(RouteHopError::Replay);
        }
        self.seen |= bit;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::route::RoutingHeader;

    fn header() -> RoutingHeader {
        RoutingHeader::new(0xDEAD_BEEF_CAFE, 0x1234, 8)
    }

    const KEY: [u8; 32] = [0x11; 32];
    const INNER: &[u8] = b"end-to-end ciphertext that a relay must not touch";

    #[test]
    fn seal_open_round_trips_and_preserves_inner_bytes() {
        let buf = seal(&KEY, 42, 7, &header(), INNER);
        let opened = open(&KEY, &buf).expect("tag verifies");
        assert_eq!(opened.hop_session_id, 42);
        assert_eq!(opened.hop_sequence, 7);
        assert_eq!(opened.header.dest_id, header().dest_id);
        assert_eq!(opened.inner, INNER, "inner packet must be byte-identical");
    }

    /// The transcript must cover session, sequence, every routing
    /// header byte, and every inner byte — flipping any one of them
    /// invalidates the tag.
    #[test]
    fn every_transcript_field_is_covered() {
        let buf = seal(&KEY, 42, 7, &header(), INNER);

        // Wrong key.
        assert_eq!(open(&[0x22; 32], &buf).unwrap_err(), RouteHopError::BadTag);

        // Session id.
        let mut t = buf.clone();
        t[0] ^= 1;
        assert_eq!(open(&KEY, &t).unwrap_err(), RouteHopError::BadTag);

        // Sequence.
        let mut t = buf.clone();
        t[8] ^= 1;
        assert_eq!(open(&KEY, &t).unwrap_err(), RouteHopError::BadTag);

        // Every byte of the routing header, including TTL and hop
        // count — a relay must not be able to resurrect TTL.
        for i in 0..ROUTING_HEADER_SIZE {
            let mut t = buf.clone();
            let at = ROUTE_HOP_PREFIX_SIZE + i;
            t[at] ^= 1;
            let err = open(&KEY, &t).unwrap_err();
            assert!(
                matches!(err, RouteHopError::BadTag | RouteHopError::BadRoutingHeader),
                "routing header byte {i} must be covered, got {err:?}",
            );
        }

        // Every byte of the inner packet.
        let inner_start = ROUTE_HOP_PREFIX_SIZE + ROUTING_HEADER_SIZE;
        for i in 0..INNER.len() {
            let mut t = buf.clone();
            t[inner_start + i] ^= 1;
            assert_eq!(
                open(&KEY, &t).unwrap_err(),
                RouteHopError::BadTag,
                "inner byte {i} must be covered",
            );
        }

        // The tag itself.
        let mut t = buf.clone();
        let last = t.len() - 1;
        t[last] ^= 1;
        assert_eq!(open(&KEY, &t).unwrap_err(), RouteHopError::BadTag);
    }

    #[test]
    fn short_buffers_are_malformed_not_panics() {
        for len in 0..(ROUTE_HOP_OVERHEAD + ROUTING_HEADER_SIZE) {
            let buf = vec![0u8; len];
            assert_eq!(open(&KEY, &buf).unwrap_err(), RouteHopError::Malformed);
        }
    }

    #[test]
    fn replay_window_admits_once_and_tolerates_reorder() {
        let mut w = HopReplayWindow::new();
        assert!(w.admit(100).is_ok());
        // Duplicate of the highest.
        assert_eq!(w.admit(100).unwrap_err(), RouteHopError::Replay);
        // Forward progress.
        assert!(w.admit(101).is_ok());
        assert_eq!(w.admit(101).unwrap_err(), RouteHopError::Replay);
        // Reordered but inside the window.
        assert!(w.admit(99).is_ok());
        assert_eq!(w.admit(99).unwrap_err(), RouteHopError::Replay);
        // A large jump forward still rejects the old sequences it
        // moved past.
        assert!(w.admit(100_000).is_ok());
        assert_eq!(w.admit(101).unwrap_err(), RouteHopError::Replay);
        assert_eq!(w.admit(99).unwrap_err(), RouteHopError::Replay);
    }

    #[test]
    fn sequences_older_than_the_window_are_refused() {
        let mut w = HopReplayWindow::new();
        assert!(w.admit(10_000).is_ok());
        assert_eq!(
            w.admit(10_000 - ROUTE_HOP_REPLAY_WINDOW - 1).unwrap_err(),
            RouteHopError::Replay,
        );
    }

    /// A tag from one edge must not verify on another: directional
    /// keys are what make an envelope non-reflectable.
    #[test]
    fn a_tag_does_not_verify_under_the_reverse_direction_key() {
        let tx = [0xAA; 32];
        let rx = [0xBB; 32];
        let buf = seal(&tx, 1, 1, &header(), INNER);
        open(&tx, &buf).expect("verifies under the sending key");
        assert_eq!(open(&rx, &buf).unwrap_err(), RouteHopError::BadTag);
    }
}
