//! The dispatcher: what arrives, where it goes, and what is dropped.
//!
//! Plan §7's list, in one place — routing envelopes addressed to
//! itself (unwrapped), plain events, channel membership (`0x0A00`),
//! stream window (`0x0B00`), capability announcement (`0x0C00`), fold
//! (`0x1000`), the nRPC wire types, RTC signal (`0x0D02`), and
//! unknown subprotocols dropped with a counter.
//!
//! # Non-forwarding is a role, not a TTL
//!
//! [`unwrap_routing`] is where that role lives. A routing envelope
//! whose `dest_id` is not this leaf is **dropped**, and dropped with
//! a counter rather than a log — it is the expected disposition, not
//! an error. The leaf sets normal TTLs on what it originates (a
//! browser's packet may legitimately cross several native hops) but
//! it never decrements one on someone else's behalf, never re-floods
//! an announcement, and never originates a pingwave. No node ever
//! learns "X via leaf".
//!
//! # Why this module is pure
//!
//! Everything here is a function from bytes to a classification, so
//! the routing table is a native unit test rather than a browser
//! observation. The parts that need a session, a transport or a
//! callback live in [`crate::node`].

use bytes::Bytes;
use net_wire::channel::membership::{self, MembershipMsg, SUBPROTOCOL_CHANNEL_MEMBERSHIP};
use net_wire::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};
use net_wire::stream_window::{
    StreamAckRanges, StreamNack, StreamReset, StreamWindow, SUBPROTOCOL_STREAM_ACK,
    SUBPROTOCOL_STREAM_NACK, SUBPROTOCOL_STREAM_RESET, SUBPROTOCOL_STREAM_WINDOW,
};

use crate::announce::{SUBPROTOCOL_CAPABILITY_ANN, SUBPROTOCOL_FOLD};
use crate::control_plane::{NodeId, SignalEnvelope};
use crate::counters::{DropReason, LeafCounters};
use crate::error::{LeafError, Result};
use crate::signal::{self, SUBPROTOCOL_RTC_SIGNAL};

/// The event plane: `subprotocol_id == 0`. nRPC and every
/// application frame ride here.
pub const SUBPROTOCOL_EVENT_PLANE: u16 = 0;

/// A `subprotocol_id` this leaf has an arm for.
///
/// The enum *is* the routing table: a value that does not map to a
/// variant is an unknown subprotocol, and there is no default arm
/// that could silently swallow one.
///
/// **`0x0D03` is deliberately absent.**
/// [`crate::establish::SUBPROTOCOL_ESTABLISHMENT_PROOF`] is consumed
/// by the responder's provisional-admission path
/// (`node::LeafNode::admit_establishment_proof`) *before* a session
/// exists, and that is the only context in which it means anything:
/// it authorises one exact establishment to be installed. Arriving
/// on an already-installed session it is a replay of a statement
/// that has already done its work, so "unknown subprotocol, dropped
/// and counted" is the correct and honest disposition rather than a
/// decode arm that would have to refuse it anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subprotocol {
    /// Plain events and nRPC frames.
    EventPlane,
    /// `0x0A00` — channel membership.
    ChannelMembership,
    /// `0x0B00` — receiver → sender credit grant.
    StreamWindow,
    /// `0x0B01` — receiver → sender retransmit request.
    StreamNack,
    /// `0x0B02` — sender → receiver stream reset.
    StreamReset,
    /// `0x0B03` — receiver → sender SACK ranges.
    StreamAck,
    /// `0x0C00` — a signed capability announcement.
    CapabilityAnnouncement,
    /// `0x0D02` — an RTC signalling envelope.
    RtcSignal,
    /// `0x1000` — a fold frame, which is how an announcement
    /// reaches a peer over a session.
    Fold,
}

impl Subprotocol {
    /// Classify a wire `subprotocol_id`.
    pub const fn from_wire(id: u16) -> Option<Self> {
        match id {
            SUBPROTOCOL_EVENT_PLANE => Some(Self::EventPlane),
            SUBPROTOCOL_CHANNEL_MEMBERSHIP => Some(Self::ChannelMembership),
            SUBPROTOCOL_STREAM_WINDOW => Some(Self::StreamWindow),
            SUBPROTOCOL_STREAM_NACK => Some(Self::StreamNack),
            SUBPROTOCOL_STREAM_RESET => Some(Self::StreamReset),
            SUBPROTOCOL_STREAM_ACK => Some(Self::StreamAck),
            SUBPROTOCOL_CAPABILITY_ANN => Some(Self::CapabilityAnnouncement),
            SUBPROTOCOL_RTC_SIGNAL => Some(Self::RtcSignal),
            SUBPROTOCOL_FOLD => Some(Self::Fold),
            _ => None,
        }
    }

