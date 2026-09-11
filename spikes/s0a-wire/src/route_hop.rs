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
    digest::{consts::U32, KeyInit, Mac},
    Blake2sMac,
};

use crate::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE};

/// Domain separator for the route-hop MAC transcript.
pub const ROUTE_HOP_MAC_DOMAIN: &[u8] = b"net.subnet.route-hop.v1";

/// Wire discriminator, `"RH"`.
///
/// Not in the plan's D6 field list, but dispatch classifies an
/// inbound datagram on its first two bytes (`ROUTING_MAGIC` /
/// `MAGIC`), and a bare `hop_session_id` there is random — it would
/// alias a legacy routing packet roughly once in 65 536. An explicit
/// magic keeps classification exact instead of probabilistic, and
/// rides in the MAC transcript like every other byte.
pub const ROUTE_HOP_MAGIC: u16 = 0x5248;

/// Envelope prefix: magic (2) + `hop_session_id` (8) + `hop_sequence` (8).
pub const ROUTE_HOP_PREFIX_SIZE: usize = 18;

/// Truncated MAC length. 16 bytes of a 256-bit keyed BLAKE2s: forgery
/// costs 2^128 online attempts against a key that dies with the
/// session, while the tag stays cheap on every hop.
pub const ROUTE_HOP_TAG_SIZE: usize = 16;

/// Bytes the envelope adds around an inner packet.
pub const ROUTE_HOP_OVERHEAD: usize = ROUTE_HOP_PREFIX_SIZE + ROUTE_HOP_TAG_SIZE;

/// How far behind the highest accepted sequence a hop packet may
/// arrive and still be admitted. Reordering is normal on a UDP edge;
/// unbounded tolerance would BE a replay window rather than bound one.
///
/// This is exactly the bitmap width in [`HopReplayWindow`], not an
/// independent policy number. S4A advertised 1024 while storing 128
/// bits, so sequences 129..=1024 behind passed the staleness bound
/// and then indexed a bit that did not exist. Anything wider needs
/// wider storage, not a larger constant.
pub const ROUTE_HOP_REPLAY_WINDOW: u64 = 128;

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

/// Why a route-hop operation failed, in either direction.
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
    /// Replay admission was attempted by a second concurrent caller.
    ///
    /// Production protected ingress is single-consumer (one receive
    /// loop, synchronous dispatch), so this never fires there. It
    /// exists so a concurrent misuse — a seam, a future refactor that
    /// breaks the ownership rule — drops the packet immediately
    /// instead of blocking, spinning, or corrupting the window. See
    /// [`SharedHopReplayWindow`].
    Contended,
    /// Outbound only: the caller's buffer cannot hold the sealed
    /// envelope. The one failure here that is a local sizing mistake
    /// rather than an attacker's doing — see [`sealed_len`].
    BufferTooSmall,
}

impl std::fmt::Display for RouteHopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "route_hop_malformed",
            Self::BadRoutingHeader => "route_hop_bad_routing_header",
            Self::BadTag => "route_hop_bad_tag",
            Self::Replay => "route_hop_replay",
            Self::Contended => "route_hop_contended",
            Self::BufferTooSmall => "route_hop_buffer_too_small",
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
    let mut mac = <Blake2sMac<U32> as KeyInit>::new_from_slice(key)
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

/// Exact sealed size for an inner packet of `inner_len` bytes.
///
/// The envelope is fixed-overhead, so a forwarder can size — or reuse
/// — a buffer before it has done any work.
///
/// Saturating rather than wrapping. Real packets are bounded well below
/// this by `MAX_PACKET_SIZE`, so the difference is unreachable in
/// production, but an unchecked `+` on a public helper panics in debug
/// builds and wraps in release ones for pathological lengths — and a
/// wrapped length would be a *small* number, which [`seal_into`] would
/// then happily accept as a fitting buffer. Saturating turns that into
/// `usize::MAX`, which no buffer satisfies, so the failure direction is
/// `BufferTooSmall`.
#[inline]
pub const fn sealed_len(inner_len: usize) -> usize {
    (ROUTE_HOP_OVERHEAD + ROUTING_HEADER_SIZE).saturating_add(inner_len)
}

