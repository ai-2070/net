//! Hole-punch rendezvous — synchronize a simultaneous open
//! between two NATed peers via a mutually-connected relay.
//!
//! Three-message dance on [`SUBPROTOCOL_RENDEZVOUS`]:
//!
//! 1. `A → R: PunchRequest { target: B, self_reflex }` — A asks R
//!    to mediate a punch to B and hands R its current best guess
//!    of its own public `SocketAddr`.
//! 2. `R → A: PunchIntroduce { peer: B, peer_reflex, fire_at }`
//!    `R → B: PunchIntroduce { peer: A, peer_reflex, fire_at }` —
//!    R tells each side the other's reflexive address and when to
//!    fire.
//! 3. At `fire_at`, A and B each send keep-alives to the other's
//!    reflex. Whichever side observes inbound from the punched
//!    path first sends `PunchAck` via the routed-handshake path
//!    (not the punched one — we don't yet know the punched path
//!    is reliable) and begins the Noise handshake on the punched
//!    socket.
//! 4. If no `PunchAck` inside a 5 s window, both sides declare
//!    the punch failed and fall back to routed-handshake. No
//!    internal retry — the single-shot contract is load-bearing
//!    (plan decision 10).
//!
//! # Wire format
//!
//! Each message is carried inside the existing event-frame wrapper
//! on [`SUBPROTOCOL_RENDEZVOUS`]:
//!
//! ```text
//! ┌──────────┬─────────────────────────────────────┐
//! │ kind (1) │ body (N)                            │
//! └──────────┴─────────────────────────────────────┘
//! ```
//!
//! - `kind` is the discriminator: `0x01 = PunchRequest`,
//!   `0x02 = PunchIntroduce`, `0x03 = PunchAck`,
//!   `0x04 = PunchReject`.
//! - Addresses are encoded as `family(1) | addr(16) | port(2)` —
//!   the same 19-byte shape used by the reflex subprotocol, so a
//!   future migration can share the codec without a wire bump.
//! - Multi-byte integers are big-endian.
//!
//! ## PunchRequest body (8 + 19 = 27 bytes)
//!
//! ```text
//! ┌──────────────────┬─────────────────────────────┐
//! │ target_node (8)  │ self_reflex (19)            │
//! └──────────────────┴─────────────────────────────┘
//! ```
//!
//! `target_node` is the `node_id` of the peer the requester wants
//! to punch to. `self_reflex` is the requester's current best
//! guess of its own public `SocketAddr` — R uses this to stamp
//! into B's `PunchIntroduce` if R doesn't have a fresher reflex
//! in its capability cache.
//!
//! ## PunchIntroduce body (8 + 19 + 8 = 35 bytes)
//!
//! ```text
//! ┌────────────┬─────────────────────────┬─────────────────┐
//! │ peer (8)   │ peer_reflex (19)        │ fire_at_ms (8)  │
//! └────────────┴─────────────────────────┴─────────────────┘
//! ```
//!
//! `peer` is the other endpoint's `node_id`. `fire_at_ms` is Unix
//! epoch milliseconds — the synchronized punch-time both sides
//! use to schedule their keep-alives.
//!
//! ## PunchAck body (8 + 8 + 4 = 20 bytes)
//!
//! ```text
//! ┌─────────────────┬───────────────┬───────────────┐
//! │ from_peer (8)   │ to_peer (8)   │ punch_id (4)  │
//! └─────────────────┴───────────────┴───────────────┘
//! ```
//!
//! `from_peer` + `to_peer` make PunchAck forwarding unambiguous:
//! the coordinator receives a PunchAck on an endpoint's session,
//! reads `to_peer` to decide where to forward, and the final
//! recipient reads `from_peer` to correlate with the punch
//! attempt it initiated. `punch_id` is a u32 correlation token
//! echoed from the originating `PunchRequest`. The plan only
//! names `peer` + `punch_id` but silently requires two different
//! identities during forwarding — this module makes them both
//! explicit so the coordinator doesn't have to rewrite bytes
//! mid-flight.
//!
//! ## PunchReject body (8 + 1 = 9 bytes)
//!
//! ```text
//! ┌─────────────────┬──────────────┐
//! │ target (8)      │ reason (1)   │
//! └─────────────────┴──────────────┘
//! ```
//!
//! `R → A` only. A coordinator that won't mediate a `PunchRequest`
//! (rate-limited, no cached target reflex, no session with the
//! target, or a failed anti-reflection check) sends this instead of
//! a `PunchIntroduce`. `target` echoes the request's target so the
//! requester resolves the matching pending-introduce waiter (keyed
//! by target node id); `reason` is a [`RejectReason`]. The requester
//! surfaces it as [`super::TraversalError::RendezvousRejected`]
//! immediately, rather than blocking until `punch_deadline`.
//!
//! # Keep-alive packet
//!
//! Keep-alives are the pre-session half of the rendezvous. At
//! `fire_at`, each endpoint sends three keep-alives to the other
//! endpoint's `peer_reflex`. These packets exist solely to open
//! NAT connection-tracking rows (outbound traffic primes inbound
//! acceptance). They **do not** ride the event-frame wrapper —
//! no session exists between A and B at fire time, so a Net
//! packet with MAGIC header wouldn't decrypt. Instead:
//!
//! ```text
//! ┌──────────────────┬──────────────────┬───────────────┐
//! │ KEEPALIVE_MAGIC  │ sender_node_id   │ punch_id      │
//! │ (2)              │ (8)              │ (4)           │
//! └──────────────────┴──────────────────┴───────────────┘
//! ```
//!
//! Total 14 bytes, distinct from both Net packet (MAGIC-prefixed,
//! ≥ 80 bytes) and pingwave (72 bytes). The receive loop pre-
//! processes any 14-byte packet starting with `KEEPALIVE_MAGIC`
//! before session lookup; matching packets fire an observer
//! oneshot keyed by the source `SocketAddr` and do not continue
//! down the session-decrypt path.
//!
//! The observer firing is what triggers the `PunchAck` —
//! "the counterpart's keep-alive reached me, so my outbound NAT
//! row is confirmed." On localhost this is equivalent to the
//! auto-emit-on-introduce shortcut (there's no NAT to confirm),
//! but on a real NAT the observer is the actual
//! connectivity signal.
//!
//! # Framing (not correctness)
//!
//! Rendezvous is an optimization, not a connectivity requirement.
//! A failed punch or a rejected `PunchRequest` doesn't prevent two
//! peers from exchanging traffic — they still have the routed-
//! handshake path. Docstrings added here must not imply the
//! rendezvous path is required for NATed peers to communicate.
//!
//! [`SUBPROTOCOL_RENDEZVOUS`]: super::SUBPROTOCOL_RENDEZVOUS

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use bytes::{BufMut, Bytes, BytesMut};

