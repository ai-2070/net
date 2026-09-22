//! Sessions: the NKpsk0 handshake as initiator, and the session
//! table keyed by node id.
//!
//! The leaf owns no second Noise implementation and no second packet
//! builder. This module is the *sequence* — handshake, install,
//! build, open — expressed over `net-mesh-wire`, which is the same
//! crate a native `MeshNode` links.
//!
//! **The prologue is the load-bearing detail.**
//! `handshake_prologue(routing_id(self), routing_id(peer))` is what
//! `MeshNode::accept_rtc` binds on the responder side; a leaf that
//! got it wrong would fail the handshake with a MAC error and no
//! clue. `routing_id` is the 32-bit projection `mesh.rs` feeds in,
//! reproduced here because both halves must agree byte for byte.
//!
//! **The responder static key comes from the credential**, never
//! from `GET /rtc/anchor`. That is the whole content of the MITM
//! witness: an impostor serving its own key over HTTP must fail the
//! handshake. This module takes the key as an argument and never
//! fetches one.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
use net_wire::parsed_packet::ParsedPacket;
use net_wire::peer_addr::{PeerAddr, RtcPeerId};
use net_wire::pool::PacketBuilder;
use net_wire::protocol::{EventFrame, PacketFlags, HEADER_SIZE, TAG_SIZE};
use net_wire::reliability::RetransmitDescriptor;
use net_wire::session::NetSession;
use net_wire::stream_window::{
    StreamAckRanges, StreamNack, StreamWindow, SUBPROTOCOL_STREAM_ACK, SUBPROTOCOL_STREAM_NACK,
    SUBPROTOCOL_STREAM_RESET, SUBPROTOCOL_STREAM_WINDOW,
};

use crate::control_plane::NodeId;
use crate::error::{LeafError, Result};
use crate::frame::split_payload;

/// Packet-builder pool size per session.
///
/// The browser leaf is single-threaded (S0b), so the pool exists for
/// buffer reuse, not for contention. Four builders cover a send that
/// fragments while another is in flight without reserving 8 KiB ×
/// pool-size per peer.
const POOL_SIZE: usize = 4;

/// Retransmit stamps a session retains at once.
///
/// A stamp is the header a retransmitted packet must be rebuilt
/// with. It is retired when the peer's cumulative ack passes its
/// sequence, so the live set is the reliable window; the cap is the
/// backstop for a peer that never acks, at ~32 bytes an entry.
///
/// **A session bound over per-stream ownership, so it is RESERVED at
/// admission rather than enforced by eviction.** Descriptors are
/// per stream and each stream's own retransmit window bounds them,
/// so nine streams each admitting 128 tiny reliable packets own
/// 1 152 descriptors while every per-stream check still passes. The
/// pre-repair table absorbed that by deleting its smallest key —
/// ordered `(stream_id, seq)`, so the casualty was the *lowest*
/// stream's oldest stamp, a packet another stream had been admitted
/// to own and could no longer rebuild ([`LeafSession::rebuild`]
/// skips a descriptor with no stamp, and neither a NACK nor an RTO
/// recovers it). One stream's traffic could therefore silently
/// strip a different stream's recoverability.
///
/// Sizing the table to the sum of the per-stream bounds is not
/// available: stream ids are created on demand and each new one adds
/// its whole window, so that sum has no ceiling and a cap derived
/// from it would not be a cap. The bound stays, and
/// [`LeafSession::build_packets`] refuses a message it cannot stamp
/// — the same whole-message admission the per-stream descriptor
/// check already performs, reported as
/// [`LeafError::ReliableWindowFull`]. Refusing is recoverable (an
/// ack retires stamps and the caller retries); evicting another
/// stream's ownership is not.
const MAX_RETRANSMIT_STAMPS: usize = 1_024;

/// Process-wide session incarnation counter.
///
/// Every installed session gets an id no replaced session can ever
/// hold again. Receive, reorder, reassembly and call-ownership state
/// key on it, which is what makes "the old session's work" a set the
/// node can retire exactly once — a peer node id cannot express that,
/// because the successor has the same one.
static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1);

fn next_incarnation() -> u64 {
    NEXT_INCARNATION.fetch_add(1, Ordering::Relaxed)
}

/// On-wire bytes one packet carrying `event_bytes` of framed events
/// costs, which is the unit both credit halves count in.
///
/// `mesh.rs::wire_bytes_for_payload`, reproduced because the two
/// ends must agree: the native sender debits
/// `events + HEADER_SIZE + TAG_SIZE` and the native receiver credits
/// the same quantity, so a leaf that counted body bytes alone would
/// drift its peer's window by 84 bytes a packet.
#[inline]
fn wire_bytes(event_bytes: usize) -> u32 {
    event_bytes
        .saturating_add(HEADER_SIZE + TAG_SIZE)
        .min(u32::MAX as usize) as u32
}

/// The framed size of one event inside a packet's payload.
#[inline]
pub fn event_frame_bytes(events: &[Bytes]) -> usize {
    events.iter().map(|e| EventFrame::LEN_SIZE + e.len()).sum()
}

/// Whether `subprotocol_id` is one of the four stream-control
/// messages — the credit and reliability feedback loop itself.
#[inline]
pub fn is_stream_control(subprotocol_id: u16) -> bool {
    matches!(
        subprotocol_id,
        SUBPROTOCOL_STREAM_WINDOW
            | SUBPROTOCOL_STREAM_NACK
            | SUBPROTOCOL_STREAM_RESET
            | SUBPROTOCOL_STREAM_ACK
    )
}

/// The 32-bit routing projection of a node id — `mesh.rs`'s
/// `routing_id`, which both halves of the handshake feed into the
/// prologue.
#[inline]
pub fn routing_id(node_id: NodeId) -> u64 {
    (node_id as u32) as u64
}

/// The endpoint a leaf's peer sits behind.
///
/// A browser has no socket address, so inventing a UDP tuple would
/// be a lie the session table then keys on. `PeerAddr::Rtc` names
/// exactly what is true: one DataChannel, identified by the slot the
/// leaf's own transport owns it in.
#[inline]
pub fn rtc_addr(slot: u32, generation: u32) -> PeerAddr {
    PeerAddr::Rtc(RtcPeerId { slot, generation })
}

