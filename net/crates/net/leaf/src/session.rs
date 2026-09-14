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

use std::cell::Cell;
use std::collections::HashMap;

use bytes::Bytes;
use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
use net_wire::parsed_packet::ParsedPacket;
use net_wire::peer_addr::{PeerAddr, RtcPeerId};
use net_wire::pool::PacketBuilder;
use net_wire::protocol::{EventFrame, PacketFlags};
use net_wire::session::NetSession;

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
/// Consumed by [`Self::read_msg2`]: a handshake cannot be read twice,
/// and the type system says so rather than a runtime flag.
///
/// No `Debug`: `NoiseHandshake` has none, and a derived one would be
/// a formatter over live handshake state.
pub struct PendingHandshake {
    handshake: NoiseHandshake,
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
                handshake,
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

    /// Consume the peer's Noise message 2 — delivered as a Net
    /// handshake packet — and install the session.
    pub fn read_msg2(mut self, raw: &[u8]) -> Result<LeafSession> {
        let parsed = ParsedPacket::parse(Bytes::copy_from_slice(raw), self.addr)
            .ok_or_else(|| LeafError::Wire("message 2 did not parse as a packet".into()))?;
        if !parsed.header.flags.is_handshake() {
            return Err(LeafError::Wire(
                "expected a handshake packet for message 2".into(),
            ));
        }
        self.handshake
            .read_message(&parsed.payload)
            .map_err(|e| LeafError::Session(format!("noise message 2: {e}")))?;
        if !self.handshake.is_finished() {
            return Err(LeafError::Session(
                "handshake did not complete after message 2".into(),
            ));
        }
        let keys = self
            .handshake
            .into_session_keys()
            .map_err(|e| LeafError::Session(format!("session keys: {e}")))?;
        Ok(LeafSession {
            peer: self.peer,
            session: NetSession::new(keys, self.addr, POOL_SIZE, false),
            next_fragment_id: Cell::new(1),
        })
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
        let keys = handshake
            .into_session_keys()
            .map_err(|e| LeafError::Session(format!("session keys: {e}")))?;
        Ok((
            LeafSession {
                peer: initiator,
                session: NetSession::new(keys, addr, POOL_SIZE, false),
                next_fragment_id: Cell::new(1),
            },
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
    /// Fragment group id.
    pub fragment_id: u16,
    /// Byte offset of this piece within its group.
    pub fragment_offset: u16,
    /// `frag_flags`, interpreted by [`crate::frame`].
    pub frag_flags: u8,
    /// The event frames the packet carried.
    pub events: Vec<Bytes>,
}

/// One established session with one peer.
#[derive(Debug)]
pub struct LeafSession {
    peer: NodeId,
    session: NetSession,
    next_fragment_id: Cell<u16>,
}

impl LeafSession {
    /// The peer's node id.
    #[inline]
    pub fn peer(&self) -> NodeId {
        self.peer
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
        let flags = if reliable {
            PacketFlags::RELIABLE
        } else {
            PacketFlags::NONE
        };
        self.session.open_stream_with(stream_id, reliable, 1);

        let mut out = Vec::with_capacity(fragments.len());
        for fragment in fragments {
            let seq = self.session.get_or_create_stream(stream_id).next_tx_seq();
            let events = [fragment.data];
            let mut builder = self.session.thread_local_pool().get();
            builder.set_channel_hash(channel_hash);
            builder.set_origin_hash(origin_hash);
            builder.set_fragment(fragment_id, fragment.offset, fragment.flags);
            out.push(builder.build_subprotocol(stream_id, seq, &events, flags, subprotocol_id));
        }
        Ok(out)
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
            return Err(LeafError::Wire(
                "the anti-replay window refused the AEAD counter".into(),
            ));
        }
        Ok(OpenedPacket {
            subprotocol_id: parsed.header.subprotocol_id,
            channel_hash: parsed.header.channel_hash,
            origin_hash: parsed.header.origin_hash,
            stream_id: parsed.header.stream_id,
            sequence: parsed.header.sequence,
            reliable: parsed.header.flags.is_reliable(),
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
}