/// Serialize an authenticated hop envelope into a caller-owned buffer,
/// returning the number of bytes written.
///
/// Layout:
/// `magic ‖ hop_session_id ‖ hop_sequence ‖ routing_header ‖ inner ‖ tag`.
///
/// This is the primitive; [`seal`] is the allocating convenience over
/// it. Forwarding is the one path here that runs per packet per hop,
/// and a `Vec` per hop puts an allocator call — and a free, and the
/// cache miss on fresh memory — between a packet arriving and leaving.
/// Nothing about the envelope needs ownership: the size is known in
/// advance from [`sealed_len`], and every byte is written exactly once
/// in order. So the buffer belongs to whatever is doing the
/// forwarding, which can hold one per worker and reuse it forever.
///
/// Writes nothing at all when `out` is too small: a partially-filled
/// buffer that a caller might still send would be worse than a clean
/// refusal.
pub fn seal_into(
    out: &mut [u8],
    key: &[u8; 32],
    hop_session_id: u64,
    hop_sequence: u64,
    header: &RoutingHeader,
    inner: &[u8],
) -> Result<usize, RouteHopError> {
    let total = sealed_len(inner.len());
    if out.len() < total {
        return Err(RouteHopError::BufferTooSmall);
    }
    let header_end = ROUTE_HOP_PREFIX_SIZE + ROUTING_HEADER_SIZE;
    let inner_end = header_end + inner.len();

    out[0..2].copy_from_slice(&ROUTE_HOP_MAGIC.to_le_bytes());
    out[2..10].copy_from_slice(&hop_session_id.to_le_bytes());
    out[10..ROUTE_HOP_PREFIX_SIZE].copy_from_slice(&hop_sequence.to_le_bytes());
    header.write_at(&mut out[ROUTE_HOP_PREFIX_SIZE..header_end]);
    out[header_end..inner_end].copy_from_slice(inner);

    // Tag the header bytes as they now sit in the buffer, so what is
    // authenticated is literally what will be sent.
    let tag = compute_tag(
        key,
        hop_session_id,
        hop_sequence,
        &out[ROUTE_HOP_PREFIX_SIZE..header_end],
        inner,
    );
    out[inner_end..total].copy_from_slice(&tag);
    Ok(total)
}

/// Allocating form of [`seal_into`], for tests and any caller not on
/// the forwarding path.
#[expect(
    clippy::expect_used,
    reason = "the buffer is sized by sealed_len on the line above, which is the same length \
              seal_into checks against; a failure here would be a contradiction in this module"
)]
pub fn seal(
    key: &[u8; 32],
    hop_session_id: u64,
    hop_sequence: u64,
    header: &RoutingHeader,
    inner: &[u8],
) -> Vec<u8> {
    let mut out = vec![0u8; sealed_len(inner.len())];
    seal_into(&mut out, key, hop_session_id, hop_sequence, header, inner)
        .expect("a buffer sized by sealed_len always fits");
    out
}

/// A MAC-verified inbound hop envelope. Borrows the inner packet
/// rather than copying it — the relay re-emits those exact bytes.
///
/// Reachable only through [`open`], so holding one means the tag
/// verified. Replay admission is a separate step the session performs
/// (`NetSession::open_route_hop`); this type does not assert it.
#[derive(Debug, Clone, Copy)]
pub struct OpenedHop<'a> {
    /// Adjacent session the tag verified under.
    pub hop_session_id: u64,
    /// Per-edge sequence. Verified as part of the transcript; whether
    /// it has been *admitted* is the caller's replay window to say.
    pub hop_sequence: u64,
    /// The mutable outer routing header.
    pub header: RoutingHeader,
    /// The untouched inner Net packet.
    pub inner: &'a [u8],
}

/// The only thing readable before authentication: which session's key
/// to try.
///
/// Deliberately NOT [`OpenedHop`]. Returning the post-verification
/// type from an unverified parse made the two indistinguishable to the
/// type system, so a caller could reach `header`/`inner` off a buffer
/// whose MAC had never been checked and nothing would complain. This
/// carries the session id and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnauthenticatedHopPrefix {
    /// Claimed adjacent session. A claim, not a fact, until
    /// [`open`] verifies the tag under that session's key.
    pub hop_session_id: u64,
}