    /// The wire id.
    pub const fn to_wire(self) -> u16 {
        match self {
            Self::EventPlane => SUBPROTOCOL_EVENT_PLANE,
            Self::ChannelMembership => SUBPROTOCOL_CHANNEL_MEMBERSHIP,
            Self::StreamWindow => SUBPROTOCOL_STREAM_WINDOW,
            Self::StreamNack => SUBPROTOCOL_STREAM_NACK,
            Self::StreamReset => SUBPROTOCOL_STREAM_RESET,
            Self::StreamAck => SUBPROTOCOL_STREAM_ACK,
            Self::CapabilityAnnouncement => SUBPROTOCOL_CAPABILITY_ANN,
            Self::RtcSignal => SUBPROTOCOL_RTC_SIGNAL,
            Self::Fold => SUBPROTOCOL_FOLD,
        }
    }
}

/// One decoded inbound event, ready for the node to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// An application or nRPC frame from the event plane.
    Event(Bytes),
    /// A membership message — a peer's Subscribe/Unsubscribe, or the
    /// Ack for one this leaf sent.
    Membership(MembershipMsg),
    /// A credit grant for one of this leaf's outbound streams.
    StreamWindow(StreamWindow),
    /// A retransmit request.
    StreamNack(StreamNack),
    /// A stream the sender gave up on.
    StreamReset(StreamReset),
    /// SACK ranges.
    StreamAck(StreamAckRanges),
    /// A signed announcement, still unverified — verification is the
    /// node's step, because it needs the store.
    Announcement(Bytes),
    /// A fold frame, which carries an announcement in practice.
    Fold(Bytes),
    /// A structurally valid signalling envelope, not yet verified.
    Signal(SignalEnvelope),
}

/// Which plane owns an event-plane carrier — the decision
/// [`crate::node::LeafNode::handle_event_plane`] makes before it
/// touches a payload.
///
/// One event-plane subprotocol carries three kinds of traffic:
/// application bytes, the nRPC **caller** plane (the reply carriers
/// this leaf subscribed to in order to be answered) and the nRPC
/// **served** plane (the `<service>.requests` carriers this leaf
/// serves). Ownership is settled at subscription/registration time —
/// exactly one owner per carrier, the [`crate::node::LeafNode::open_stream`]
/// / `ensure_reply_subscription` reservation discipline — so the
/// payload never decides which plane it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// Opaque application bytes, delivered byte-exact.
    Application,
    /// nRPC replies for this leaf's own calls.
    CallerRpc,
    /// nRPC requests, chunks, grants and cancels for the services this
    /// leaf serves.
    ServedRpc,
}

/// Route an event-plane carrier by ownership, never by payload.
///
/// Both planes claiming one carrier is a registration bug (the
/// reservation discipline refuses it at registration); should it ever
/// happen, the served plane wins so admitted calls can still reach
/// their one terminal.
pub fn plane_for(caller_owned: bool, served_owned: bool) -> Plane {
    match (caller_owned, served_owned) {
        (false, false) => Plane::Application,
        (true, false) => Plane::CallerRpc,
        (_, true) => Plane::ServedRpc,
    }
}