/// Length of the address-encoding block: `family(1) + addr(16) +
/// port(2) = 19 bytes`. Identical shape to
/// [`super::reflex::RESPONSE_LEN`] on purpose so the codecs can
/// share logic in a future refactor without a wire bump.
const ADDR_LEN: usize = 19;

const FAMILY_V4: u8 = 4;
const FAMILY_V6: u8 = 6;

const KIND_PUNCH_REQUEST: u8 = 0x01;
const KIND_PUNCH_INTRODUCE: u8 = 0x02;
const KIND_PUNCH_ACK: u8 = 0x03;
const KIND_PUNCH_REJECT: u8 = 0x04;

/// Total on-wire size of a `PunchRequest` payload.
/// `kind(1) + target_node(8) + self_reflex(19) = 28`.
pub const PUNCH_REQUEST_LEN: usize = 1 + 8 + ADDR_LEN;

/// Total on-wire size of a `PunchIntroduce` payload.
/// `kind(1) + peer(8) + peer_reflex(19) + fire_at_ms(8) = 36`.
pub const PUNCH_INTRODUCE_LEN: usize = 1 + 8 + ADDR_LEN + 8;

/// Total on-wire size of a `PunchAck` payload.
/// `kind(1) + from_peer(8) + to_peer(8) + punch_id(4) = 21`.
pub const PUNCH_ACK_LEN: usize = 1 + 8 + 8 + 4;

/// Total on-wire size of a `PunchReject` payload.
/// `kind(1) + target(8) + reason(1) = 10`.
pub const PUNCH_REJECT_LEN: usize = 1 + 8 + 1;

/// Two-byte magic prefix identifying a pre-session keep-alive
/// packet. Chosen to be disjoint from the Net packet `MAGIC`
/// (`0x4E45` on the wire) so the receive loop can discriminate
/// without parsing. Value: ASCII `"PH"` (Punch Hello).
///
/// Byte-order: the magic is compared against
/// `u16::from_le_bytes([data[0], data[1]])` at receive time, so
/// the on-wire bytes are `[b'P', b'H']` = `[0x50, 0x48]` and the
/// little-endian u16 is `0x4850`.
pub const KEEPALIVE_MAGIC: u16 = 0x4850;

/// On-wire length of a keep-alive packet: magic(2) +
/// sender_node_id(8) + punch_id(4) = 14 bytes. Distinct from
/// pingwave's 72 bytes and from the minimum Net packet length.
pub const KEEPALIVE_LEN: usize = 2 + 8 + 4;