/// Read the session id from an envelope without verifying anything.
///
/// The only legitimate use is selecting a key to hand to [`open`],
/// which is the authenticating entry point.
pub fn parse_prefix(buf: &[u8]) -> Result<UnauthenticatedHopPrefix, RouteHopError> {
    let opened = parse_unverified(buf)?;
    Ok(UnauthenticatedHopPrefix {
        hop_session_id: opened.hop_session_id,
    })
}

/// Structural decode without MAC verification. Private: everything it
/// returns is attacker-controlled until [`open`] says otherwise.
fn parse_unverified(buf: &[u8]) -> Result<OpenedHop<'_>, RouteHopError> {
    if buf.len() < ROUTE_HOP_OVERHEAD + ROUTING_HEADER_SIZE {
        return Err(RouteHopError::Malformed);
    }
    if u16::from_le_bytes([buf[0], buf[1]]) != ROUTE_HOP_MAGIC {
        return Err(RouteHopError::Malformed);
    }
    let hop_session_id = u64::from_le_bytes(
        buf[2..10]
            .try_into()
            .map_err(|_| RouteHopError::Malformed)?,
    );
    let hop_sequence = u64::from_le_bytes(
        buf[10..18]
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
    let opened = parse_unverified(buf)?;
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
    ///
    /// Bit `i` of `seen` marks `highest - 1 - i`, so the bitmap
    /// represents exactly `1..=ROUTE_HOP_REPLAY_WINDOW` behind the
    /// highest — the shift arithmetic and the staleness bound share
    /// that one constant, which is what keeps a sequence from being
    /// simultaneously "in window" and unrepresentable.
    pub fn admit(&mut self, sequence: u64) -> Result<(), RouteHopError> {
        const W: u64 = ROUTE_HOP_REPLAY_WINDOW;
        if !self.started {
            self.started = true;
            self.highest = sequence;
            return Ok(());
        }
        if sequence > self.highest {
            let advance = sequence - self.highest;
            // The old highest lands at bit `advance - 1`; older marks
            // shift up by `advance` and fall off the top.
            //
            // `advance == W` is its own arm: the old highest is still
            // representable (last slot), but `seen << W` would be a
            // shift overflow. Folding it into the `>= W` clear-all
            // branch was the S4A bug — it wiped the bitmap while
            // leaving that sequence inside the staleness bound, so a
            // duplicate exactly `W` behind was re-admitted.
            self.seen = if advance > W {
                0
            } else if advance == W {
                1u128 << (W - 1)
            } else {
                (self.seen << advance) | (1u128 << (advance - 1))
            };
            self.highest = sequence;
            return Ok(());
        }
        if sequence == self.highest {
            return Err(RouteHopError::Replay);
        }
        let behind = self.highest - sequence;
        if behind > W {
            return Err(RouteHopError::Replay);
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "behind is bounded by W = 128 immediately above, so the cast is exact"
        )]
        let bit = 1u128 << ((behind - 1) as u32);
        if self.seen & bit != 0 {
            return Err(RouteHopError::Replay);
        }
        self.seen |= bit;
        Ok(())
    }
}