/// Strip a routing envelope, or decide the packet is not ours.
///
/// Returns the inner Net packet. `None` means the packet was dropped
/// and a counter moved — the two cases being an envelope for someone
/// else (the leaf does not forward) and one that arrived expired.
///
/// Discrimination is on the first two bytes: `ROUTING_MAGIC`
/// (`0x5452`, "RT") versus the Net packet's own `0x4E45`. That is
/// unambiguous by construction, which is why the 18-byte header
/// carries a magic at all.
pub fn unwrap_routing(bytes: Bytes, self_node: NodeId, counters: &LeafCounters) -> Option<Bytes> {
    if bytes.len() < 2 {
        counters.drop_for(DropReason::Unparsable);
        return None;
    }
    let magic = u16::from_le_bytes([bytes[0], bytes[1]]);
    if magic != ROUTING_MAGIC {
        // A direct packet. Left to the session to parse.
        return Some(bytes);
    }
    let Some(header) = RoutingHeader::from_bytes(&bytes) else {
        counters.drop_for(DropReason::Unparsable);
        return None;
    };
    if header.dest_id != self_node {
        // **The role.** Not an error, not a log line, not a forward.
        counters.drop_for(DropReason::NotAddressedToUs);
        return None;
    }
    if header.is_expired() {
        counters.drop_for(DropReason::RoutingExpired);
        return None;
    }
    if bytes.len() <= ROUTING_HEADER_SIZE {
        counters.drop_for(DropReason::Unparsable);
        return None;
    }
    Some(bytes.slice(ROUTING_HEADER_SIZE..))
}

/// Decode one event from a classified subprotocol.
///
/// `Ok(None)` never happens here: an unknown subprotocol is rejected
/// by [`Subprotocol::from_wire`] before this is reached, which is the
/// point of splitting the two.
pub fn decode(subprotocol: Subprotocol, payload: Bytes) -> Result<Decoded> {
    match subprotocol {
        Subprotocol::EventPlane => Ok(Decoded::Event(payload)),
        Subprotocol::ChannelMembership => membership::decode(&payload)
            .map(Decoded::Membership)
            .map_err(|e| LeafError::Wire(format!("membership: {e}"))),
        Subprotocol::StreamWindow => StreamWindow::decode(&payload)
            .map(Decoded::StreamWindow)
            .map_err(|e| LeafError::Wire(format!("stream window: {e}"))),
        Subprotocol::StreamNack => StreamNack::decode(&payload)
            .map(Decoded::StreamNack)
            .map_err(|e| LeafError::Wire(format!("stream nack: {e}"))),
        Subprotocol::StreamReset => StreamReset::decode(&payload)
            .map(Decoded::StreamReset)
            .map_err(|e| LeafError::Wire(format!("stream reset: {e}"))),
        Subprotocol::StreamAck => StreamAckRanges::decode(&payload)
            .map(Decoded::StreamAck)
            .map_err(|e| LeafError::Wire(format!("stream ack: {e}"))),
        Subprotocol::CapabilityAnnouncement => Ok(Decoded::Announcement(payload)),
        Subprotocol::Fold => Ok(Decoded::Fold(payload)),
        Subprotocol::RtcSignal => signal::decode(&payload).map(Decoded::Signal),
    }
}