/// Decoded keep-alive packet. Produced by [`decode_keepalive`]
/// when the receive loop recognizes a packet by its 14-byte
/// length and `KEEPALIVE_MAGIC` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keepalive {
    /// The sending endpoint's `node_id`. Load-bearing: the
    /// observer correlates inbound keep-alives against the peer
    /// it's expecting to hear from; without the sender id, a
    /// stray packet on the right source addr would falsely
    /// signal "punch succeeded."
    pub sender_node_id: u64,
    /// Correlation token echoed from the originating
    /// `PunchRequest`. Same value that rides `PunchAck`, so an
    /// ack reaching the initiator can be matched against the
    /// keep-alive that triggered it.
    pub punch_id: u32,
}

/// Encode a keep-alive packet. Output length is always
/// [`KEEPALIVE_LEN`].
///
/// All three fields are little-endian. An earlier revision mixed
/// encodings (LE magic, BE body via `BytesMut::put_u64` /
/// `put_u32`, which default to big-endian) — the round-trip
/// worked within the crate because `decode_keepalive` used
/// `from_be_bytes` for the body, but it diverged from the Net
/// packet header (LE). Anyone reading the codec against the wire
/// layout, or reusing these helpers outside the crate, would
/// mis-correlate. Cubic flagged this as P2; unified on LE here.
///
/// Note: the rendezvous `Punch*` messages elsewhere in this file
/// still use big-endian `put_u64` / `put_u16` with matching
/// `from_be_bytes` on the decode side — round-trip is consistent
/// internally but diverges from the Net header's LE convention.
/// That's an unresolved wire-format nit (`BUGS.md` INFO entry),
/// not a functional bug.
pub fn encode_keepalive(ka: &Keepalive) -> Bytes {
    let mut buf = BytesMut::with_capacity(KEEPALIVE_LEN);
    buf.put_slice(&KEEPALIVE_MAGIC.to_le_bytes());
    buf.put_u64_le(ka.sender_node_id);
    buf.put_u32_le(ka.punch_id);
    debug_assert_eq!(buf.len(), KEEPALIVE_LEN);
    buf.freeze()
}

/// Decode a keep-alive packet. Returns `None` if the length
/// doesn't match or the magic prefix is wrong. A packet that
/// fails this check isn't a keep-alive and should continue down
/// the normal receive-loop dispatch path.
///
/// Matches [`encode_keepalive`]: little-endian throughout.
pub fn decode_keepalive(data: &[u8]) -> Option<Keepalive> {
    if data.len() != KEEPALIVE_LEN {
        return None;
    }
    let magic = u16::from_le_bytes([data[0], data[1]]);
    if magic != KEEPALIVE_MAGIC {
        return None;
    }
    let sender_node_id = u64::from_le_bytes(data[2..10].try_into().ok()?);
    let punch_id = u32::from_le_bytes(data[10..14].try_into().ok()?);
    Some(Keepalive {
        sender_node_id,
        punch_id,
    })
}

/// A `PunchRequest` payload — A → R ("please mediate a punch to
/// `target`").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchRequest {
    /// The peer `A` wants to punch to. R looks up the peer's
    /// `reflex_addr` from its capability cache; if missing, R
    /// rejects the request with a typed error and A falls back to
    /// routed-handshake (plan §3 coordinator step 1).
    pub target: u64,
    /// A's current best guess of its own public `SocketAddr`.
    /// R forwards this into B's `PunchIntroduce` — it's an
    /// optimization, not load-bearing: R may override with a
    /// fresher reflex observation from its own cache.
    pub self_reflex: SocketAddr,
}

/// Why a coordinator refused to mediate a `PunchRequest`. Rides a
/// [`PunchReject`] back to the requester so its `request_punch`
/// resolves immediately with a typed
/// [`super::TraversalError::RendezvousRejected`] instead of waiting
/// out `punch_deadline`. Unknown values decode to
/// [`RejectReason::Unspecified`] so a future reason code never turns
/// a reject into a dropped packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RejectReason {
    /// The coordinator's per-requester `PunchRequest` budget is
    /// exhausted for the current window.
    RateLimited = 0x01,
    /// The coordinator holds no cached reflex for the named target,
    /// so it cannot introduce the pair.
    UnknownTargetReflex = 0x02,
    /// The coordinator has no live session with the named target.
    NoSessionWithTarget = 0x03,
    /// The requester's `self_reflex` IP disagreed with its session
    /// source (anti-reflection guard, Finding 1).
    ReflexMismatch = 0x04,
    /// Reason not recognized by this build — treated as a generic
    /// refusal. Never sent; only produced by [`decode`] for
    /// forward-compatibility.
    Unspecified = 0xFF,
}

impl RejectReason {
    /// Wire byte.
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a wire byte. Unrecognized values map to
    /// [`RejectReason::Unspecified`] rather than failing, so a newer
    /// coordinator's reason code degrades gracefully.
    pub fn from_u8(b: u8) -> Self {
        match b {
            0x01 => Self::RateLimited,
            0x02 => Self::UnknownTargetReflex,
            0x03 => Self::NoSessionWithTarget,
            0x04 => Self::ReflexMismatch,
            _ => Self::Unspecified,
        }
    }