/// [`HopReplayWindow`] behind `&self`, without a lock on the ordinary
/// path.
///
/// Production protected ingress is single-consumer by construction:
/// one spawned receive loop pulls packets off one `IngressReceiver`
/// and dispatches synchronously, so exactly one thread ever reaches
/// admission for a given session. A mutex on that path bought no
/// correctness — only a lock/unlock round trip per packet and a
/// blocking primitive sitting where a hostile peer's traffic is
/// processed.
///
/// This replaces it with fixed-size atomic fields and a
/// compare-exchange claim: the single production consumer always wins
/// the claim uncontended; a second concurrent caller — which can only
/// mean the ownership rule was broken — is refused **immediately**
/// with [`RouteHopError::Contended`], dropping that packet. Fail
/// closed, no waiting, no spinning, no retry loop, no allocation.
///
/// Memory ordering: the successful `Acquire` claim synchronizes-with
/// the previous holder's `Release` publish, so the relaxed field
/// accesses in between are data-race-free and always see the previous
/// admission's state.
#[derive(Debug)]
pub struct SharedHopReplayWindow {
    /// Claim flag — `false` when free. Never waited on: a failed
    /// claim is an immediate drop, not a spin.
    claimed: core::sync::atomic::AtomicBool,
    started: core::sync::atomic::AtomicBool,
    highest: core::sync::atomic::AtomicU64,
    /// The 128-bit `seen` bitmap, split into halves — no 128-bit
    /// atomic exists on the supported targets, and the claim already
    /// makes the pair single-writer.
    seen_lo: core::sync::atomic::AtomicU64,
    seen_hi: core::sync::atomic::AtomicU64,
}

impl Default for SharedHopReplayWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII release for [`SharedHopReplayWindow`]'s claim. Dropping —
/// including on an unwind — publishes the fields (`Release`) and
/// frees the claim, so a panicking caller can never leave the window
/// permanently claimed and every later packet dropping as
/// `Contended`.
struct ReplayClaim<'a>(&'a SharedHopReplayWindow);

impl Drop for ReplayClaim<'_> {
    fn drop(&mut self) {
        self.0
            .claimed
            .store(false, core::sync::atomic::Ordering::Release);
    }
}

impl SharedHopReplayWindow {
    /// Empty window. `const`, so a session embeds it inline — no
    /// first-packet (or any-packet) allocation.
    pub const fn new() -> Self {
        use core::sync::atomic::{AtomicBool, AtomicU64};
        Self {
            claimed: AtomicBool::new(false),
            started: AtomicBool::new(false),
            highest: AtomicU64::new(0),
            seen_lo: AtomicU64::new(0),
            seen_hi: AtomicU64::new(0),
        }
    }

    /// One compare-exchange claim attempt. `None` means another
    /// caller holds the window right now — the caller drops, it never
    /// waits. Private: the tests use it directly to make contention
    /// DETERMINISTIC (hold the claim, assert `admit` refuses) instead
    /// of hoping a thread race manifests one.
    fn try_claim(&self) -> Option<ReplayClaim<'_>> {
        use core::sync::atomic::Ordering;
        // `.then(..)`, NOT `.then_some(..)`: the guard must be
        // constructed lazily, only on a WON claim. `then_some` builds
        // its argument eagerly, and dropping that temporary on the
        // lost-claim path would run `ReplayClaim::drop` — releasing
        // the claim the OTHER caller legitimately holds. The
        // deterministic contention witness below turns red on exactly
        // that regression.
        self.claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then(|| ReplayClaim(self))
    }

    /// Admit `sequence` exactly once — [`HopReplayWindow::admit`]'s
    /// contract, plus [`RouteHopError::Contended`] for a concurrent
    /// second caller. The window state is unchanged on every error.
    pub fn admit(&self, sequence: u64) -> Result<(), RouteHopError> {
        use core::sync::atomic::Ordering;
        let Some(claim) = self.try_claim() else {
            return Err(RouteHopError::Contended);
        };
        // Exclusive until `claim` drops. Run the one tested admission
        // algorithm on a local copy rather than a second
        // implementation on the atomics.
        let mut window = HopReplayWindow {
            highest: self.highest.load(Ordering::Relaxed),
            seen: (u128::from(self.seen_hi.load(Ordering::Relaxed)) << 64)
                | u128::from(self.seen_lo.load(Ordering::Relaxed)),
            started: self.started.load(Ordering::Relaxed),
        };
        let result = window.admit(sequence);
        if result.is_ok() {
            self.highest.store(window.highest, Ordering::Relaxed);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "deliberate: the low half of the split u128 bitmap"
            )]
            self.seen_lo.store(window.seen as u64, Ordering::Relaxed);
            self.seen_hi
                .store((window.seen >> 64) as u64, Ordering::Relaxed);
            self.started.store(window.started, Ordering::Relaxed);
        }
        drop(claim);
        result
    }
}