/// Classify and decode in one step, counting what it refuses.
///
/// This is the arm the node calls per event. A subprotocol with no
/// arm and a payload that does not decode are both `None` **with a
/// counter** — the S0c lesson applied one layer up: a leaf that
/// dropped either silently would look dead to the peer that sent it.
pub fn dispatch_event(
    subprotocol_id: u16,
    payload: Bytes,
    counters: &LeafCounters,
) -> Option<Decoded> {
    let Some(subprotocol) = Subprotocol::from_wire(subprotocol_id) else {
        counters.drop_for(DropReason::UnknownSubprotocol);
        return None;
    };
    match decode(subprotocol, payload) {
        Ok(decoded) => Some(decoded),
        Err(_) => {
            counters.drop_for(DropReason::Unparsable);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three-plane routing table: a carrier has exactly one owner,
    /// settled before the payload is read.
    #[test]
    fn event_plane_carriers_route_by_ownership_not_payload() {
        assert_eq!(plane_for(false, false), Plane::Application);
        assert_eq!(plane_for(true, false), Plane::CallerRpc);
        assert_eq!(plane_for(false, true), Plane::ServedRpc);
        // Both claiming one carrier is a registration bug; the served
        // plane wins so admitted calls still reach their one terminal.
        assert_eq!(plane_for(true, true), Plane::ServedRpc);
    }

    use crate::identity::{EntityKeypair, LeafIdentity};
    use net_wire::channel::name::ChannelName;
    use net_wire::route_codec::_MAX_TTL;

    const SELF_NODE: NodeId = 0x1111_2222_3333_4444;
    const OTHER_NODE: NodeId = 0xAAAA_BBBB_CCCC_DDDD;

    fn routed(dest: NodeId, ttl: u8, inner: &[u8]) -> Bytes {
        let header = RoutingHeader::new(dest, SELF_NODE as u32, ttl);
        let mut out = Vec::with_capacity(ROUTING_HEADER_SIZE + inner.len());
        out.extend_from_slice(&header.to_bytes());
        out.extend_from_slice(inner);
        Bytes::from(out)
    }

    /// The routing table, as a table. Every id §7 lists must land on
    /// its arm, and anything else must be an unknown subprotocol.
    #[test]
    fn every_listed_subprotocol_maps_to_its_arm_and_nothing_else_does() {
        for (id, expect) in [
            (0x0000u16, Subprotocol::EventPlane),
            (0x0A00, Subprotocol::ChannelMembership),
            (0x0B00, Subprotocol::StreamWindow),
            (0x0B01, Subprotocol::StreamNack),
            (0x0B02, Subprotocol::StreamReset),
            (0x0B03, Subprotocol::StreamAck),
            (0x0C00, Subprotocol::CapabilityAnnouncement),
            (0x0D02, Subprotocol::RtcSignal),
            (0x1000, Subprotocol::Fold),
        ] {
            assert_eq!(
                Subprotocol::from_wire(id),
                Some(expect),
                "{id:#06x} must dispatch to {expect:?}"
            );
            assert_eq!(expect.to_wire(), id, "the mapping must be a bijection");
        }

        // Ids that exist in the mesh but have no leaf arm: the
        // negotiation plane, migration, sensing, RedEX, MeshDB, and
        // the scoped-announcement id whose audience key a leaf does
        // not hold.
        for id in [
            0x0500u16, 0x0600, 0x0C01, 0x0C02, 0x0C03, 0x0C04, 0x0D00, 0x0D01, 0x0E00, 0x0F00,
            0xFFFF,
        ] {
            assert_eq!(
                Subprotocol::from_wire(id),
                None,
                "{id:#06x} must NOT have a leaf arm"
            );
        }
    }

    #[test]
    fn an_unknown_subprotocol_is_dropped_and_counted() {
        let c = LeafCounters::new();
        assert!(dispatch_event(0x0F00, Bytes::from_static(b"whatever"), &c).is_none());
        assert_eq!(c.drops(DropReason::UnknownSubprotocol), 1);
        assert_eq!(
            c.total_drops(),
            1,
            "exactly one counter moves, so a diagnosis is unambiguous"
        );
    }

    #[test]
    fn a_payload_that_does_not_decode_is_dropped_and_counted_separately() {
        let c = LeafCounters::new();
        // 0x0B00 has a fixed 24-byte wire form; 5 bytes cannot be one.
        assert!(dispatch_event(0x0B00, Bytes::from_static(b"short"), &c).is_none());
        assert_eq!(c.drops(DropReason::Unparsable), 1);
        assert_eq!(c.drops(DropReason::UnknownSubprotocol), 0);
    }

    /// The non-forwarding role.
    #[test]
    fn a_routing_envelope_for_another_node_is_dropped_and_counted() {
        let c = LeafCounters::new();
        let packet = routed(OTHER_NODE, _MAX_TTL, b"inner packet bytes");
        assert!(
            unwrap_routing(packet, SELF_NODE, &c).is_none(),
            "a leaf never forwards"
        );
        assert_eq!(c.drops(DropReason::NotAddressedToUs), 1);
    }

    #[test]
    fn a_routing_envelope_for_this_leaf_is_unwrapped() {
        let c = LeafCounters::new();
        let packet = routed(SELF_NODE, _MAX_TTL, b"inner packet bytes");
        let inner = unwrap_routing(packet, SELF_NODE, &c).expect("addressed to us");
        assert_eq!(&inner[..], b"inner packet bytes");
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn an_expired_envelope_is_dropped_under_its_own_reason() {
        let c = LeafCounters::new();
        let packet = routed(SELF_NODE, 0, b"inner");
        assert!(unwrap_routing(packet, SELF_NODE, &c).is_none());
        assert_eq!(c.drops(DropReason::RoutingExpired), 1);
        assert_eq!(
            c.drops(DropReason::NotAddressedToUs),
            0,
            "expiry and misaddressing must be distinguishable"
        );
    }

    #[test]
    fn a_direct_packet_passes_the_routing_discriminator_untouched() {
        let c = LeafCounters::new();
        // A Net packet's own magic is 0x4E45.
        let direct = Bytes::from_static(&[0x45, 0x4E, 0x01, 0x00, 0x00]);
        let out = unwrap_routing(direct.clone(), SELF_NODE, &c).expect("passes through");
        assert_eq!(out, direct);
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn a_truncated_envelope_is_dropped_as_unparsable() {
        let c = LeafCounters::new();
        assert!(unwrap_routing(Bytes::from_static(&[0x52]), SELF_NODE, &c).is_none());
        // Magic present, header truncated.
        assert!(unwrap_routing(Bytes::from_static(&[0x52, 0x54, 0x00]), SELF_NODE, &c).is_none());
        // Header complete, nothing inside it.
        let header_only = routed(SELF_NODE, _MAX_TTL, b"");
        assert!(unwrap_routing(header_only, SELF_NODE, &c).is_none());
        assert_eq!(c.drops(DropReason::Unparsable), 3);
    }

    /// Each control arm must decode the production encoder's bytes —
    /// which is exactly what the move into `net-mesh-wire` bought.
    #[test]
    fn the_control_arms_decode_what_the_production_encoders_produce() {
        let c = LeafCounters::new();

        let subscribe = membership::encode(&MembershipMsg::Subscribe {
            channel: ChannelName::new("sensors/lidar").expect("valid name"),
            nonce: 0x99,
            token: None,
            queue_group: None,
        });
        match dispatch_event(0x0A00, Bytes::from(subscribe), &c) {
            Some(Decoded::Membership(MembershipMsg::Subscribe { channel, nonce, .. })) => {
                assert_eq!(channel.as_str(), "sensors/lidar");
                assert_eq!(nonce, 0x99);
            }
            other => panic!("expected a Subscribe, got {other:?}"),
        }

        let grant = StreamWindow {
            stream_id: 0x0102_0304,
            total_consumed: 4096,
            ack_seq: 12,
        };
        assert_eq!(
            dispatch_event(0x0B00, Bytes::copy_from_slice(&grant.encode()), &c),
            Some(Decoded::StreamWindow(grant))
        );

        let nack = StreamNack {
            stream_id: 7,
            next_expected: 3,
            missing_bitmap: 0b101,
        };
        assert_eq!(
            dispatch_event(0x0B01, Bytes::copy_from_slice(&nack.encode()), &c),
            Some(Decoded::StreamNack(nack))
        );

        let reset = StreamReset { stream_id: 9 };
        assert_eq!(
            dispatch_event(0x0B02, Bytes::copy_from_slice(&reset.encode()), &c),
            Some(Decoded::StreamReset(reset))
        );

        let ack = StreamAckRanges {
            stream_id: 11,
            ack_seq: 5,
            ranges: vec![(7, 9)],
        };
        assert_eq!(
            dispatch_event(0x0B03, Bytes::from(ack.encode()), &c),
            Some(Decoded::StreamAck(ack))
        );

        assert_eq!(c.total_drops(), 0, "nothing legitimate may be dropped");
    }

    #[test]
    fn the_signal_arm_decodes_a_real_envelope() {
        let c = LeafCounters::new();
        let id = LeafIdentity::from_secrets(EntityKeypair::from_secret([0x51; 32]), [0x52; 32]);
        let envelope = signal::sign(
            &id,
            SELF_NODE,
            0x77,
            crate::control_plane::SignalKind::Answer,
            b"v=0".to_vec(),
            2_000,
        );
        let bytes = signal::encode(&envelope).expect("encode");
        assert_eq!(
            dispatch_event(0x0D02, Bytes::from(bytes), &c),
            Some(Decoded::Signal(envelope))
        );
        assert_eq!(c.total_drops(), 0);
    }

    #[test]
    fn announcements_and_fold_frames_reach_the_node_unverified() {
        let c = LeafCounters::new();
        let bytes = Bytes::from_static(b"{\"node_id\":1}");
        assert_eq!(
            dispatch_event(0x0C00, bytes.clone(), &c),
            Some(Decoded::Announcement(bytes.clone())),
            "the dispatcher must not verify — the store does"
        );
        assert_eq!(
            dispatch_event(0x1000, bytes.clone(), &c),
            Some(Decoded::Fold(bytes))
        );
    }
}