/// An initiator handshake in flight.
///
/// [`Self::read_msg2`] takes the Noise state only once it has built
/// the session, so an owner that keeps this entry in flight across a
/// message 2 that fails to validate still holds a handshake the real
/// message 2 can complete. A handshake is nonetheless read **at most
/// once**: the state is taken on success, and a later read is refused
/// rather than producing a second session. The take is behind `&self`
/// deliberately — the whole point is that a failed read must not
/// require ownership to have been surrendered.
///
/// No `Debug`: `NoiseHandshake` has none, and a derived one would be
/// a formatter over live handshake state.
pub struct PendingHandshake {
    /// `None` once [`Self::read_msg2`] built the session.
    handshake: RefCell<Option<NoiseHandshake>>,
    peer: NodeId,
    addr: PeerAddr,
}

impl PendingHandshake {
    /// Build the initiator state against the **credential-pinned**
    /// responder static key and produce Noise message 1, already
    /// wrapped in the Net handshake packet the peer's dispatcher
    /// recognises (`PacketBuilder::build_handshake`, the same call
    /// `handshake_initiator` makes natively).
    pub fn initiate(
        psk: &[u8; 32],
        responder_static: &[u8; 32],
        self_node: NodeId,
        peer_node: NodeId,
        addr: PeerAddr,
    ) -> Result<(Self, Bytes)> {
        let prologue = handshake_prologue(routing_id(self_node), routing_id(peer_node));
        let mut handshake =
            NoiseHandshake::initiator_with_prologue(psk, responder_static, &prologue)
                .map_err(|e| LeafError::Session(format!("initiator handshake state: {e}")))?;
        let msg1 = handshake
            .write_message(&[])
            .map_err(|e| LeafError::Session(format!("noise message 1: {e}")))?;
        // A handshake packet is unencrypted, so the builder's key is
        // never used; the session key does not exist yet.
        let mut builder = PacketBuilder::new(&[0u8; 32], 0);
        let packet = builder.build_handshake(&msg1);
        Ok((
            Self {
                handshake: RefCell::new(Some(handshake)),
                peer: peer_node,
                addr,
            },
            packet,
        ))
    }

    /// The peer this handshake is with.
    #[inline]
    pub fn peer(&self) -> NodeId {
        self.peer
    }

    /// Read the peer's Noise message 2 — delivered as a Net
    /// handshake packet — and build the session it completes.
    ///
    /// **A failed read takes nothing.** The shape checks run before
    /// the state is touched and the state itself is taken only once
    /// the session is built, so a message that does not complete this
    /// handshake leaves the pending alive for the one that does —
    /// `CallTable::deliver`'s rule that a frame which is not ours
    /// neither completes the attempt nor takes its slot. The one
    /// caveat is the wire crate's: Noise `read_message` is not
    /// rollback-safe, so a *well-formed* message that fails its tag
    /// check has already mixed its ephemeral into the transcript and
    /// cannot be followed by another message 2. What never happens
    /// anymore is the entry itself dying — and the mis-routing that
    /// used to follow it — as a side effect of an error return.
    pub fn read_msg2(&self, raw: &[u8]) -> Result<LeafSession> {
        let parsed = ParsedPacket::parse(Bytes::copy_from_slice(raw), self.addr)
            .ok_or_else(|| LeafError::Wire("message 2 did not parse as a packet".into()))?;
        if !parsed.header.flags.is_handshake() {
            return Err(LeafError::Wire(
                "expected a handshake packet for message 2".into(),
            ));
        }
        let mut slot = self.handshake.borrow_mut();
        let handshake = slot
            .as_mut()
            .ok_or_else(|| LeafError::Session("message 2 was already read".into()))?;
        handshake
            .read_message(&parsed.payload)
            .map_err(|e| LeafError::Session(format!("noise message 2: {e}")))?;
        if !handshake.is_finished() {
            return Err(LeafError::Session(
                "handshake did not complete after message 2".into(),
            ));
        }
        // Read before `into_session_keys` consumes the state: this
        // is the transcript an establishment proof is signed over
        // (`crate::establish`), and the only projection the session
        // keys publish is an eight-byte session name.
        let handshake_hash = handshake
            .handshake_hash()
            .map_err(|e| LeafError::Session(format!("handshake transcript: {e}")))?;
        // The state is taken exactly here — on success.
        let keys = slot
            .take()
            .ok_or_else(|| LeafError::Session("message 2 was already read".into()))?
            .into_session_keys()
            .map_err(|e| LeafError::Session(format!("session keys: {e}")))?;
        Ok(LeafSession::install(
            self.peer,
            NetSession::new(keys, self.addr, POOL_SIZE, false),
            handshake_hash,
        ))
    }