    /// Stable machine-readable sub-kind, embedded in the
    /// `RendezvousRejected(_)` message string. Never localized.
    pub fn kind(self) -> &'static str {
        match self {
            Self::RateLimited => "rate-limited",
            Self::UnknownTargetReflex => "unknown-target-reflex",
            Self::NoSessionWithTarget => "no-session-with-target",
            Self::ReflexMismatch => "reflex-mismatch",
            Self::Unspecified => "unspecified",
        }
    }
}

/// A `PunchIntroduce` payload — R → A and R → B ("here's the
/// other endpoint's reflex and when to fire").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchIntroduce {
    /// The other endpoint's `node_id`.
    pub peer: u64,
    /// The other endpoint's public `SocketAddr` — the address
    /// this endpoint's keep-alive packets should target.
    pub peer_reflex: SocketAddr,
    /// Unix epoch milliseconds — the synchronized punch-time.
    /// Both endpoints subtract `now()` and schedule keep-alives
    /// relative to the resulting offset. Sub-millisecond drift
    /// between the two sides is fine — the keep-alive train
    /// spans 250 ms and a firewall state-install is faster than
    /// that.
    pub fire_at_ms: u64,
}

/// A `PunchAck` payload — the side that first observed inbound
/// traffic on the punched path tells the other side the punch
/// succeeded. Sent via the routed-handshake path, not the punched
/// one — we don't yet know the punched path is symmetric-reliable.
///
/// Carries both endpoints' identities so the coordinator can
/// forward the ack to `to_peer` without rewriting wire bytes,
/// and the recipient can correlate with the punch attempt it
/// initiated by reading `from_peer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchAck {
    /// The endpoint that observed the punch and emitted the ack.
    /// On the ack arriving at the final recipient, this field
    /// names the peer the recipient's punch attempt targeted.
    pub from_peer: u64,
    /// The endpoint the ack is addressed to. The coordinator
    /// dispatches the ack to this peer when it's not the
    /// coordinator itself.
    pub to_peer: u64,
    /// Correlation token echoed from the originating
    /// `PunchRequest`. Stage-3b wiring generates these; stage 3a
    /// preserves them on the wire.
    pub punch_id: u32,
}

/// A `PunchReject` payload — R → A ("I won't mediate this punch").
/// Sent instead of a `PunchIntroduce` so the requester's
/// `request_punch` resolves immediately with a typed error rather
/// than blocking until `punch_deadline`. `target` echoes the
/// `PunchRequest.target` so the requester can resolve the matching
/// waiter (which is keyed by target node id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchReject {
    /// The target the rejected `PunchRequest` named — lets the
    /// requester correlate against its pending-introduce waiter.
    pub target: u64,
    /// Why the coordinator refused. Wire-encoded via
    /// [`RejectReason::as_u8`].
    pub reason: RejectReason,
}

/// Decoded rendezvous subprotocol message. The variants correspond
/// to the event-frame payload shapes described in the module docs.
/// Use [`decode`] to obtain one from raw bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendezvousMsg {
    /// A → R: please mediate a punch to `target`.
    PunchRequest(PunchRequest),
    /// R → {A, B}: here's the peer's reflex + the fire time.
    PunchIntroduce(PunchIntroduce),
    /// {A, B} → {A, B}: punch succeeded on my side (sent via
    /// routed-handshake, not the punched path).
    PunchAck(PunchAck),
    /// R → A: refused to mediate (rate-limit / unknown target /
    /// anti-reflection). Fast typed failure in place of a silent
    /// drop + `punch_deadline` timeout.
    PunchReject(PunchReject),
}

impl RendezvousMsg {
    /// Encode the message as an event-body `Bytes`. Length is
    /// exactly one of [`PUNCH_REQUEST_LEN`], [`PUNCH_INTRODUCE_LEN`],
    /// [`PUNCH_ACK_LEN`], or [`PUNCH_REJECT_LEN`] depending on the
    /// variant.
    pub fn encode(&self) -> Bytes {
        match self {
            RendezvousMsg::PunchRequest(req) => encode_punch_request(req),
            RendezvousMsg::PunchIntroduce(intro) => encode_punch_introduce(intro),
            RendezvousMsg::PunchAck(ack) => encode_punch_ack(ack),
            RendezvousMsg::PunchReject(rej) => encode_punch_reject(rej),
        }
    }
}

fn encode_addr(buf: &mut BytesMut, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(v4) => {
            buf.put_u8(FAMILY_V4);
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&v4.ip().octets());
            buf.put_slice(&bytes);
            buf.put_u16(v4.port());
        }
        SocketAddr::V6(v6) => {
            buf.put_u8(FAMILY_V6);
            buf.put_slice(&v6.ip().octets());
            buf.put_u16(v6.port());
        }
    }
}