    /// The **responder** half: consume a peer's Noise message 1 and
    /// return the installed session together with the message-2
    /// packet to put on the wire.
    ///
    /// §9 needs this and the anchor never did: a browser ↔ browser
    /// attempt has no anchor to be the responder, so one of the two
    /// leaves answers. There is no `PendingHandshake` to hold in
    /// between — NKpsk0's responder finishes on message 1 — which is
    /// why this returns a `LeafSession` directly rather than a
    /// two-step type.
    ///
    /// **The two things that keep this as strong as the initiator
    /// half.** The PSK is the trust domain's, so an attempt from
    /// outside it fails the MAC; and `initiator` — a *claim*, since
    /// nothing has authenticated it yet — goes into the
    /// **prologue**, exactly as `MeshNode::accept_rtc` puts a
    /// browser's claimed node id there. A peer that claimed one node
    /// id and is treated as another cannot complete the handshake,
    /// so the session this installs is bound to the id it is keyed
    /// under. The responder's own static key is its identity's,
    /// never a key read off the wire.
    pub fn respond(
        psk: &[u8; 32],
        static_keypair: &StaticKeypair,
        initiator: NodeId,
        self_node: NodeId,
        addr: PeerAddr,
        msg1: &[u8],
    ) -> Result<(LeafSession, Bytes)> {
        // The initiator computed `prologue(self, peer)`, so the
        // responder's order is (initiator, us). Getting this
        // backwards fails with a MAC error and no clue, which is why
        // both halves live in one module.
        let prologue = handshake_prologue(routing_id(initiator), routing_id(self_node));
        let mut handshake = NoiseHandshake::responder_with_prologue(psk, static_keypair, &prologue)
            .map_err(|e| LeafError::Session(format!("responder handshake state: {e}")))?;

        let parsed = ParsedPacket::parse(Bytes::copy_from_slice(msg1), addr)
            .ok_or_else(|| LeafError::Wire("message 1 did not parse as a packet".into()))?;
        if !parsed.header.flags.is_handshake() {
            return Err(LeafError::Wire(
                "expected a handshake packet for message 1".into(),
            ));
        }
        handshake
            .read_message(&parsed.payload)
            .map_err(|e| LeafError::Session(format!("noise message 1: {e}")))?;
        let msg2 = handshake
            .write_message(&[])
            .map_err(|e| LeafError::Session(format!("noise message 2: {e}")))?;
        if !handshake.is_finished() {
            return Err(LeafError::Session(
                "handshake did not complete after message 2".into(),
            ));
        }
        // Unencrypted, like message 1: the session key does not
        // exist until both halves have read.
        let packet = PacketBuilder::new(&[0u8; 32], 0).build_handshake(&msg2);
        let handshake_hash = handshake
            .handshake_hash()
            .map_err(|e| LeafError::Session(format!("handshake transcript: {e}")))?;
        let keys = handshake
            .into_session_keys()
            .map_err(|e| LeafError::Session(format!("session keys: {e}")))?;
        Ok((
            LeafSession::install(
                initiator,
                NetSession::new(keys, addr, POOL_SIZE, false),
                handshake_hash,
            ),
            packet,
        ))
    }
}

/// One inbound packet, decrypted and unframed.
#[derive(Debug, Clone)]
pub struct OpenedPacket {
    /// `subprotocol_id` from the header — what the dispatcher routes
    /// on.
    pub subprotocol_id: u16,
    /// The `u16` channel-hash hint.
    pub channel_hash: u16,
    /// The publisher's full 64-bit origin hash.
    pub origin_hash: u64,
    /// Stream this packet belongs to.
    pub stream_id: u64,
    /// Per-stream sequence.
    pub sequence: u64,
    /// Whether the sender marked it reliable.
    pub reliable: bool,
    /// `Some(sequence)` when the sender stamped this packet
    /// [`PacketFlags::MODE_BOUNDARY`]: its sequence is the FIRST
    /// reliable one on this stream, so everything below is
    /// fire-and-forget and conceded and everything from here on is
    /// a reliable obligation. `None` on every other packet — which
    /// is not "the boundary is zero", it is "this packet says
    /// nothing about the boundary".
    pub mode_boundary: Option<u64>,
    /// Fragment group id.
    pub fragment_id: u16,
    /// Byte offset of this piece within its group.
    pub fragment_offset: u16,
    /// `frag_flags`, interpreted by [`crate::frame`].
    pub frag_flags: u8,
    /// The event frames the packet carried.
    pub events: Vec<Bytes>,
}

/// The header one retransmitted packet must be rebuilt with.
///
/// `RetransmitDescriptor` carries the stream, sequence, events and
/// flags — everything the wire layer needs to decide *whether* to
/// resend, and nothing that says where the packet was addressed on
/// the event plane. A leaf packet's channel hint, origin hash and
/// fragment position all live in the header and are authenticated by
/// the AEAD, so a rebuild that dropped them would arrive as a
/// differently-addressed, differently-positioned packet. The sender
/// keeps them beside the wire window and retires them on ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PacketStamp {
    subprotocol_id: u16,
    channel_hash: u16,
    origin_hash: u64,
    fragment_id: u16,
    fragment_offset: u16,
    frag_flags: u8,
}

/// One established session with one peer.
#[derive(Debug)]
pub struct LeafSession {
    peer: NodeId,
    session: NetSession,
    next_fragment_id: Cell<u16>,
    /// This session's process-unique incarnation.
    incarnation: u64,
    /// The final Noise handshake hash of the establishment that
    /// produced this session.
    ///
    /// Retained because it is the transcript an establishment proof
    /// signs ([`crate::establish`]) and the only value that ties a
    /// signature to *this* handshake rather than to the pair of
    /// identities, which is stable across attempts. Not secret: it
    /// is the running hash of material both endpoints sent, which is
    /// why Noise names it as the channel binding.
    handshake_hash: [u8; 32],
    /// Rebuild headers for packets still in the retransmit window,
    /// keyed `(stream_id, sequence)`.
    stamps: RefCell<BTreeMap<(u64, u64), PacketStamp>>,
}

impl LeafSession {
    /// Wrap a freshly negotiated wire session, minting its
    /// incarnation.
    fn install(peer: NodeId, session: NetSession, handshake_hash: [u8; 32]) -> Self {
        Self {
            peer,
            session,
            next_fragment_id: Cell::new(1),
            incarnation: next_incarnation(),
            handshake_hash,
            stamps: RefCell::new(BTreeMap::new()),
        }
    }

    /// The establishment transcript this session was negotiated
    /// under — what an establishment proof is verified over.
    #[inline]
    pub fn handshake_hash(&self) -> &[u8; 32] {
        &self.handshake_hash
    }

    /// The peer's node id.
    #[inline]
    pub fn peer(&self) -> NodeId {
        self.peer
    }

    /// This session's incarnation — unique for the life of the
    /// process, so a replacement never collides with what it
    /// replaced.
    #[inline]
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// The session id both halves derived from the handshake.
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.session.session_id()
    }

    /// The underlying wire session, for the stream and credit state
    /// the wire crate owns.
    #[inline]
    pub fn wire(&self) -> &NetSession {
        &self.session
    }

    /// Fragment group ids, wrapping but never zero.
    ///
    /// Zero is what an unfragmented packet carries, so reserving it
    /// keeps "no group" and "group 0" distinguishable on the wire.
    fn next_fragment_id(&self) -> u16 {
        let id = self.next_fragment_id.get();
        self.next_fragment_id
            .set(if id == u16::MAX { 1 } else { id + 1 });
        id
    }

    /// Build the packets that carry `payload` to this peer.
    ///
    /// One `Batch` per packet (S0c) and one event per packet: the
    /// caller's payload is the event. A payload above
    /// [`crate::frame::MAX_FRAGMENT_PAYLOAD`] becomes several
    /// packets sharing a fragment group; one above
    /// [`crate::frame::MAX_FRAGMENTED_PAYLOAD`] is refused, not
    /// truncated and not dropped.
    ///
    /// Each packet consumes one stream sequence, which is what makes
    /// the consumer-side reorder work: fragments of one payload are
    /// contiguous sequences on the same stream.
    ///
    /// **This is where the wire's send-side machinery is driven.**
    /// The whole payload is admitted against the stream's send
    /// credit **before** any sequence is consumed — a fragmented
    /// message is admitted whole or refused whole, synchronously and
    /// bounded, never half-sent — and every reliable packet is
    /// registered with the stream's reliability mode together with
    /// the header a retransmit must be rebuilt with. Sharing
    /// `NetSession` does not do either of these by itself: the wire
    /// APIs are passive and want a driver.
    pub fn build_packets(
        &self,
        stream_id: u64,
        subprotocol_id: u16,
        channel_hash: u16,
        origin_hash: u64,
        reliable: bool,
        payload: &[u8],
    ) -> Result<Vec<Bytes>> {
        let fragments = split_payload(payload)?;
        let fragment_id = if fragments.len() > 1 {
            self.next_fragment_id()
        } else {
            0
        };
        // **The stream's mode is the contract; a handle's flag is a
        // request.** A channel's publish stream id is derived from
        // the channel, so fire-and-forget and reliable producers
        // land on one id and one sequence space, and a
        // still-open fire-and-forget handle keeps sending after a
        // reliable one promoted the stream. Leaving those packets
        // fire-and-forget puts unrebuildable sequences INSIDE the
        // reliable region of a shared sequence space: the receiver
        // must then either wait forever for a retransmit that
        // cannot come, or skip past them — and skipping takes the
        // cursor past reliable records it is still assembling, which
        // is how a complete reliable message came to be discarded as
        // a duplicate.
        //
        // So the promoted stream inherits, and the alternative —
        // refusing a fire-and-forget send on a promoted stream,
        // typed — is deliberately NOT what happens here. Refusing
        // would break a working publisher because some unrelated
        // subscriber on the same channel asked for reliability,
        // turning a third party's choice into this caller's error.
        // Inheriting cannot break anyone: reliable is strictly
        // stronger than fire-and-forget, so a caller that asked for
        // "may be lost" and got "will not be lost" received
        // everything it asked for. It also costs the send a
        // retransmit descriptor and a stamp, which is exactly what
        // makes the receiver's obligation answerable.
        //
        // Stream-control traffic is exempt: it is the credit and
        // reliability loop itself and rides `CONTROL_STREAM_ID`
        // outside the window, for the same reason the receive path
        // does not reorder it.
        let control = is_stream_control(subprotocol_id);
        let reliable = reliable
            || (!control
                && self
                    .session
                    .try_stream(stream_id)
                    .is_some_and(|s| s.tx_promoted()));
        self.session.open_stream_with(stream_id, reliable, 1);

        // The stream-control subprotocols carry the credit loop
        // itself. Gating a grant or an ack on the credit it exists
        // to replenish would deadlock the stream it is refilling, so
        // they ride outside the window — the same reason the receive
        // path does not reorder them.
        if !control {
            // **Three independent bounds, all checked before any
            // sequence is consumed.** Credit is bytes; the
            // retransmit window is a count of descriptors, and
            // small messages exhaust the second first — 129
            // one-byte reliable sends cost ~11.5 KiB against a
            // 64 KiB window and overrun a 128-descriptor cap. Past
            // it `ReliableStream::on_send` evicts the oldest
            // still-unacknowledged descriptor: the packet is on the
            // wire, nothing can rebuild it, and neither a NACK nor
            // an RTO recovers it. So packet ownership is reserved
            // as well as byte credit, and the whole message is
            // admitted or refused — never half-owned.
            //
            // The third bound is the session's, not the stream's:
            // every retained descriptor also needs a header stamp,
            // and the stamp table is shared by every stream
            // ([`MAX_RETRANSMIT_STAMPS`]). Per-stream descriptor
            // headroom can therefore be available on THIS stream
            // while the session has no stamp left, and the
            // pre-repair table made room by deleting another
            // stream's oldest stamp. Reserving here is what makes
            // "admitted" mean the sender can still rebuild every
            // packet it owns — on every stream, not just this one.
            if reliable {
                let headroom = self
                    .session
                    .get_or_create_stream(stream_id)
                    .with_reliability(|r| r.retransmit_headroom());
                if let Some(remaining) = headroom {
                    if fragments.len() > remaining {
                        return Err(LeafError::ReliableWindowFull {
                            stream_id,
                            needed: fragments.len(),
                            remaining,
                        });
                    }
                }
                let stamp_headroom =
                    MAX_RETRANSMIT_STAMPS.saturating_sub(self.stamps.borrow().len());
                if fragments.len() > stamp_headroom {
                    return Err(LeafError::ReliableWindowFull {
                        stream_id,
                        needed: fragments.len(),
                        remaining: stamp_headroom,
                    });
                }
            }
            let needed: u32 = fragments
                .iter()
                .map(|f| wire_bytes(EventFrame::LEN_SIZE + f.data.len()))
                .fold(0u32, u32::saturating_add);
            let admitted = {
                let stream = self.session.get_or_create_stream(stream_id);
                if stream.try_acquire_tx_credit(needed) {
                    None
                } else {
                    Some(stream.tx_credit_remaining())
                }
            };
            if let Some(remaining) = admitted {
                return Err(LeafError::Backpressure {
                    stream_id,
                    needed,
                    remaining,
                });
            }
        }

        let mut out = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let offset = fragment.offset;
            let frag_flags = fragment.flags;
            let stream = self.session.get_or_create_stream(stream_id);
            let seq = stream.next_tx_seq();
            // The promotion boundary is STAMPED, not inferred: this
            // is the sender's first reliable sequence on the stream,
            // and saying so is what lets the receiver concede the
            // fire-and-forget sequences below it without
            // acknowledging a packet it never got. True exactly
            // once per stream, and the flag rides the descriptor
            // too, so a lost boundary packet re-announces itself on
            // retransmit.
            let flags = match reliable {
                true if stream.promote_tx_at(seq) => {
                    PacketFlags::RELIABLE.with(PacketFlags::MODE_BOUNDARY)
                }
                true => PacketFlags::RELIABLE,
                false => PacketFlags::NONE,
            };
            drop(stream);
            let events = [fragment.data];
            let mut builder = self.session.thread_local_pool().get();
            builder.set_channel_hash(channel_hash);
            builder.set_origin_hash(origin_hash);
            builder.set_fragment(fragment_id, offset, frag_flags);
            out.push(builder.build_subprotocol(stream_id, seq, &events, flags, subprotocol_id));

            if reliable {
                self.retain_retransmit(
                    stream_id,
                    seq,
                    &events,
                    flags,
                    PacketStamp {
                        subprotocol_id,
                        channel_hash,
                        origin_hash,
                        fragment_id,
                        fragment_offset: offset,
                        frag_flags,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Hand one just-built reliable packet to the stream's
    /// reliability mode, and keep the header its rebuild needs.
    ///
    /// Claims nothing it cannot honour: the stamp slot is checked
    /// before the descriptor is registered, so a retransmit
    /// obligation never outlives the header its rebuild needs. The
    /// capacity was already reserved for this whole message by
    /// [`Self::build_packets`], which is why the refusal below is a
    /// defensive floor rather than a reachable path — and why it is
    /// a refusal at all. Making room by deleting the table's
    /// smallest key, as this used to, evicted the LOWEST stream's
    /// oldest stamp: another stream's admitted, still-unacknowledged
    /// packet, silently made unrebuildable by this stream's traffic.
    fn retain_retransmit(
        &self,
        stream_id: u64,
        seq: u64,
        events: &[Bytes],
        flags: PacketFlags,
        stamp: PacketStamp,
    ) {
        if self.stamps.borrow().len() >= MAX_RETRANSMIT_STAMPS {
            debug_assert!(
                false,
                "build_packets reserved {} stamp slots before admitting this message; \
                 reaching the cap here means a reliable send bypassed admission",
                MAX_RETRANSMIT_STAMPS
            );
            return;
        }
        let descriptor = Arc::new(RetransmitDescriptor {
            seq,
            stream_id,
            events: events.to_vec(),
            flags,
            // `None` deliberately: the leaf does not carry its
            // fragment stamp on the descriptor. `self.stamps` is an
            // admission RESERVATION — `build_packets` refuses a whole
            // message with `ReliableWindowFull` against
            // `MAX_RETRANSMIT_STAMPS` before consuming a sequence
            // (F3b), the cap is non-evicting since L5, and `rebuild`
            // skips a descriptor whose reservation is gone. Folding
            // the stamp onto the descriptor would leave that
            // reservation with nothing to count. The field is the
            // NATIVE sender's carrier, where no such table exists.
            fragment: None,
        });
        let Some(stream) = self.session.try_stream(stream_id) else {
            return;
        };
        stream.with_reliability(|r| r.on_send(descriptor));
        drop(stream);
        self.stamps.borrow_mut().insert((stream_id, seq), stamp);
    }

    /// Retire the stamps for every sequence below `ack_seq` on
    /// `stream_id` — the peer has them, so they can never be rebuilt.
    pub fn retire_acked_stamps(&self, stream_id: u64, ack_seq: u64) {
        let mut stamps = self.stamps.borrow_mut();
        let stale: Vec<(u64, u64)> = stamps
            .range((stream_id, 0)..(stream_id, ack_seq))
            .map(|(k, _)| *k)
            .collect();
        for key in stale {
            stamps.remove(&key);
        }
    }

    /// Retire **every** stamp `stream_id` owns, at any sequence: its
    /// send half has ended terminally, so no descriptor of its can
    /// ever be rebuilt again.
    ///
    /// Returns how many were retired, which is what a caller reports.
    ///
    /// This is the other end of the `MAX_RETRANSMIT_STAMPS` cap. That
    /// cap is deliberately non-evicting — [`Self::build_packets`]
    /// refuses a message it cannot stamp rather than deleting another
    /// stream's ownership — so the only ways a slot came back were a
    /// cumulative ack and destroying the whole session. A stream whose
    /// retransmits exhausted satisfies neither: it held its slots
    /// forever while owning nothing, and enough ended streams closed
    /// the session's reliable admission permanently — against the
    /// "use another stream id" recovery that
    /// [`LeafError::ReliableWindowFull`] exists to offer.
    ///
    /// The reservation therefore ends with **the exact owner**. The
    /// range is bounded to this one `stream_id`, so a live stream's
    /// stamps are untouched: the cap is enforced rather than relaxed,
    /// and nothing is taken from work that can still be rebuilt.
    pub fn retire_stream_stamps(&self, stream_id: u64) -> usize {
        let mut stamps = self.stamps.borrow_mut();
        let owned: Vec<(u64, u64)> = stamps
            .range((stream_id, 0)..=(stream_id, u64::MAX))
            .map(|(k, _)| *k)
            .collect();
        for key in &owned {
            stamps.remove(key);
        }
        owned.len()
    }

    /// Rebuild `descriptors` into packets with fresh AEAD counters.
    ///
    /// A retransmit cannot replay the original ciphertext — the
    /// receiver's replay window refuses the repeated counter — so
    /// each packet is rebuilt from the descriptor's pre-encryption
    /// events plus the header stamp retained at send time.
    pub fn rebuild(&self, descriptors: &[Arc<RetransmitDescriptor>]) -> Vec<Bytes> {
        let stamps = self.stamps.borrow();
        let mut out = Vec::with_capacity(descriptors.len());
        for d in descriptors {
            let Some(stamp) = stamps.get(&(d.stream_id, d.seq)) else {
                // No stamp means the packet's header is gone: a
                // rebuild would be a differently-addressed packet,
                // which is worse than not resending.
                continue;
            };
            let mut builder = self.session.thread_local_pool().get();
            builder.set_channel_hash(stamp.channel_hash);
            builder.set_origin_hash(stamp.origin_hash);
            builder.set_fragment(stamp.fragment_id, stamp.fragment_offset, stamp.frag_flags);
            out.push(builder.build_subprotocol(
                d.stream_id,
                d.seq,
                &d.events,
                d.flags,
                stamp.subprotocol_id,
            ));
        }
        out
    }

    /// Every reliable packet whose retransmit timer has expired,
    /// rebuilt and ready for the transport.
    pub fn due_retransmits(&self) -> Vec<Bytes> {
        let due = self.session.collect_timed_out_retransmits();
        if due.is_empty() {
            return Vec::new();
        }
        self.rebuild(&due)
    }

    /// Record one accepted inbound packet against the wire's
    /// receive state, and return the grant owed to its sender.
    ///
    /// `mode_boundary` is the arriving packet's
    /// [`OpenedPacket::mode_boundary`], handed to
    /// [`NetSession::get_or_create_stream_for_packet`](net_wire::session::NetSession::get_or_create_stream_for_packet)
    /// so the wire applies it BEFORE the sequence is offered to the
    /// reliability mode: it is what decides whether the sequences
    /// below it are a conceded fire-and-forget prefix or a reliable
    /// gap this receiver must keep NACKing. Applied after, the
    /// boundary packet's own sequence would be measured against a
    /// cursor the boundary was about to move — and applied by this
    /// caller rather than by the shared call, a second receive path
    /// silently gets the assumed boundary instead.
    ///
    /// `None` when the reliability layer refused the sequence (a
    /// duplicate, or past its acceptance horizon): crediting those
    /// bytes would refund the sender window for traffic that never
    /// became progress.
    pub fn note_received(
        &self,
        stream_id: u64,
        reliable: bool,
        mode_boundary: Option<u64>,
        sequence: u64,
        event_bytes: usize,
    ) -> Option<StreamWindow> {
        let stream =
            self.session
                .get_or_create_stream_for_packet(stream_id, reliable, mode_boundary);
        if !stream.with_reliability(|r| r.on_receive(sequence)) {
            return None;
        }
        stream.update_rx_seq(sequence);
        let total_consumed = stream.on_bytes_consumed(u64::from(wire_bytes(event_bytes)))?;
        let ack_seq = stream.with_reliability(|r| r.rx_ack_seq());
        stream.note_grant_sent();
        Some(StreamWindow {
            stream_id,
            total_consumed,
            ack_seq,
        })
    }

    /// The NACK this session owes `stream_id` **right now**,
    /// because the arrival just processed revealed a hole no NACK
    /// has reported yet.
    ///
    /// `None` when nothing new was revealed — so a burst stacked
    /// behind one loss reports that loss once, and steady-state
    /// traffic costs nothing. See
    /// [`ReliabilityMode::take_gap_opened`](net_wire::reliability::ReliabilityMode::take_gap_opened)
    /// for why the cumulative ack the same arrival already sends
    /// cannot do this job.
    pub fn opened_gap_nack(&self, stream_id: u64) -> Option<StreamNack> {
        let stream = self.session.try_stream(stream_id)?;
        let payload =
            stream.with_reliability(|r| r.take_gap_opened().then(|| r.build_nack()).flatten())?;
        Some(StreamNack {
            stream_id,
            next_expected: payload.next_expected,
            missing_bitmap: payload.missing_bitmap,
        })
    }

    /// The cumulative acknowledgement this session owes `stream_id`
    /// right now, without recording any consumption.
    ///
    /// A reliable packet the reliability layer refuses is a
    /// **retransmit**, and a retransmit is the peer saying it never
    /// heard the ack for what it already delivered. Its bytes are
    /// not progress — they were credited when the original arrived,
    /// and crediting them again would refund the sender a window it
    /// never spent — so nothing is consumed here. What is owed is
    /// the ack, repeated.
    ///
    /// `None` for a stream that does not exist or is not reliable:
    /// fire-and-forget has no cumulative ack to repeat.
    pub fn repeat_ack(&self, stream_id: u64) -> Option<StreamWindow> {
        let stream = self.session.try_stream(stream_id)?;
        if !stream.reliable_mode() {
            return None;
        }
        Some(StreamWindow {
            stream_id,
            total_consumed: stream.rx_credit().consumed(),
            ack_seq: stream.with_reliability(|r| r.rx_ack_seq()),
        })
    }

    /// Apply a peer's cumulative ack: prune the retransmit window
    /// and the stamps that belong to it.
    pub fn apply_ack(&self, stream_id: u64, ack_seq: u64) {
        if let Some(stream) = self.session.try_stream(stream_id) {
            stream.with_reliability(|r| r.on_ack(ack_seq));
        }
        self.retire_acked_stamps(stream_id, ack_seq);
    }

    /// Apply a peer's SACK ranges, then its cumulative ack.
    pub fn apply_ack_ranges(&self, ack: &StreamAckRanges) {
        if let Some(stream) = self.session.try_stream(ack.stream_id) {
            stream.with_reliability(|r| r.on_ack_ranges(ack.ack_seq, &ack.ranges));
        }
        self.retire_acked_stamps(ack.stream_id, ack.ack_seq);
    }

    /// The packets a peer's NACK asks this session to resend.
    pub fn nack_retransmits(&self, nack: &StreamNack) -> Vec<Bytes> {
        let payload = net_wire::protocol::NackPayload {
            next_expected: nack.next_expected,
            missing_bitmap: nack.missing_bitmap,
        };
        let Some(stream) = self.session.try_stream(nack.stream_id) else {
            return Vec::new();
        };
        let due = stream.with_reliability(|r| r.on_nack(&payload));
        drop(stream);
        self.rebuild(&due)
    }

    /// The NACKs this session's receive side owes its peer — one per
    /// stream currently missing a sequence.
    pub fn gap_nacks(&self) -> Vec<StreamNack> {
        self.session
            .collect_gap_reports(false, 0)
            .into_iter()
            .map(|report| StreamNack {
                stream_id: report.stream_id,
                next_expected: report.nack.next_expected,
                missing_bitmap: report.nack.missing_bitmap,
            })
            .collect()
    }

    /// Decrypt one inbound packet and unframe its events.
    ///
    /// The AEAD open and the anti-replay window are both the wire
    /// crate's; a packet whose counter the window refuses is an
    /// error here, never a silently accepted replay.
    pub fn open_packet(&self, raw: &[u8]) -> Result<OpenedPacket> {
        let parsed = ParsedPacket::parse(Bytes::copy_from_slice(raw), self.session.peer_addr())
            .ok_or_else(|| LeafError::Wire("packet did not parse".into()))?;
        if !parsed.header.validate() {
            return Err(LeafError::Wire(
                "packet header failed validate() — over-cap payload or bad version".into(),
            ));
        }
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(
            parsed.header.nonce[4..12]
                .try_into()
                .map_err(|_| LeafError::Wire("short nonce".into()))?,
        );
        let rx = self.session.rx_cipher();
        let plain = rx
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .map_err(|e| LeafError::Wire(format!("decrypt: {e}")))?;
        if !rx.try_admit_rx_counter(counter) {
            return Err(LeafError::Replay);
        }
        Ok(OpenedPacket {
            subprotocol_id: parsed.header.subprotocol_id,
            channel_hash: parsed.header.channel_hash,
            origin_hash: parsed.header.origin_hash,
            stream_id: parsed.header.stream_id,
            sequence: parsed.header.sequence,
            reliable: parsed.header.flags.is_reliable(),
            mode_boundary: parsed
                .header
                .flags
                .is_mode_boundary()
                .then_some(parsed.header.sequence),
            fragment_id: parsed.header.fragment_id,
            fragment_offset: parsed.header.fragment_offset,
            frag_flags: parsed.header.frag_flags,
            events: EventFrame::read_events(plain, parsed.header.event_count),
        })
    }
}

/// The session table: one entry per peer node id.
///
/// Keyed on node id and not on address — §9 step 4 replaces a routed
/// session with a direct one "rather than joining it", and that is a
/// table update, not a route install, precisely because the key is
/// the peer's identity.
#[derive(Debug, Default)]
pub struct SessionTable {
    by_node: HashMap<NodeId, LeafSession>,
}

impl SessionTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a session, **replacing** any existing one for that
    /// peer and returning what it replaced.
    pub fn install(&mut self, session: LeafSession) -> Option<LeafSession> {
        self.by_node.insert(session.peer(), session)
    }

    /// The session for `peer`, if there is one.
    pub fn get(&self, peer: NodeId) -> Option<&LeafSession> {
        self.by_node.get(&peer)
    }

    /// Drop the session for `peer`, returning it.
    pub fn remove(&mut self, peer: NodeId) -> Option<LeafSession> {
        self.by_node.remove(&peer)
    }

    /// Every peer this leaf holds a session with.
    pub fn peers(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.by_node.keys().copied()
    }

    /// Find the session whose `session_id` a packet header names.
    ///
    /// The leaf's transport delivers bytes per DataChannel, so the
    /// peer is usually known from the channel; this is the fallback
    /// for a packet that arrived without that context.
    pub fn by_session_id(&self, session_id: u64) -> Option<&LeafSession> {
        self.by_node.values().find(|s| s.session_id() == session_id)
    }

    /// How many sessions are installed.
    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_wire::crypto::StaticKeypair;

    const LEAF_NODE: NodeId = 0x1111_2222_3333_4444;
    const ANCHOR_NODE: NodeId = 0xAAAA_BBBB_CCCC_DDDD;
    const PSK: [u8; 32] = [0x4B; 32];

    /// A deterministic responder static keypair, derived through
    /// `x25519-dalek` — the same curve the Noise resolver uses, so
    /// the responder's own key agreement lands on the same shared
    /// secret. Deterministic so the handshake needs no entropy
    /// beyond what Noise itself draws.
    fn responder_keypair() -> StaticKeypair {
        let secret = x25519_dalek::StaticSecret::from([7u8; 32]);
        let public = x25519_dalek::PublicKey::from(&secret);
        StaticKeypair::from_keys([7u8; 32], *public.as_bytes())
    }

    /// The full initiator sequence against a real responder: if the
    /// prologue, the packet wrapper or the key ordering were wrong,
    /// this is where it shows.
    fn established() -> (LeafSession, NetSession) {
        let responder_static = responder_keypair();
        let prologue = handshake_prologue(routing_id(LEAF_NODE), routing_id(ANCHOR_NODE));
        let mut responder =
            NoiseHandshake::responder_with_prologue(&PSK, &responder_static, &prologue)
                .expect("responder state");

        let (pending, msg1_packet) = PendingHandshake::initiate(
            &PSK,
            responder_static.public_key(),
            LEAF_NODE,
            ANCHOR_NODE,
            rtc_addr(0, 1),
        )
        .expect("initiate");

        let parsed = ParsedPacket::parse(msg1_packet.clone(), rtc_addr(0, 1)).expect("msg1 parses");
        assert!(parsed.header.flags.is_handshake());
        responder
            .read_message(&parsed.payload)
            .expect("responder reads msg1");
        let msg2 = responder.write_message(&[]).expect("msg2");
        let msg2_packet = PacketBuilder::new(&[0u8; 32], 0).build_handshake(&msg2);

        let leaf = pending.read_msg2(&msg2_packet).expect("install");
        let responder_keys = responder.into_session_keys().expect("responder keys");
        let peer = NetSession::new(responder_keys, rtc_addr(1, 1), 2, false);
        (leaf, peer)
    }

    #[test]
    fn the_initiator_handshake_installs_a_session_both_halves_agree_on() {
        let (leaf, peer) = established();
        assert_eq!(
            leaf.session_id(),
            peer.session_id(),
            "both halves must derive the same session id from the handshake"
        );
        assert_eq!(leaf.peer(), ANCHOR_NODE);
    }

    /// A session the leaf builds must be openable by the peer — the
    /// round trip that proves the AEAD, the AAD and the framing all
    /// agree.
    #[test]
    fn a_built_packet_opens_on_the_peer_side() {
        let (leaf, peer) = established();
        let payload = b"leaf to anchor".to_vec();
        let packets = leaf
            .build_packets(
                0x0002_0000_0000_0001,
                0,
                0x1234,
                0xABCD_EF01_2345_6789,
                true,
                &payload,
            )
            .expect("build");
        assert_eq!(packets.len(), 1);

        let parsed = ParsedPacket::parse(packets[0].clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(parsed.header.session_id, peer.session_id());
        assert_eq!(parsed.header.channel_hash, 0x1234);
        assert_eq!(parsed.header.origin_hash, 0xABCD_EF01_2345_6789);
        assert_eq!(parsed.header.frag_flags, 0, "one packet is not a fragment");
        assert!(parsed.header.flags.is_reliable());

        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        let plain = peer
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("the peer decrypts");
        let events = EventFrame::read_events(plain, parsed.header.event_count);
        assert_eq!(events.len(), 1);
        assert_eq!(&events[0][..], &payload[..], "payload rides verbatim");
    }

    /// Fragmentation stamps the header fields and consumes one
    /// sequence per packet, all on the same group.
    #[test]
    fn an_over_cap_payload_becomes_a_fragment_group_on_contiguous_sequences() {
        let (leaf, _peer) = established();
        let payload = vec![0x5A; crate::frame::MAX_FRAGMENT_PAYLOAD * 2 + 1];
        let packets = leaf
            .build_packets(0x0002_0000_0000_0002, 0, 0, 0, true, &payload)
            .expect("build");
        assert_eq!(packets.len(), 3);

        let mut group = None;
        let mut seqs = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let p = ParsedPacket::parse(packet.clone(), rtc_addr(0, 1)).expect("parses");
            assert!(
                p.header.validate(),
                "every fragment must pass validate() — that is the point"
            );
            assert_ne!(p.header.fragment_id, 0, "a fragment group is never id 0");
            let g = *group.get_or_insert(p.header.fragment_id);
            assert_eq!(p.header.fragment_id, g, "one group per payload");
            assert_eq!(
                p.header.frag_flags & crate::frame::FRAG_FRAGMENTED,
                crate::frame::FRAG_FRAGMENTED
            );
            assert_eq!(
                p.header.frag_flags & crate::frame::FRAG_LAST != 0,
                i == packets.len() - 1,
                "only the last fragment sets FRAG_LAST"
            );
            seqs.push(p.header.sequence);
        }
        let first = seqs[0];
        assert_eq!(
            seqs,
            vec![first, first + 1, first + 2],
            "fragments must occupy contiguous sequences on one stream"
        );
    }

    #[test]
    fn the_session_table_replaces_rather_than_joins() {
        let (leaf, _) = established();
        let peer = leaf.peer();
        let mut table = SessionTable::new();
        assert!(table.install(leaf).is_none());
        assert_eq!(table.len(), 1);

        let (again, _) = established();
        let replaced = table.install(again).expect("the old session comes back");
        assert_eq!(replaced.peer(), peer);
        assert_eq!(table.len(), 1, "one session per peer, never two");
        assert!(table.get(peer).is_some());
        assert!(table.remove(peer).is_some());
        assert!(table.is_empty());
    }

    #[test]
    fn fragment_ids_never_repeat_zero() {
        let (leaf, _) = established();
        // Walk the counter to its wrap point.
        for _ in 0..u16::MAX {
            assert_ne!(leaf.next_fragment_id(), 0, "id 0 means unfragmented");
        }
        assert_ne!(leaf.next_fragment_id(), 0);
    }

    /// **X4: one stream's traffic must not strip another stream's
    /// recoverability.** Descriptors are per stream and each stream's
    /// own retransmit window bounds them, but the header stamps every
    /// descriptor needs to be rebuilt from live in ONE session-wide
    /// table. Eight streams each admitting a full window of tiny
    /// reliable packets fill it while every per-stream check still
    /// passes, and the pre-repair table made room by deleting its
    /// smallest key — the LOWEST stream's oldest stamp, a packet that
    /// stream had been admitted to own.
    ///
    /// Two halves. The ninth stream is refused typed, whole-message,
    /// before any sequence is consumed; and the first stream's first
    /// packet is still rebuildable, which is the ownership the
    /// eviction used to destroy silently ([`LeafSession::rebuild`]
    /// skips a descriptor whose stamp is gone, so the packet is on the
    /// wire with nothing able to resend it).
    #[test]
    fn a_full_stamp_table_refuses_a_new_stream_rather_than_evicting_an_owned_one() {
        let (leaf, _) = established();
        let base = 0x0002_0000_0000_0010u64;
        // Per-stream descriptor headroom on the default window; the
        // table holds `MAX_RETRANSMIT_STAMPS`, so this is how many
        // streams it takes to fill it exactly.
        let per_stream = net_wire::reliability::ReliableStream::max_pending_for_window(
            net_wire::stream::DEFAULT_STREAM_WINDOW_BYTES,
        );
        let streams = MAX_RETRANSMIT_STAMPS / per_stream;
        assert!(
            streams > 1,
            "the defect needs more than one stream to fit in the table"
        );

        for n in 0..streams {
            for _ in 0..per_stream {
                leaf.build_packets(base + n as u64, 0, 0, 0, true, b"x")
                    .expect("each stream stays inside its own window");
            }
        }

        let refused = leaf.build_packets(base + streams as u64, 0, 0, 0, true, b"x");
        assert!(
            matches!(
                refused,
                Err(LeafError::ReliableWindowFull {
                    needed: 1,
                    remaining: 0,
                    ..
                })
            ),
            "a message the session cannot stamp must be refused, got {refused:?}"
        );

        // The first stream's first packet — the eviction's casualty —
        // still has the header its rebuild needs.
        let owned = Arc::new(RetransmitDescriptor {
            seq: 0,
            stream_id: base,
            events: vec![Bytes::from_static(b"x")],
            flags: PacketFlags::RELIABLE,
            fragment: None,
        });
        assert_eq!(
            leaf.rebuild(&[owned]).len(),
            1,
            "another stream's traffic silently made this packet unrebuildable"
        );
    }
}