fn decode_addr(bytes: &[u8]) -> Option<SocketAddr> {
    if bytes.len() != ADDR_LEN {
        return None;
    }
    let family = bytes[0];
    let addr_bytes: [u8; 16] = bytes[1..17].try_into().ok()?;
    let port = u16::from_be_bytes([bytes[17], bytes[18]]);
    let ip = match family {
        FAMILY_V4 => IpAddr::V4(Ipv4Addr::new(
            addr_bytes[0],
            addr_bytes[1],
            addr_bytes[2],
            addr_bytes[3],
        )),
        FAMILY_V6 => IpAddr::V6(Ipv6Addr::from(addr_bytes)),
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn encode_punch_request(req: &PunchRequest) -> Bytes {
    let mut buf = BytesMut::with_capacity(PUNCH_REQUEST_LEN);
    buf.put_u8(KIND_PUNCH_REQUEST);
    buf.put_u64(req.target);
    encode_addr(&mut buf, req.self_reflex);
    debug_assert_eq!(buf.len(), PUNCH_REQUEST_LEN);
    buf.freeze()
}

fn encode_punch_introduce(intro: &PunchIntroduce) -> Bytes {
    let mut buf = BytesMut::with_capacity(PUNCH_INTRODUCE_LEN);
    buf.put_u8(KIND_PUNCH_INTRODUCE);
    buf.put_u64(intro.peer);
    encode_addr(&mut buf, intro.peer_reflex);
    buf.put_u64(intro.fire_at_ms);
    debug_assert_eq!(buf.len(), PUNCH_INTRODUCE_LEN);
    buf.freeze()
}

fn encode_punch_ack(ack: &PunchAck) -> Bytes {
    let mut buf = BytesMut::with_capacity(PUNCH_ACK_LEN);
    buf.put_u8(KIND_PUNCH_ACK);
    buf.put_u64(ack.from_peer);
    buf.put_u64(ack.to_peer);
    buf.put_u32(ack.punch_id);
    debug_assert_eq!(buf.len(), PUNCH_ACK_LEN);
    buf.freeze()
}

fn encode_punch_reject(rej: &PunchReject) -> Bytes {
    let mut buf = BytesMut::with_capacity(PUNCH_REJECT_LEN);
    buf.put_u8(KIND_PUNCH_REJECT);
    buf.put_u64(rej.target);
    buf.put_u8(rej.reason.as_u8());
    debug_assert_eq!(buf.len(), PUNCH_REJECT_LEN);
    buf.freeze()
}

/// Decode a rendezvous payload. Returns `None` on any malformed
/// input (wrong length for the claimed kind, unknown kind
/// discriminator, unknown address family byte). Callers drop
/// malformed payloads silently — the subprotocol is an
/// optimization, so a bad packet is neither fatal nor worth
/// surfacing.
pub fn decode(payload: &[u8]) -> Option<RendezvousMsg> {
    let &kind = payload.first()?;
    match kind {
        KIND_PUNCH_REQUEST => {
            if payload.len() != PUNCH_REQUEST_LEN {
                return None;
            }
            let target = u64::from_be_bytes(payload[1..9].try_into().ok()?);
            let self_reflex = decode_addr(&payload[9..28])?;
            Some(RendezvousMsg::PunchRequest(PunchRequest {
                target,
                self_reflex,
            }))
        }
        KIND_PUNCH_INTRODUCE => {
            if payload.len() != PUNCH_INTRODUCE_LEN {
                return None;
            }
            let peer = u64::from_be_bytes(payload[1..9].try_into().ok()?);
            let peer_reflex = decode_addr(&payload[9..28])?;
            let fire_at_ms = u64::from_be_bytes(payload[28..36].try_into().ok()?);
            Some(RendezvousMsg::PunchIntroduce(PunchIntroduce {
                peer,
                peer_reflex,
                fire_at_ms,
            }))
        }
        KIND_PUNCH_ACK => {
            if payload.len() != PUNCH_ACK_LEN {
                return None;
            }
            let from_peer = u64::from_be_bytes(payload[1..9].try_into().ok()?);
            let to_peer = u64::from_be_bytes(payload[9..17].try_into().ok()?);
            let punch_id = u32::from_be_bytes(payload[17..21].try_into().ok()?);
            Some(RendezvousMsg::PunchAck(PunchAck {
                from_peer,
                to_peer,
                punch_id,
            }))
        }
        KIND_PUNCH_REJECT => {
            if payload.len() != PUNCH_REJECT_LEN {
                return None;
            }
            let target = u64::from_be_bytes(payload[1..9].try_into().ok()?);
            let reason = RejectReason::from_u8(payload[9]);
            Some(RendezvousMsg::PunchReject(PunchReject { target, reason }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(addr: &str) -> SocketAddr {
        addr.parse().unwrap()
    }

    #[test]
    fn punch_request_roundtrip_ipv4() {
        let req = PunchRequest {
            target: 0x1122_3344_5566_7788,
            self_reflex: sa("192.0.2.1:9001"),
        };
        let encoded = RendezvousMsg::PunchRequest(req).encode();
        assert_eq!(encoded.len(), PUNCH_REQUEST_LEN);
        match decode(&encoded) {
            Some(RendezvousMsg::PunchRequest(out)) => assert_eq!(out, req),
            other => panic!("expected PunchRequest, got {other:?}"),
        }
    }

    #[test]
    fn punch_request_roundtrip_ipv6() {
        let req = PunchRequest {
            target: 42,
            self_reflex: sa("[2001:db8::1]:443"),
        };
        let encoded = RendezvousMsg::PunchRequest(req).encode();
        match decode(&encoded) {
            Some(RendezvousMsg::PunchRequest(out)) => assert_eq!(out, req),
            other => panic!("expected PunchRequest, got {other:?}"),
        }
    }

    /// Byte-level regression test for the [`PunchRequest`] wire
    /// layout + the size math in the module doc header.
    ///
    /// History: the doc used to claim `PunchRequest body (12 + 19 =
    /// 31 bytes)`, which was wrong twice over — the body is
    /// `target_node(8) + self_reflex(19) = 27 bytes`, and the
    /// total on-wire payload is `kind(1) + 27 = 28 bytes`
    /// ([`PUNCH_REQUEST_LEN`]). A reviewer flagged the doc
    /// inconsistency; this test pins the layout so the doc and
    /// code can't drift silently again.
    ///
    /// Assertions:
    ///
    /// - Total length is `PUNCH_REQUEST_LEN` (28 bytes).
    /// - Byte 0 is the kind discriminator (`KIND_PUNCH_REQUEST = 0x01`).
    /// - Bytes 1..9 are `target_node` as big-endian u64.
    /// - Bytes 9..28 are the 19-byte `self_reflex` socket-addr
    ///   block (family byte + 16-byte address + big-endian port).
    ///   For IPv4 the address block is zero-padded in its upper
    ///   12 bytes — same shape as `reflex::encode_response`.
    #[test]
    fn punch_request_wire_layout_matches_doc() {
        let req = PunchRequest {
            target: 0x0102_0304_0506_0708,
            self_reflex: sa("192.0.2.7:9001"),
        };
        let encoded = RendezvousMsg::PunchRequest(req).encode();

        // Size matches PUNCH_REQUEST_LEN and the documented
        // "body (8 + 19 = 27) + kind (1) = 28" math.
        assert_eq!(encoded.len(), PUNCH_REQUEST_LEN, "total wire length");
        assert_eq!(PUNCH_REQUEST_LEN, 28, "kind(1) + body(27)");
        assert_eq!(
            PUNCH_REQUEST_LEN - 1,
            27,
            "body = 8 (target) + 19 (self_reflex)",
        );

        // Byte 0: kind discriminator.
        assert_eq!(encoded[0], 0x01, "kind byte = KIND_PUNCH_REQUEST");

        // Bytes 1..9: target_node big-endian.
        assert_eq!(
            &encoded[1..9],
            &0x0102_0304_0506_0708_u64.to_be_bytes(),
            "target_node big-endian at offset 1",
        );

        // Bytes 9..28: self_reflex socket-addr block (family + 16 + port).
        assert_eq!(encoded[9], FAMILY_V4, "family byte at offset 9");
        assert_eq!(&encoded[10..14], &[192, 0, 2, 7], "IPv4 in low 4 bytes");
        assert_eq!(
            &encoded[14..26],
            &[0u8; 12],
            "upper 12 bytes of address field zero-padded for IPv4",
        );
        assert_eq!(
            &encoded[26..28],
            &9001_u16.to_be_bytes(),
            "port big-endian at offset 26",
        );
    }

    #[test]
    fn punch_introduce_roundtrip() {
        let intro = PunchIntroduce {
            peer: 0xDEAD_BEEF_FEED_CAFE,
            peer_reflex: sa("198.51.100.5:54321"),
            fire_at_ms: 1_700_000_000_500,
        };
        let encoded = RendezvousMsg::PunchIntroduce(intro).encode();
        assert_eq!(encoded.len(), PUNCH_INTRODUCE_LEN);
        match decode(&encoded) {
            Some(RendezvousMsg::PunchIntroduce(out)) => assert_eq!(out, intro),
            other => panic!("expected PunchIntroduce, got {other:?}"),
        }
    }

    #[test]
    fn punch_ack_roundtrip() {
        let ack = PunchAck {
            from_peer: 7,
            to_peer: 42,
            punch_id: 0xCAFEBABE,
        };
        let encoded = RendezvousMsg::PunchAck(ack).encode();
        assert_eq!(encoded.len(), PUNCH_ACK_LEN);
        match decode(&encoded) {
            Some(RendezvousMsg::PunchAck(out)) => assert_eq!(out, ack),
            other => panic!("expected PunchAck, got {other:?}"),
        }
    }

    #[test]
    fn punch_ack_from_and_to_are_distinguishable_on_wire() {
        // Regression guard: if encode/decode accidentally swap the
        // two identities, the coordinator would forward the ack
        // back to the initiator, dropping both sides into a
        // timeout. Assert by constructing an ack with visibly
        // different from/to and verifying neither slot swaps.
        let ack = PunchAck {
            from_peer: 0x1111_1111_1111_1111,
            to_peer: 0x2222_2222_2222_2222,
            punch_id: 0x3333_3333,
        };
        let encoded = RendezvousMsg::PunchAck(ack).encode();
        match decode(&encoded) {
            Some(RendezvousMsg::PunchAck(out)) => {
                assert_eq!(out.from_peer, 0x1111_1111_1111_1111);
                assert_eq!(out.to_peer, 0x2222_2222_2222_2222);
            }
            other => panic!("expected PunchAck, got {other:?}"),
        }
    }

    #[test]
    fn punch_reject_roundtrip() {
        for reason in [
            RejectReason::RateLimited,
            RejectReason::UnknownTargetReflex,
            RejectReason::NoSessionWithTarget,
            RejectReason::ReflexMismatch,
        ] {
            let rej = PunchReject {
                target: 0xDEAD_BEEF_0000_1234,
                reason,
            };
            let encoded = RendezvousMsg::PunchReject(rej).encode();
            assert_eq!(encoded.len(), PUNCH_REJECT_LEN);
            match decode(&encoded) {
                Some(RendezvousMsg::PunchReject(out)) => assert_eq!(out, rej),
                other => panic!("expected PunchReject, got {other:?}"),
            }
        }
    }

    #[test]
    fn punch_reject_unknown_reason_decodes_to_unspecified() {
        // A reason byte this build doesn't recognize must still decode
        // as a reject (never a dropped packet) so a newer coordinator's
        // reason code degrades gracefully.
        let mut payload = vec![0u8; PUNCH_REJECT_LEN];
        payload[0] = KIND_PUNCH_REJECT;
        payload[9] = 0x7E; // unknown reason
        match decode(&payload) {
            Some(RendezvousMsg::PunchReject(out)) => {
                assert_eq!(out.reason, RejectReason::Unspecified);
            }
            other => panic!("expected PunchReject, got {other:?}"),
        }
    }

    #[test]
    fn punch_reject_wrong_length_rejects() {
        // Kind byte says reject but the body is short — must decode
        // as None, never panic.
        let mut payload = vec![0u8; PUNCH_REJECT_LEN - 1];
        payload[0] = KIND_PUNCH_REJECT;
        assert!(decode(&payload).is_none());
    }

    #[test]
    fn reject_reason_kind_strings_are_stable() {
        assert_eq!(RejectReason::RateLimited.kind(), "rate-limited");
        assert_eq!(
            RejectReason::UnknownTargetReflex.kind(),
            "unknown-target-reflex"
        );
        assert_eq!(
            RejectReason::NoSessionWithTarget.kind(),
            "no-session-with-target"
        );
        assert_eq!(RejectReason::ReflexMismatch.kind(), "reflex-mismatch");
    }

    #[test]
    fn unknown_kind_rejects() {
        // Length matches PunchAck but kind byte is outside the
        // reserved vocabulary. Must decode as `None`, never panic.
        let mut payload = vec![0u8; PUNCH_ACK_LEN];
        payload[0] = 0xFF;
        assert!(decode(&payload).is_none());
    }

    #[test]
    fn empty_payload_rejects() {
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn wrong_length_rejects_per_kind() {
        // A kind byte that claims PunchRequest but carries an
        // incorrect body length is malformed. Tests each kind's
        // length guard — regression protection for a decoder that
        // forgets to check length after reading the kind byte.
        let short_request = vec![KIND_PUNCH_REQUEST; PUNCH_REQUEST_LEN - 1];
        assert!(decode(&short_request).is_none());

        let short_introduce = vec![KIND_PUNCH_INTRODUCE; PUNCH_INTRODUCE_LEN - 1];
        assert!(decode(&short_introduce).is_none());

        let short_ack = vec![KIND_PUNCH_ACK; PUNCH_ACK_LEN - 1];
        assert!(decode(&short_ack).is_none());

        // Too-long is also rejected — extra trailing bytes are
        // never silently ignored.
        let long_ack = vec![KIND_PUNCH_ACK; PUNCH_ACK_LEN + 1];
        assert!(decode(&long_ack).is_none());
    }

    #[test]
    fn unknown_address_family_rejects() {
        // Build an otherwise-valid PunchRequest payload but with
        // an unknown address-family byte (neither 4 nor 6). Must
        // decode as None, not panic or produce a garbage addr.
        let mut payload = vec![0u8; PUNCH_REQUEST_LEN];
        payload[0] = KIND_PUNCH_REQUEST;
        // target = 0 (bytes 1..9 left at 0)
        // address family at byte 9 — set to invalid
        payload[9] = 7;
        assert!(decode(&payload).is_none());
    }

    #[test]
    fn keepalive_roundtrip() {
        let ka = Keepalive {
            sender_node_id: 0xA1B2_C3D4_E5F6_0718,
            punch_id: 0x1234_5678,
        };
        let encoded = encode_keepalive(&ka);
        assert_eq!(encoded.len(), KEEPALIVE_LEN);
        match decode_keepalive(&encoded) {
            Some(out) => assert_eq!(out, ka),
            None => panic!("decode_keepalive returned None on a valid packet"),
        }
    }

    /// Regression test for a cubic-flagged P2 bug: an earlier
    /// revision encoded the magic in LE but the body in BE (via
    /// the default `BytesMut::put_u64` / `put_u32`). The intra-
    /// crate round-trip worked because the decoder used
    /// `from_be_bytes` to match, but the layout diverged from
    /// every other wire field in the mesh (packet header / all
    /// other traversal codecs are LE). Anyone reading the codec
    /// against the wire dump, or reusing it outside this crate,
    /// would mis-correlate. This test pins the layout to
    /// little-endian throughout by asserting the exact byte
    /// sequence for a known fixture.
    #[test]
    fn keepalive_byte_layout_is_all_little_endian() {
        let ka = Keepalive {
            sender_node_id: 0x0102_0304_0506_0708,
            punch_id: 0x1A2B_3C4D,
        };
        let encoded = encode_keepalive(&ka);
        let expected: [u8; 14] = [
            // magic 0x4850 LE
            0x50, 0x48,
            // sender_node_id 0x0102030405060708 LE (least-significant byte first)
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // punch_id 0x1A2B3C4D LE
            0x4D, 0x3C, 0x2B, 0x1A,
        ];
        assert_eq!(
            &encoded[..],
            &expected[..],
            "keep-alive must be little-endian on the wire — \
             mixing endianness with the packet header is fragile",
        );
    }

    #[test]
    fn keepalive_magic_is_distinct_from_net_magic() {
        // The receive loop uses `u16::from_le_bytes([d[0], d[1]]) != MAGIC`
        // to route to the pingwave path; we use the same little-endian
        // read to recognize keep-alives. The two discriminators MUST be
        // different or a keep-alive would either be mis-routed to
        // pingwave or mis-parsed as a Net packet.
        use crate::adapter::net::protocol::MAGIC;
        assert_ne!(
            KEEPALIVE_MAGIC, MAGIC,
            "KEEPALIVE_MAGIC collides with Net packet MAGIC",
        );
    }

    #[test]
    fn keepalive_wrong_length_rejects() {
        // A 14-byte packet that doesn't start with KEEPALIVE_MAGIC
        // is not a keep-alive. Similarly, anything outside
        // KEEPALIVE_LEN isn't recognized. Guards against a receive
        // loop that "almost" recognizes a keep-alive.
        let mut too_short = vec![0u8; KEEPALIVE_LEN - 1];
        too_short[0..2].copy_from_slice(&KEEPALIVE_MAGIC.to_le_bytes());
        assert!(decode_keepalive(&too_short).is_none());

        let mut too_long = vec![0u8; KEEPALIVE_LEN + 1];
        too_long[0..2].copy_from_slice(&KEEPALIVE_MAGIC.to_le_bytes());
        assert!(decode_keepalive(&too_long).is_none());

        let mut wrong_magic = vec![0u8; KEEPALIVE_LEN];
        wrong_magic[0..2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        assert!(decode_keepalive(&wrong_magic).is_none());
    }

    #[test]
    fn encoded_kind_byte_matches_discriminator() {
        // Explicit layout check — guards against a future refactor
        // that reorders the discriminator byte away from offset 0,
        // which would silently break any peer running the prior
        // version.
        let req = PunchRequest {
            target: 1,
            self_reflex: sa("10.0.0.1:1"),
        };
        let intro = PunchIntroduce {
            peer: 1,
            peer_reflex: sa("10.0.0.1:1"),
            fire_at_ms: 1,
        };
        let ack = PunchAck {
            from_peer: 1,
            to_peer: 1,
            punch_id: 1,
        };
        assert_eq!(
            RendezvousMsg::PunchRequest(req).encode()[0],
            KIND_PUNCH_REQUEST
        );
        assert_eq!(
            RendezvousMsg::PunchIntroduce(intro).encode()[0],
            KIND_PUNCH_INTRODUCE
        );
        assert_eq!(RendezvousMsg::PunchAck(ack).encode()[0], KIND_PUNCH_ACK);
    }
}
