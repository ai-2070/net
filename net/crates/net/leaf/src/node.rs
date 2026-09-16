//! The leaf node: everything above the transport, and nothing that
//! touches `web_sys`.
//!
//! This is a **sans-IO** core. It takes bytes in
//! ([`LeafNode::on_datagram`]), hands bytes out
//! ([`LeafNode::take_outbound`]), and emits events
//! ([`LeafNode::drain_events`]). The RTC driver and the control plane
//! pump it; nothing in here knows what a `RtcDataChannel` is.
//!
//! That shape is not aesthetic. It is what makes the leaf's protocol
//! behaviour — dispatch, reassembly, reorder, call disposition,
//! announcement verification — testable natively, including a full
//! handshake and round trip against a real responder session. A
//! browser is then needed only to prove the transport, which is what
//! the Playwright matrix and the wasm runner are for.

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use net_wire::clock::Instant;

use crate::announce::{self, AnnouncementStore, VerifiedAnnouncement, SUBPROTOCOL_CAPABILITY_ANN};
use crate::channel::{reply_channel, request_channel, Channel, SUBPROTOCOL_MEMBERSHIP};
use crate::clock;
use crate::control_plane::{NodeId, SignalEnvelope};
use crate::counters::{DropReason, LeafCounters};
use crate::dispatch::{self, Decoded, SUBPROTOCOL_EVENT_PLANE};
use crate::error::{LeafError, Result, RpcError};
use crate::frame::{PieceMeta, Reassembler};
use crate::identity::{unhex, LeafIdentity};
use crate::rpc::{CallOwner, CallResult, CallTable, DEFAULT_CALL_TIMEOUT_MS};
use crate::rpc_wire::{self, RpcRequestPayload};
use crate::session::{event_frame_bytes, rtc_addr, PendingHandshake, SessionTable};
use crate::signal::{self, SeenSignals};
use crate::stream::{stream_id_from_label, Reliability, RxStream, StreamRecord};
use net_wire::route_codec::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};
use net_wire::stream_window::{
    StreamReset, StreamWindow, SUBPROTOCOL_STREAM_NACK, SUBPROTOCOL_STREAM_RESET,
    SUBPROTOCOL_STREAM_WINDOW,
};

/// One thing that happened, as the SDK sees it.
///
/// Every `u64` crosses the wasm boundary as a decimal **string**:
/// `channel_hash`, `origin_hash`, `stream_id`, `node_id` and
/// `call_id` all exceed 2^53, and `JSON.parse` would round them — a
/// page filtering `channel_message` by hash would silently match the
/// wrong channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafEvent {
    /// A session with `peer` is established.
    Connected {
        /// This leaf's node id.
        node_id: NodeId,
        /// The peer's node id.
        peer_node: NodeId,
        /// The peer's public RTC socket, when the bootstrap
        /// published one. The datum the `UdpBlocked` correction
        /// needs: the STUN probe aims here, never at an ICE server.
        rtc_addr: Option<String>,
    },
    /// A session with `peer` is gone.
    Disconnected {
        /// The peer.
        peer_node: NodeId,
        /// Why.
        reason: String,
    },
    /// An event-plane frame that is not an nRPC reply.
    ChannelMessage {
        /// Wire `u16` channel hint.
        channel_hash: u16,
        /// The publisher's origin hash.
        origin_hash: NodeId,
        /// The payload.
        payload: Bytes,
    },
    /// Data on an application stream, already reordered.
    StreamData {
        /// The stream.
        stream_id: u64,
        /// The sequence this batch arrived under.
        seq: u64,
        /// The payload.
        payload: Bytes,
    },
    /// A reliable stream ended without delivering everything, and
    /// nothing will recover it.
    ///
    /// The alternative — letting a hole be abandoned and a counter
    /// move — turns a reliability failure into silence the consumer
    /// cannot distinguish from a quiet peer. This says it.
    StreamFailed {
        /// The peer whose stream failed.
        peer_node: NodeId,
        /// The stream.
        stream_id: u64,
        /// Which end gave up.
        reason: StreamFailure,
    },
    /// A verified announcement was ingested.
    Announcement(VerifiedAnnouncement),
    /// A verified signalling envelope arrived for this leaf.
    Signal(SignalEnvelope),
    /// Something was refused. Carries the reason's stable name.
    Dropped {
        /// The reason.
        reason: DropReason,
    },
}

/// Which end of a reliable stream gave up, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFailure {
    /// This leaf resent a packet `max_retries` times and the peer
    /// never acknowledged it.
    RetransmitsExhausted,
    /// The peer sent a `StreamReset`: its own retransmits are
    /// exhausted, so the rest of the stream is never coming.
    PeerReset,
    /// This leaf held [`MAX_REORDER_HELD`](crate::stream::MAX_REORDER_HELD)
    /// records on a reliable stream and the head gap was still
    /// open.
    ///
    /// The bound is memory, and memory is finite; what is *not*
    /// forced is releasing the records behind the hole and moving a
    /// counter, which is silent loss on the one stream mode whose
    /// contract is that nothing is lost. So the receive half ends
    /// here, typed, naming the stream.
    ///
    /// **No RESET is sent for this.** A `StreamReset` means "my
    /// send half gave up", and its receiver answers by resetting
    /// its own receive cursor to zero so the sender can restart the
    /// id — send it for a receive-side failure and the peer would
    /// re-accept sequences it had already delivered on a stream
    /// this leaf is still sending on. The sender learns instead
    /// from the fact it is holding: the head gap it never got an
    /// ack for is exactly the packet its own retransmit loop
    /// exhausts and RESETs.
    ReorderOverflow,
    /// A fragment group this leaf had already acknowledged was
    /// given up on before it completed — reaped at the reassembly
    /// TTL, or destroyed because a later piece contradicted it.
    ///
    /// The acknowledgement is what makes this terminal rather than
    /// a counter. Once the receive half acknowledges a fragment's
    /// sequence, the sender retires its only copy: there is nothing
    /// left to rebuild the group from, and no wire gap left to NACK
    /// either, because the sequences were consumed. The bytes are
    /// gone, and the consumer that was promised them has to be told
    /// which stream lost them — the alternative, which is what this
    /// replaced, is a stream that simply stops delivering with
    /// `events == []`.
    ///
    /// **No RESET is sent for this**, for the same reason
    /// [`Self::ReorderOverflow`] sends none: it is a receive-side
    /// verdict, and a `StreamReset` announces a *send* half giving
    /// up. The peer's own send half is unaffected and this leaf may
    /// still be sending on the id.
    ReassemblyAbandoned,
}

impl StreamFailure {
    /// The stable string the JSON event carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetransmitsExhausted => "retransmits_exhausted",
            Self::PeerReset => "peer_reset",
            Self::ReorderOverflow => "reorder_overflow",
            Self::ReassemblyAbandoned => "reassembly_abandoned",
        }
    }
}

impl LeafEvent {
    /// The JSON one `on_event` callback receives.
    pub fn to_json(&self) -> String {
        match self {
            Self::Connected {
                node_id,
                peer_node,
                rtc_addr,
            } => format!(
                "{{\"type\":\"connected\",\"node_id\":\"{node_id}\",\"node_id_hex\":\"{node_id:016x}\",\"peer_node\":\"{peer_node}\",\"rtc_addr\":{}}}",
                json_string_or_null(rtc_addr.as_deref())
            ),
            Self::Disconnected { peer_node, reason } => format!(
                "{{\"type\":\"disconnected\",\"peer_node\":\"{peer_node}\",\"reason\":{}}}",
                json_string(reason)
            ),
            Self::ChannelMessage {
                channel_hash,
                origin_hash,
                payload,
            } => format!(
                "{{\"type\":\"channel_message\",\"channel_hash\":\"{channel_hash}\",\"origin_hash\":\"{origin_hash}\",\"payload\":\"{}\"}}",
                base64(payload)
            ),
            Self::StreamData {
                stream_id,
                seq,
                payload,
            } => format!(
                "{{\"type\":\"stream_data\",\"stream_id\":\"{stream_id}\",\"seq\":\"{seq}\",\"payload\":\"{}\"}}",
                base64(payload)
            ),
            Self::StreamFailed {
                peer_node,
                stream_id,
                reason,
            } => format!(
                "{{\"type\":\"stream_failed\",\"peer_node\":\"{peer_node}\",\"stream_id\":\"{stream_id}\",\"reason\":\"{}\"}}",
                reason.as_str()
            ),
            Self::Announcement(a) => {
                format!("{{\"type\":\"announcement\",\"announcement\":{}}}", a.to_json())
            }
            Self::Signal(envelope) => format!(
                "{{\"type\":\"signal\",\"from\":\"{}\",\"to\":\"{}\",\"dialog\":\"{}\",\"kind\":{},\"payload\":\"{}\",\"not_after\":\"{}\"}}",
                envelope.from,
                envelope.to,
                envelope.dialog,
                envelope.kind.tag(),
                base64(&envelope.payload),
                envelope.not_after
            ),
            Self::Dropped { reason } => format!(
                "{{\"type\":\"dropped\",\"reason\":\"{}\"}}",
                reason.as_str()
            ),
        }
    }
}

/// One outbound packet and who it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    /// The peer whose DataChannel carries it.
    pub peer: NodeId,
    /// The packet.
    pub packet: Bytes,
}

/// The TTL a relayed packet leaves with.
///
/// Eight, mirroring the native routed `0x0D02` send. See
/// [`LeafNode::route_outbound`].
const RELAY_TTL: u8 = 8;

/// What the transport delivered, once its routing envelope — if it
/// had one — has been stripped and its sender resolved.
///
/// Produced by [`LeafNode::classify_datagram`], which documents what
/// `from` is derived from and, for a relayed packet, what it is not
/// authority for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// A Noise handshake packet. The driver runs
    /// [`LeafNode::complete_handshake`] or
    /// [`LeafNode::accept_handshake`] according to
    /// [`LeafNode::is_handshaking`].
    Handshake {
        /// Who it is from.
        from: NodeId,
        /// The packet, routing envelope removed.
        packet: Bytes,
        /// Whether it arrived through a relay. The answer decides
        /// which transport message 2 goes back on, and it is the
        /// only thing that distinguishes a relayed session being
        /// established from a direct one **replacing** it (§9 step
        /// 4).
        relayed: bool,
    },
    /// Anything else: hand `packet` to
    /// [`LeafNode::on_datagram`] with `from` as the peer.
    Session {
        /// Who it is from.
        from: NodeId,
        /// The packet, routing envelope removed.
        packet: Bytes,
        /// Whether it arrived through a relay.
        relayed: bool,
    },
    /// Refused; a counter moved. The driver does nothing.
    Refused,
}

/// A Net handshake packet: `0x4E45` magic and the HANDSHAKE flag.
///
/// The one place this is decided. The wasm driver and the
/// classifier above both read it, and two spellings of "is this a
/// handshake" is how a driver ends up feeding message 1 to the
/// session path.
pub fn is_handshake_packet(bytes: &[u8]) -> bool {
    bytes.len() >= net_wire::protocol::HEADER_SIZE
        && u16::from_le_bytes([bytes[0], bytes[1]]) == net_wire::protocol::MAGIC
        && net_wire::protocol::PacketFlags::from_bits(bytes[3]).is_handshake()
}

/// An open application stream's parameters.
///
/// **The handle carries the incarnation it was opened on**, and
/// [`LeafNode::stream_send`] / [`LeafNode::close_stream`] refuse a
/// handle whose incarnation is not the peer's current session. A
/// replacement retires the predecessor's receive cursors, partial
/// reassemblies and pending calls, but it cannot reach into a
/// `StreamHandle` the caller already holds: a value is not a table
/// entry. Without the fence, a stale handle's `stream_send`
/// resolved the peer's *current* session and pushed application
/// bytes onto a stream the far end never opened under that
/// incarnation — the same alias the native R12 fix closed by
/// binding session incarnation into the native handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHandle {
    /// The peer.
    pub peer: NodeId,
    /// The incarnation of the session this stream was opened on.
    pub incarnation: u64,
    /// The wire stream id.
    pub stream_id: u64,
    /// The `u16` channel hint stamped on its packets.
    pub channel_hash: u16,
    /// Reliable or fire-and-forget.
    pub reliability: Reliability,
}

/// What a stream id is, in this leaf's own namespace.
///
/// Classification is a **registry** question, not a bit test. Both
/// id formulas `OR` a discriminator into a full 64-bit hash, so
/// roughly half of all channel hashes already carry the leaf bit —
/// reading that one bit made an ordinary subscribed channel emit
/// `StreamData`. What the leaf actually knows is which ids it
/// opened as streams and which it subscribed to or published on as
/// channels; that is what decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    /// An application stream this leaf opened.
    Stream,
    /// A channel this leaf subscribed to or publishes on.
    Channel,
}

/// The leaf.
pub struct LeafNode {
    identity: LeafIdentity,
    sessions: SessionTable,
    handshakes: HashMap<NodeId, PendingHandshake>,
    reassembler: Reassembler,
    /// Consumer-side reorder, keyed by **session incarnation** and
    /// stream id. Keying on the peer would hand a replacement its
    /// predecessor's cursor, and the successor's sequence zero would
    /// be refused as a duplicate.
    rx_streams: HashMap<(u64, u64), RxStream>,
    calls: CallTable,
    /// What this leaf has registered each `(peer, stream id)` as.
    stream_kinds: HashMap<(NodeId, u64), StreamKind>,
    /// `(incarnation, stream id)` → the receiver's cumulative
    /// consumed-byte count as of the last stream-window frame this
    /// leaf sent, so *credit* is granted on a volume threshold
    /// rather than per packet.
    grants_sent: HashMap<(u64, u64), u64>,
    /// `(incarnation, stream id)` → the cumulative ack the last
    /// stream-window frame carried. The peer's retransmit window
    /// prunes against this number, and it is not a volume — see
    /// [`LeafNode::maybe_grant`] for why it cannot share the
    /// credit cadence.
    acks_sent: HashMap<(u64, u64), u64>,
    announcements: AnnouncementStore,
    seen_signals: SeenSignals,
    counters: LeafCounters,
    outbound: VecDeque<Outbound>,
    events: Vec<LeafEvent>,
    /// Monotonic announcement version. Starts at 1: the fold rejects
    /// generation 0 outright.
    announcement_version: u64,
    /// The peer's published RTC socket, per peer, for the
    /// `UdpBlocked` evidence and the `connected` event.
    peer_rtc_addr: HashMap<NodeId, String>,
    /// `peer` → the peer that **relays** for it: every packet for
    /// `peer` leaves wrapped in a routing envelope and addressed to
    /// the relay's transport instead (plan §9 step 3).
    ///
    /// Empty for a leaf that talks only to its anchor, which is why
    /// [`Self::take_outbound`] costs nothing until something is in
    /// here. A direct DataChannel for `peer` is what removes the
    /// entry ([`Self::clear_peer_relay`]) — §9 step 4's replacement,
    /// from the addressing side.
    relays: HashMap<NodeId, NodeId>,
    /// Subscribe nonces, so an Ack can be correlated.
    next_nonce: u64,
    /// The delegation chain the anchor issued at enrollment. `None`
    /// means the session is still PROVISIONAL, and §12 refuses
    /// everything above the transport.
    delegation_chain: Option<Vec<u8>>,
    /// `(peer, reply-channel canonical hash)` this leaf has already
    /// subscribed to.
    ///
    /// **Why a set and not a subscribe per call.** An anchor
    /// publishes an nRPC RESPONSE on `<service>.replies.<origin>`
    /// and forwards it only to subscribers, so a call whose reply
    /// channel was never subscribed runs the handler and then
    /// strands the reply — the caller sees a bare deadline. But
    /// re-subscribing per call would put a `0x0A00` membership frame
    /// on the wire per call, which §2's admission would rightly
    /// start refusing under load. Once per (peer, service) is the
    /// only correct cadence; cleared for a peer when its session
    /// goes, because the anchor's roster entry goes with it.
    reply_subscriptions: std::collections::HashSet<(NodeId, u64)>,
    /// `(peer, carrier stream id)` the **nRPC plane owns** on the
    /// receive side.
    ///
    /// This is the plane-ownership registry the event-plane
    /// classifier reads, and it is the *only* thing that can make
    /// an inbound event-plane frame an nRPC reply — see
    /// [`LeafNode::handle_event_plane`]. One entry per reply channel
    /// this leaf's nRPC client subscribed to in order to be answered
    /// (`<service>.replies.<own origin>`), held as the stream id a
    /// publisher of that channel derives, which is exactly the
    /// carrier a pending call's `CallOwner` demands. Written only by
    /// [`LeafNode::ensure_reply_subscription`] — application
    /// `subscribe` / `publish` / `open_stream` never reach it — and
    /// cleared with `reply_subscriptions` when the session goes,
    /// because a carrier whose subscription the anchor forgot is not
    /// one the RPC plane still owns.
    rpc_reply_carriers: std::collections::HashSet<(NodeId, u64)>,
    /// `(incarnation, stream id)` whose **send** half this leaf gave
    /// up on, and will not send on again.
    ///
    /// **A `StreamFailed` event is a notification, not retirement.**
    /// When this leaf's retransmits are exhausted it sends a RESET
    /// and the wire drops the given-up packet — but `next_tx_seq`
    /// does not rewind, and the peer answers a RESET by resetting
    /// its *receive* half to sequence zero
    /// (`NetSession::reset_rx_stream`, the same thing this leaf
    /// does). So the next byte pushed onto that id would arrive as
    /// sequence *K* against a cursor expecting zero: a K-deep hole
    /// the peer NACKs for sequences that were never lost, behind
    /// which nothing is delivered. The disposition is therefore
    /// explicit rather than assumed: the id is terminal for this
    /// incarnation, handle-addressed send and close refuse typed,
    /// [`Self::open_stream`] refuses to hand out a fresh handle for
    /// it, and a new session is what makes the id usable again —
    /// which is what [`Self::retire_incarnation`] clears it for.
    send_failed: std::collections::HashSet<(u64, u64)>,
    /// `(incarnation, stream id)` whose **receive** half this leaf
    /// gave up on, and will not reassemble again.
    ///
    /// Set by a reliable reorder overflow
    /// ([`StreamFailure::ReorderOverflow`]) and by an acknowledged
    /// fragment group being abandoned
    /// ([`StreamFailure::ReassemblyAbandoned`]). Without it the
    /// consumer's failure would not be terminal: the very next
    /// arrival would build a fresh `RxStream` at sequence zero,
    /// hold the same 64 records against the same unfillable hole
    /// and fail again, once per bound, forever. Further arrivals
    /// are dropped and counted instead — which is also what stops
    /// a delayed tail being acknowledged into a group whose head
    /// was just reaped. A peer RESET **clears** it — that is the
    /// peer's send half restarting, which is exactly the case a
    /// new receive cursor is correct for.
    recv_failed: std::collections::HashSet<(u64, u64)>,
    /// `(incarnation, stream id)` whose consumer called
    /// [`Self::close_stream`] and has not reopened it.
    ///
    /// Close is a **local** operation: one DataChannel carries
    /// every stream, nothing goes on the wire, and the peer's
    /// transmit sequence for the id is not rewound by it. So the
    /// receive cursor cannot be deleted — deleting it made a
    /// reopen build a fresh cursor waiting for sequence zero while
    /// the peer went on sending sequence one, and the stream
    /// stalled with nothing lost and no event. The cursor lives
    /// for the stream's lifetime within the session and this set
    /// is what suppresses delivery in the meantime;
    /// [`Self::open_stream`] clears the id, and the reopened
    /// consumer resumes at the peer's next sequence.
    rx_closed: std::collections::HashSet<(u64, u64)>,
}

impl LeafNode {
    /// A node with this identity and no sessions.
    ///
    /// `call_id_seed` seeds the call table — see
    /// [`CallTable::with_seed`] for why it is not zero.
    pub fn new(identity: LeafIdentity, call_id_seed: u64) -> Self {
        Self {
            identity,
            sessions: SessionTable::new(),
            handshakes: HashMap::new(),
            reassembler: Reassembler::new(),
            rx_streams: HashMap::new(),
            calls: CallTable::with_seed(call_id_seed),
            stream_kinds: HashMap::new(),
            grants_sent: HashMap::new(),
            acks_sent: HashMap::new(),
            announcements: AnnouncementStore::new(),
            seen_signals: SeenSignals::new(),
            counters: LeafCounters::new(),
            outbound: VecDeque::new(),
            events: Vec::new(),
            announcement_version: 1,
            peer_rtc_addr: HashMap::new(),
            relays: HashMap::new(),
            next_nonce: 1,
            reply_subscriptions: std::collections::HashSet::new(),
            rpc_reply_carriers: std::collections::HashSet::new(),
            send_failed: std::collections::HashSet::new(),
            recv_failed: std::collections::HashSet::new(),
            rx_closed: std::collections::HashSet::new(),
            delegation_chain: None,
        }
    }

    /// This node's id.
    #[inline]
    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    /// This node's origin hash — the value every packet header it
    /// seals carries, and the one a receiver checks an event
    /// payload's `EventMeta.origin_hash` against.
    #[inline]
    pub fn origin_hash(&self) -> u64 {
        self.identity.origin_hash()
    }

    /// This node's identity.
    #[inline]
    pub fn identity(&self) -> &LeafIdentity {
        &self.identity
    }

    /// The counters.
    #[inline]
    pub fn counters(&self) -> &LeafCounters {
        &self.counters
    }

    /// Whether a session with `peer` is installed.
    pub fn has_session(&self, peer: NodeId) -> bool {
        self.sessions.get(peer).is_some()
    }

    /// Record what the bootstrap published for `peer`.
    pub fn set_peer_rtc_addr(&mut self, peer: NodeId, addr: Option<String>) {
        match addr {
            Some(addr) => {
                self.peer_rtc_addr.insert(peer, addr);
            }
            None => {
                self.peer_rtc_addr.remove(&peer);
            }
        }
    }

    // ──────────────── the relayed path (plan §9 step 3) ─────────────

    /// Send everything for `peer` through `relay`, wrapped in a
    /// routing envelope.
    ///
    /// This is plan §9 step 2–3 from the leaf's side: before ICE has
    /// solved anything, A reaches B by handing packets to a peer that
    /// forwards them **blind**. The relay learns `(src, dest, ttl)`
    /// and the inner packet's cleartext subprotocol id — the metadata
    /// it already sees for every packet it carries today — and can
    /// drop or delay, which is a liveness failure, not an
    /// authenticity one. It cannot read the payload: the inner packet
    /// is sealed to the A↔B session, and a `0x0D02` envelope riding
    /// inside it is additionally signed by the sender's entity key.
    ///
    /// Nothing about the session changes. The relay is an
    /// **addressing** fact, which is why it lives in its own map and
    /// not in the session table: `SessionTable` is keyed on identity
    /// exactly so that §9 step 4 can replace a relayed session with a
    /// direct one as a table update rather than a route install.
    pub fn set_peer_relay(&mut self, peer: NodeId, relay: NodeId) {
        self.relays.insert(peer, relay);
    }

    /// Stop relaying for `peer`: its packets go out on its own
    /// transport again. `true` if a relay was set.
    ///
    /// The addressing half of §9 step 4. Called once a direct
    /// DataChannel to `peer` is carrying its session; the session
    /// replacement itself is [`Self::install_session`]'s, and this
    /// does not touch it.
    pub fn clear_peer_relay(&mut self, peer: NodeId) -> bool {
        self.relays.remove(&peer).is_some()
    }

    /// The relay set for `peer`, if any.
    pub fn peer_relay(&self, peer: NodeId) -> Option<NodeId> {
        self.relays.get(&peer).copied()
    }

    /// Address one packet for the wire: which transport carries it,
    /// and what bytes go on it.
    ///
    /// With no relay for `peer` this is the identity function, which
    /// is the Stage 5 path unchanged. With one, the packet is wrapped
    /// in a `RoutingHeader` naming `peer` as the destination and this
    /// leaf's 32-bit projection as the source, and handed to the
    /// relay's transport.
    ///
    /// The TTL is 8, which is what the native side stamps on a routed
    /// `0x0D02` frame (`adapter/net/mesh.rs`'s `send_rtc_signal`:
    /// `RoutingHeader::new(peer_node_id, self.node_id as u32, 8)`).
    /// Mirrored rather than re-chosen — a leaf relaying with a
    /// different hop count than the mesh's own signalling would be a
    /// second policy for one number.
    pub fn route_outbound(&self, peer: NodeId, packet: Bytes) -> Outbound {
        let Some(relay) = self.peer_relay(peer) else {
            return Outbound { peer, packet };
        };
        // The 32-bit projection is the wire format, not a narrowing
        // this code chose: `RoutingHeader::src_id` is a `u32` and
        // `session::routing_id` already binds the same projection
        // into the handshake prologue.
        #[allow(clippy::cast_possible_truncation)]
        let header = RoutingHeader::new(peer, self.identity.node_id() as u32, RELAY_TTL);
        let mut wire = Vec::with_capacity(ROUTING_HEADER_SIZE + packet.len());
        wire.extend_from_slice(&header.to_bytes());
        wire.extend_from_slice(&packet);
        Outbound {
            peer: relay,
            packet: Bytes::from(wire),
        }
    }

    /// Whether an **initiator** handshake with `peer` is in flight.
    ///
    /// The driver's discriminator for an inbound handshake packet:
    /// `true` means it is message 2 for a handshake this leaf started
    /// and [`Self::complete_handshake`] owns it; `false` means it is
    /// somebody's message 1 and only [`Self::accept_handshake`] can.
    /// Guessing from "do I have a session" cannot tell them apart
    /// once a session exists, which is exactly the §9 step 4 case.
    pub fn is_handshaking(&self, peer: NodeId) -> bool {
        self.handshakes.contains_key(&peer)
    }

    /// Send one signed envelope to `peer` as a `0x0D02` frame on the
    /// session with `peer`.
    ///
    /// Plan §9 step 3, verbatim: the signalling rides the A↔B session
    /// and the relay forwards it blind. The stream id is the
    /// subprotocol id and the frame is fire-and-forget, which is the
    /// framing the native `send_rtc_signal` uses for the same message
    /// (`PacketFlags::NONE`, `stream_id = SUBPROTOCOL_RTC_SIGNAL as
    /// u64`) — so an anchor that counts per-pair *application* data
    /// excludes these by subprotocol id, and signalling can never be
    /// mistaken for payload in the §10 witness.
    ///
    /// Requires a session with `peer`; a peer with no session is
    /// [`crate::control_plane::ControlPlane::signal`]'s job, and that
    /// carrier is the serverless one.
    pub fn send_signal_frame(&mut self, peer: NodeId, envelope: &SignalEnvelope) -> Result<()> {
        let payload = signal::encode(envelope)?;
        self.send_subprotocol(
            peer,
            u64::from(signal::SUBPROTOCOL_RTC_SIGNAL),
            signal::SUBPROTOCOL_RTC_SIGNAL,
            0,
            &payload,
            false,
        )
    }

    /// Classify one datagram the transport delivered: who it is from,
    /// and whether it is a handshake.
    ///
    /// # Why the driver needs this
    ///
    /// A **relayed** packet arrives on the RELAY's DataChannel, so
    /// the transport-level peer is the relay and not the sender.
    /// [`Self::on_datagram`] is unchanged and still resolves an
    /// ordinary datagram's session by the peer the transport named;
    /// this function is what turns a relayed datagram into the
    /// `(sender, inner packet)` pair `on_datagram` already knows how
    /// to take. A datagram with no routing envelope comes back with
    /// its bytes and its peer untouched.
    ///
    /// # What `src_id` is, and what it is NOT
    ///
    /// The only hint about who sent a relayed packet is
    /// `RoutingHeader::src_id`: the low 32 bits of a node id, written
    /// by the sender and rewritable by any hop.
    ///
    /// It is used for exactly one thing — choosing **which key to
    /// try**. It is authority for nothing. A [`Inbound::Session`]
    /// packet still has to open under that session's AEAD, and an
    /// [`Inbound::Handshake`] still has to complete NKpsk0 against
    /// this leaf's own static key with the trust domain's PSK and the
    /// claimed id bound into the prologue. So a forged `src_id` can
    /// only make this leaf try the wrong key — which fails and is
    /// counted — and can never make a frame speak for a peer.
    ///
    /// Resolution is through the **announcement store**, so the
    /// sender must be a node whose signed announcement this leaf
    /// verified: §5 Layer 1's "key discovery precedes signalling",
    /// applied to the session seam as well as to the envelope. An
    /// unresolvable source is refused and counted, and a projection
    /// shared by two fresh announcements is refused rather than
    /// guessed.
    pub fn classify_datagram(&self, transport_peer: NodeId, bytes: Bytes) -> Inbound {
        if bytes.len() < 2 {
            self.counters.drop_for(DropReason::Unparsable);
            return Inbound::Refused;
        }
        if u16::from_le_bytes([bytes[0], bytes[1]]) != ROUTING_MAGIC {
            // Not relayed: the Stage 5 path, byte for byte.
            return Self::classified(transport_peer, bytes, false);
        }
        let Some(header) = RoutingHeader::from_bytes(&bytes) else {
            self.counters.drop_for(DropReason::Unparsable);
            return Inbound::Refused;
        };
        if header.dest_id != self.identity.node_id() {
            // The role: a leaf never forwards.
            self.counters.drop_for(DropReason::NotAddressedToUs);
            return Inbound::Refused;
        }
        if header.is_expired() {
            self.counters.drop_for(DropReason::RoutingExpired);
            return Inbound::Refused;
        }
        if bytes.len() <= ROUTING_HEADER_SIZE {
            self.counters.drop_for(DropReason::Unparsable);
            return Inbound::Refused;
        }
        let Some(from) = self.resolve_relayed_source(header.src_id) else {
            // Nothing this leaf has verified an announcement for, or
            // two that share the projection. Either way there is no
            // key to try, which is the same disposition an ordinary
            // datagram from an unknown peer gets.
            self.counters.drop_for(DropReason::NoSession);
            return Inbound::Refused;
        };
        Self::classified(from, bytes.slice(ROUTING_HEADER_SIZE..), true)
    }

    /// The full node id a relayed packet's 32-bit source names.
    ///
    /// A live session's peer first — a pair already talking resolves
    /// without consulting discovery at all, and its own AEAD is what
    /// confirms the guess — then a verified announcement. Ambiguity
    /// in either direction is `None`.
    fn resolve_relayed_source(&self, src_id: u32) -> Option<NodeId> {
        let mut found = None;
        #[allow(clippy::cast_possible_truncation)]
        for peer in self.sessions.peers().filter(|p| *p as u32 == src_id) {
            if found.replace(peer).is_some() {
                return None;
            }
        }
        found.or_else(|| self.announcements.resolve_routing_id(src_id))
    }

    /// Split a packet with a known sender into handshake or session.
    fn classified(from: NodeId, packet: Bytes, relayed: bool) -> Inbound {
        if is_handshake_packet(&packet) {
            Inbound::Handshake {
                from,
                packet,
                relayed,
            }
        } else {
            Inbound::Session {
                from,
                packet,
                relayed,
            }
        }
    }

    /// Start the NKpsk0 handshake with `peer` as initiator, returning
    /// the message-1 packet to put on the wire.
    ///
    /// `responder_static` must come from the bootstrap credential.
    /// Passing a key read from `GET /rtc/anchor` would make the MITM
    /// witness meaningless, which is why this takes the key rather
    /// than fetching one.
    pub fn begin_handshake(
        &mut self,
        peer: NodeId,
        psk: &[u8; 32],
        responder_static: &[u8; 32],
        slot: u32,
    ) -> Result<Bytes> {
        let (pending, packet) = PendingHandshake::initiate(
            psk,
            responder_static,
            self.identity.node_id(),
            peer,
            rtc_addr(slot, 1),
        )?;
        self.handshakes.insert(peer, pending);
        Ok(packet)
    }

    /// Finish the handshake with `peer` from its message-2 packet.
    pub fn complete_handshake(&mut self, peer: NodeId, msg2: &[u8]) -> Result<()> {
        let pending = self
            .handshakes
            .remove(&peer)
            .ok_or_else(|| LeafError::Session(format!("no handshake in flight with {peer:#x}")))?;
        let session = pending.read_msg2(msg2)?;
        self.install_session(peer, session);
        Ok(())
    }

    /// Accept a peer's NKpsk0 message 1 as **responder**, returning
    /// the message-2 packet to put on the wire.
    ///
    /// §9's browser ↔ browser attempt has no anchor to be the
    /// responder, so one of the two leaves answers with its own
    /// Noise static key. Both halves of the discipline the
    /// initiator has apply here: the PSK is the trust domain's, and
    /// the initiator's claimed node id enters the handshake
    /// **prologue** — the same binding `MeshNode::accept_rtc` makes
    /// for a browser's claimed id — so the installed session is
    /// bound to the id it is keyed under and a peer that claimed a
    /// different one cannot complete.
    ///
    /// `slot` is the transport slot the DataChannel occupies, the
    /// same value [`Self::begin_handshake`] takes.
    pub fn accept_handshake(
        &mut self,
        peer: NodeId,
        psk: &[u8; 32],
        msg1: &[u8],
        slot: u32,
    ) -> Result<Bytes> {
        let (session, msg2) = PendingHandshake::respond(
            psk,
            self.identity.noise(),
            peer,
            self.identity.node_id(),
            rtc_addr(slot, 1),
            msg1,
        )?;
        // A responder that already had a session with this peer is
        // being re-offered: §9 step 4 replaces rather than joins,
        // and the table is keyed on identity precisely so that is a
        // table update.
        self.install_session(peer, session);
        Ok(msg2)
    }

    /// Install a session, retiring whatever it replaced.
    ///
    /// **Replacement is not a table update alone.** The predecessor
    /// owns receive cursors, partial reassemblies and pending calls;
    /// left behind, its reorder state suppresses the successor's
    /// sequence zero as a duplicate and its calls wait for a reply
    /// no one will send on a session that no longer exists. Each of
    /// those is keyed by incarnation, so retirement is exact: the
    /// old incarnation's work ends once, typed, and the successor —
    /// which already holds a different incarnation — is untouched.
    fn install_session(&mut self, peer: NodeId, session: crate::session::LeafSession) {
        let replaced = self.sessions.install(session);
        if let Some(old) = replaced {
            self.retire_incarnation(old.incarnation());
            // The peer's roster entry went with the old session, so
            // a reply channel subscribed on it is not subscribed on
            // this one.
            self.reply_subscriptions.retain(|(p, _)| *p != peer);
            self.rpc_reply_carriers.retain(|(p, _)| *p != peer);
            self.stream_kinds.retain(|(p, _), _| *p != peer);
        }
        self.events.push(LeafEvent::Connected {
            node_id: self.identity.node_id(),
            peer_node: peer,
            rtc_addr: self.peer_rtc_addr.get(&peer).cloned(),
        });
    }

    /// End every piece of work one incarnation owned, exactly once.
    fn retire_incarnation(&mut self, incarnation: u64) {
        self.rx_streams.retain(|(i, _), _| *i != incarnation);
        self.grants_sent.retain(|(i, _), _| *i != incarnation);
        self.acks_sent.retain(|(i, _), _| *i != incarnation);
        self.send_failed.retain(|(i, _)| *i != incarnation);
        self.recv_failed.retain(|(i, _)| *i != incarnation);
        self.rx_closed.retain(|(i, _)| *i != incarnation);
        self.reassembler.retire(incarnation);
        self.calls.fail_incarnation(incarnation);
    }

    /// Tear down the session with `peer`.
    ///
    /// Pending calls on it fail [`RpcError::SessionLost`] — typed,
    /// never silently retried (§8).
    pub fn drop_session(&mut self, peer: NodeId, reason: impl Into<String>) {
        if let Some(old) = self.sessions.remove(peer) {
            self.retire_incarnation(old.incarnation());
        }
        self.handshakes.remove(&peer);
        // A call registered before its session existed has no
        // incarnation to retire it; the peer is still the right key
        // for those.
        self.calls.fail_peer(peer);
        self.stream_kinds.retain(|(p, _), _| *p != peer);
        // The anchor's roster entry died with the session, so a
        // reconnect must re-subscribe or its replies strand again.
        self.reply_subscriptions.retain(|(p, _)| *p != peer);
        self.rpc_reply_carriers.retain(|(p, _)| *p != peer);
        // Addressing dies with the session it addressed. A relay
        // entry left behind would wrap the next attempt's packets
        // for a session that no longer exists.
        self.relays.remove(&peer);
        self.events.push(LeafEvent::Disconnected {
            peer_node: peer,
            reason: reason.into(),
        });
    }

    /// Fail every in-flight call because this tab lost the identity.
    ///
    /// §8's disposition rule: the caller is told which generation
    /// owned the call and decides. Nothing is re-issued.
    pub fn fail_calls_on_leader_loss(&mut self, generation: u64) -> usize {
        self.calls.fail_all(RpcError::LeaderLost { generation })
    }

    /// Periodic work: expire call deadlines and stale reassemblies,
    /// and drive the wire's send-side recovery.
    ///
    /// Called on every inbound packet and from the wasm surface's
    /// timer. Returns how many calls timed out.
    ///
    /// **The retransmit timer has no thread.** A browser leaf has no
    /// runtime, so the reliability layer's RTO is swept here, the
    /// same way call deadlines are: each due descriptor is rebuilt
    /// with a fresh AEAD counter and queued, each stream whose
    /// retries are exhausted becomes a typed terminal failure and a
    /// RESET to the peer, and each gap this leaf is holding becomes
    /// a NACK.
    pub fn tick(&mut self, now: Instant) -> usize {
        let expired = self.calls.expire(now);
        for call in &expired {
            // Tell the server, so it is not left running work
            // nobody awaits.
            let frame = rpc_wire::encode_cancel_frame(
                self.identity.origin_hash(),
                call.call_id,
                call.route,
            );
            let _ = self.send_event_plane(
                call.peer,
                route_stream_id(call.route),
                call.route as u16,
                &frame,
                true,
            );
        }
        self.sweep_reassemblies(now);
        self.drive_reliability();
        expired.len()
    }

    /// One sweep of the wire's send-side recovery across every
    /// session: retransmits, exhaustion, and the NACKs this leaf
    /// owes for gaps it is holding.
    fn drive_reliability(&mut self) {
        let peers: Vec<NodeId> = self.sessions.peers().collect();
        for peer in peers {
            let Some(session) = self.sessions.get(peer) else {
                continue;
            };
            let mut queued: Vec<Bytes> = session.due_retransmits();
            let failed = session.wire().take_failed_stream_ids();
            let nacks = session.gap_nacks();
            let incarnation = session.incarnation();
            let origin = self.identity.origin_hash();
            for stream_id in &failed {
                if let Ok(packets) = session.build_packets(
                    net_wire::session::CONTROL_STREAM_ID,
                    SUBPROTOCOL_STREAM_RESET,
                    0,
                    origin,
                    false,
                    &StreamReset {
                        stream_id: *stream_id,
                    }
                    .encode(),
                ) {
                    queued.extend(packets);
                }
            }
            for nack in &nacks {
                if let Ok(packets) = session.build_packets(
                    net_wire::session::CONTROL_STREAM_ID,
                    SUBPROTOCOL_STREAM_NACK,
                    0,
                    origin,
                    false,
                    &nack.encode(),
                ) {
                    queued.extend(packets);
                }
            }
            for packet in queued {
                self.counters.packet_out();
                self.outbound.push_back(Outbound { peer, packet });
            }
            for stream_id in failed {
                // Terminal, not merely reported: the RESET queued
                // above tells the peer to reset its receive cursor
                // for this id, and `next_tx_seq` does not rewind to
                // match it.
                //
                // Terminal also means the reservation ends. The wire
                // dropped this stream's descriptors when its retries
                // ran out, so nothing of its can be rebuilt — but its
                // header stamps were only released by a cumulative
                // ack or by destroying the session, and a stream that
                // ended this way will never be acked. The slots sat
                // occupied by an owner that owned nothing, and the
                // session's stamp budget is shared, so enough ended
                // streams permanently refused fresh ones with
                // `ReliableWindowFull` — exactly the recovery that
                // error tells the caller to attempt. Retiring by the
                // exact owner keeps the cap intact and evicts nothing
                // live.
                session.retire_stream_stamps(stream_id);
                self.send_failed.insert((incarnation, stream_id));
                self.counters.drop_for(DropReason::StreamFailed);
                self.events.push(LeafEvent::StreamFailed {
                    peer_node: peer,
                    stream_id,
                    reason: StreamFailure::RetransmitsExhausted,
                });
            }
        }
    }

    /// Take everything queued for the transport, addressed.
    ///
    /// Addressing is applied here and nowhere else: every packet the
    /// node enqueues names the peer it is *for*, and a peer reached
    /// through a relay needs a routing envelope and the relay's
    /// transport instead (see [`Self::route_outbound`]). One place,
    /// so a send path added later cannot forget. A leaf with no
    /// relays — every leaf that talks only to its anchor — takes the
    /// original path, allocation for allocation.
    pub fn take_outbound(&mut self) -> Vec<Outbound> {
        if self.relays.is_empty() {
            return self.outbound.drain(..).collect();
        }
        let queued: Vec<Outbound> = self.outbound.drain(..).collect();
        queued
            .into_iter()
            .map(|out| self.route_outbound(out.peer, out.packet))
            .collect()
    }

    /// Take everything queued for the application.
    pub fn drain_events(&mut self) -> Vec<LeafEvent> {
        core::mem::take(&mut self.events)
    }

    // ───────────────────────────── outbound ─────────────────────────

    /// Queue one payload on the event plane.
    fn send_event_plane(
        &mut self,
        peer: NodeId,
        stream_id: u64,
        channel_hash: u16,
        payload: &[u8],
        reliable: bool,
    ) -> Result<()> {
        self.send_subprotocol(
            peer,
            stream_id,
            SUBPROTOCOL_EVENT_PLANE,
            channel_hash,
            payload,
            reliable,
        )
    }

    /// Queue one payload under `subprotocol_id`.
    ///
    /// **The shared terminal fence.** `drive_reliability` marks a
    /// stream's send half `send_failed` when its retransmits are
    /// exhausted, queues a RESET, and does not rewind `next_tx_seq`
    /// to meet the peer's reset receive cursor — so anything queued
    /// afterwards lands on a sequence space the peer has abandoned.
    /// The handle API refused that ([`Self::open_stream`],
    /// `check_handle`), but every producer that does not go through a
    /// handle — [`Self::publish`], [`Self::subscribe`], request
    /// emission, the event plane — arrived here and built anyway.
    /// Terminal disposition belongs to the path they all share.
    ///
    /// Stream-control subprotocols are exempt, and must be: the RESET
    /// that tells the peer about the failure, and the acks, grants and
    /// NACKs the credit loop runs on, are the feedback this refusal is
    /// reporting. Refusing them would refuse the message that carries
    /// the news. Same exemption, same reason, as the receive half's
    /// `recv_failed` gate.
    fn send_subprotocol(
        &mut self,
        peer: NodeId,
        stream_id: u64,
        subprotocol_id: u16,
        channel_hash: u16,
        payload: &[u8],
        reliable: bool,
    ) -> Result<()> {
        let origin_hash = self.identity.origin_hash();
        let session = self
            .sessions
            .get(peer)
            .ok_or_else(|| LeafError::Session(format!("no session with {peer:#x}")))?;
        let incarnation = session.incarnation();
        if !crate::session::is_stream_control(subprotocol_id)
            && self.send_failed.contains(&(incarnation, stream_id))
        {
            return Err(LeafError::Session(format!(
                "stream {stream_id:#x} on the session with {peer:#x} failed terminally \
                 (incarnation {incarnation}); reconnect or use another stream id"
            )));
        }
        let packets = session.build_packets(
            stream_id,
            subprotocol_id,
            channel_hash,
            origin_hash,
            reliable,
            payload,
        )?;
        let fragmented = packets.len() > 1;
        for packet in packets {
            self.counters.packet_out();
            if fragmented {
                self.counters.fragment_out();
            }
            self.outbound.push_back(Outbound { peer, packet });
        }
        Ok(())
    }

    /// Subscribe to `channel` on `peer`, through the production
    /// `0x0A00` encoder.
    ///
    /// Returns the nonce the Ack will echo.
    pub fn subscribe(&mut self, peer: NodeId, channel: &str) -> Result<u64> {
        let channel = Channel::new(channel)?;
        let nonce = self.next_nonce;
        self.next_nonce = self.next_nonce.wrapping_add(1);
        let payload = channel.subscribe_payload(nonce);
        self.send_subprotocol(
            peer,
            channel.publish_stream_id(),
            SUBPROTOCOL_MEMBERSHIP,
            channel.wire_hash(),
            &payload,
            true,
        )?;
        // Registered only after the frame is queued: an id this
        // leaf failed to subscribe to is not a channel it knows.
        self.stream_kinds
            .insert((peer, channel.publish_stream_id()), StreamKind::Channel);
        Ok(nonce)
    }

    /// Publish `payload` on `channel` to `peer`.
    ///
    /// The stream id and the header's channel hint are the ones
    /// `MeshNode::try_publish_to_peer` derives, so the receiver's
    /// per-channel dispatcher sees the frame.
    pub fn publish(&mut self, peer: NodeId, channel: &str, payload: &[u8]) -> Result<()> {
        let channel = Channel::new(channel)?;
        self.send_event_plane(
            peer,
            channel.publish_stream_id(),
            channel.wire_hash(),
            payload,
            true,
        )?;
        self.stream_kinds
            .insert((peer, channel.publish_stream_id()), StreamKind::Channel);
        Ok(())
    }

    /// Open an application stream.
    ///
    /// `stream_id` and `channel_hash` override the derivation when
    /// the caller must match a specific publish contract — which is
    /// how a stream's payload reaches a native node's real handler
    /// rather than only moving a counter.
    ///
    /// The returned handle is **scoped to the session that is
    /// current now**: it carries that session's incarnation, and
    /// [`Self::stream_send`] refuses it once the peer's session has
    /// been replaced. An unfenced id-addressed send is a different
    /// operation and is not what this returns.
    ///
    /// Refuses an id whose send half already failed terminally on
    /// this session — its sequence space cannot rewind to meet the
    /// peer's reset receive cursor, so a handle for it would be a
    /// handle onto a stream that cannot deliver.
    pub fn open_stream(
        &mut self,
        peer: NodeId,
        label: &str,
        reliability: Reliability,
        stream_id: Option<u64>,
        channel_hash: Option<u16>,
    ) -> Result<StreamHandle> {
        let Some(incarnation) = self.sessions.get(peer).map(|s| s.incarnation()) else {
            return Err(LeafError::Session(format!("no session with {peer:#x}")));
        };
        let stream_id = stream_id.unwrap_or_else(|| stream_id_from_label(label));
        if self.send_failed.contains(&(incarnation, stream_id)) {
            return Err(LeafError::Session(format!(
                "stream {stream_id:#x} on the session with {peer:#x} failed terminally \
                 (incarnation {incarnation}); reconnect or use another stream id"
            )));
        }
        // The registry is what the receive path classifies on: an
        // id this leaf opened as a stream is a stream, whatever
        // bits its hash happens to carry.
        self.stream_kinds
            .insert((peer, stream_id), StreamKind::Stream);
        // Reopening is what un-closes the receive half. The cursor
        // a previous `close_stream` left in place is deliberately
        // still there, at the peer's next sequence, so this
        // consumer resumes where the peer actually is instead of
        // waiting for a sequence zero the peer sent long ago.
        self.rx_closed.remove(&(incarnation, stream_id));
        Ok(StreamHandle {
            peer,
            incarnation,
            stream_id,
            channel_hash: channel_hash.unwrap_or(0),
            reliability,
        })
    }

    /// Send on an open stream.
    ///
    /// Refuses a handle opened on a session that has since been
    /// replaced, and a stream whose send half failed terminally.
    /// Both refusals are typed and nothing is queued — the caller
    /// holding a stale value learns it now rather than having its
    /// bytes land on the successor's stream.
    pub fn stream_send(&mut self, handle: StreamHandle, payload: &[u8]) -> Result<()> {
        self.check_handle(handle)?;
        self.send_event_plane(
            handle.peer,
            handle.stream_id,
            handle.channel_hash,
            payload,
            handle.reliability.is_reliable(),
        )
    }

    /// Close an open stream: stop delivering to its consumer and
    /// drop its stream registration.
    ///
    /// **The receive cursor stays**, for the rest of this stream's
    /// lifetime within the session. Close is local — nothing goes
    /// on the wire, one DataChannel carries every stream, and the
    /// peer's half is its own to close — so the peer's transmit
    /// sequence for the id keeps counting up and the wire's receive
    /// state keeps acknowledging it. Deleting the cursor here made
    /// [`Self::open_stream`] on the same id build a fresh one
    /// waiting for sequence zero while the peer sent sequence one:
    /// the record was accepted and acknowledged on the wire and
    /// then held against a gap nothing could ever fill, so a
    /// successful close/reopen silently stranded every later
    /// payload with no loss and no event. Keeping it means a reopen
    /// resumes at the peer's next sequence, and until then arrivals
    /// are counted [`DropReason::StreamClosed`] rather than
    /// delivered to a consumer that is gone.
    ///
    /// The send half is untouched: the same id stays sendable, and
    /// the caller's own transmit sequence is not rewound either.
    ///
    /// Fenced exactly like [`Self::stream_send`], and for the same
    /// reason — a stale handle must not retire the successor's
    /// stream state.
    pub fn close_stream(&mut self, handle: StreamHandle) -> Result<()> {
        self.check_handle(handle)?;
        self.rx_closed
            .insert((handle.incarnation, handle.stream_id));
        self.stream_kinds.remove(&(handle.peer, handle.stream_id));
        Ok(())
    }

    /// The fence both handle-addressed operations share.
    ///
    /// A `StreamHandle` is a value the caller keeps. Replacement
    /// retires the predecessor's tables, but it cannot reach a
    /// value already handed out, so the check belongs where the
    /// handle is *used*: the incarnation it was opened on must
    /// still be the peer's current session.
    fn check_handle(&self, handle: StreamHandle) -> Result<()> {
        let current = self
            .sessions
            .get(handle.peer)
            .map(|s| s.incarnation())
            .ok_or_else(|| LeafError::Session(format!("no session with {:#x}", handle.peer)))?;
        if current != handle.incarnation {
            return Err(LeafError::Session(format!(
                "stale stream handle: opened on incarnation {} of the session with {:#x}, \
                 which now holds incarnation {current}; reopen the stream",
                handle.incarnation, handle.peer
            )));
        }
        if self.send_failed.contains(&(current, handle.stream_id)) {
            return Err(LeafError::Session(format!(
                "stream {:#x} failed terminally on this session (incarnation {current})",
                handle.stream_id
            )));
        }
        Ok(())
    }

    /// Make an nRPC call. The future resolves to the reply body or a
    /// typed failure.
    ///
    /// Registers the call **before** the request goes out, so a
    /// reply that arrives while this function is still returning
    /// cannot find an empty table.
    pub fn call(
        &mut self,
        peer: NodeId,
        service: &str,
        payload: &[u8],
        timeout_ms: Option<u64>,
    ) -> Result<CallResult> {
        let request = request_channel(service)?;
        let request = Channel::from_name(request);
        // The route discriminator is the CANONICAL u64 hash of the
        // channel the frame rides, not the u16 bucket.
        let route = request.canonical();

        // **Subscribe to the reply channel before the REQUEST goes
        // out.** An anchor forwards an nRPC RESPONSE only to
        // subscribers of `<service>.replies.<origin>`, so without
        // this the handler runs, the reply is published, nothing
        // routes it here, and the caller sees `rpc: the call's
        // deadline elapsed` — indistinguishable from a service that
        // never answered. Once per (peer, service): the membership
        // frame is reliable and ordered ahead of the REQUEST on the
        // same channel, and re-sending it per call would be traffic
        // §2's admission should refuse.
        self.ensure_reply_subscription(peer, service)?;

        // The triple a reply must present. The incarnation is read
        // now, so a session replaced while the call is in flight
        // retires it rather than letting the successor's traffic
        // answer it.
        let incarnation = self
            .sessions
            .get(peer)
            .ok_or_else(|| LeafError::Session(format!("no session with {peer:#x}")))?
            .incarnation();
        let reply_route =
            Channel::from_name(reply_channel(service, self.identity.origin_hash())?).canonical();
        // The channel the reply must ride, as a stream id: the same
        // derivation the publisher uses for that channel name, so
        // the expected carrier is computed from the reply channel
        // this leaf subscribed to and never taken from the frame.
        let carrier_stream_id = route_stream_id(reply_route);

        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS);
        let (call_id, receiver) = self
            .calls
            .register(
                CallOwner {
                    peer,
                    incarnation,
                    reply_route,
                    carrier_stream_id,
                },
                route,
                timeout_ms,
            )
            .map_err(LeafError::Rpc)?;

        let deadline_ns =
            clock::now_unix_nanos().saturating_add(timeout_ms.saturating_mul(1_000_000));
        let frame = rpc_wire::encode_request_frame(
            self.identity.origin_hash(),
            call_id,
            route,
            &RpcRequestPayload::unary(service, deadline_ns, Bytes::copy_from_slice(payload)),
        )?;
        if let Err(e) = self.send_event_plane(
            peer,
            request.publish_stream_id(),
            request.wire_hash(),
            &frame,
            true,
        ) {
            // The call never reached the wire. Fail it now rather
            // than let it time out — and never retry it.
            self.calls.take(call_id);
            return Err(e);
        }
        Ok(receiver)
    }

    /// Subscribe to `service`'s reply channel on `peer`, unless this
    /// leaf already has.
    ///
    /// Idempotent per `(peer, service)`. Returns whether a
    /// membership frame was queued, so a test can assert the
    /// cadence rather than infer it from packet counts.
    pub fn ensure_reply_subscription(&mut self, peer: NodeId, service: &str) -> Result<bool> {
        let reply = reply_channel(service, self.identity.origin_hash())?;
        let key = (peer, net_wire::channel::name::channel_hash(reply.as_str()));
        if self.reply_subscriptions.contains(&key) {
            return Ok(false);
        }
        self.subscribe(peer, reply.as_str())?;
        // Recorded only after the frame is queued: a refused
        // subscribe must not mark the channel as subscribed, or
        // every later call on it strands its reply silently.
        self.reply_subscriptions.insert(key);
        // The same fact, in the form the receive path classifies
        // on: from here the RPC plane OWNS this carrier, so a frame
        // arriving on it is RPC-plane traffic and a frame arriving
        // anywhere else is not. `key.1` is the reply channel's
        // canonical hash, and `route_stream_id` is the derivation
        // both its publisher and `call`'s
        // `CallOwner::carrier_stream_id` use.
        self.rpc_reply_carriers
            .insert((peer, route_stream_id(key.1)));
        Ok(true)
    }

    /// The reply channel this leaf must be subscribed to before a
    /// call to `service` can be answered.
    pub fn reply_channel_for(&self, service: &str) -> Result<String> {
        reply_channel(service, self.identity.origin_hash()).map(|c| c.as_str().to_string())
    }

    /// **The enrollment exchange** — the nRPC client's first caller,
    /// and the step that promotes the session out of provisional.
    ///
    /// Two packets, in this order, both reliable on the same
    /// DataChannel so the anchor sees them in order:
    ///
    /// 1. a `0x0A00` Subscribe to `net.mesh.enroll.replies.<our
    ///    origin>` — §12 permits exactly this one channel, with no
    ///    token and no queue group, and the anchor cannot publish
    ///    the reply until our roster entry exists;
    /// 2. the nRPC REQUEST to `net.mesh.enroll` carrying a signed
    ///    [`JoinRequest`](crate::enroll::build_join_request).
    ///
    /// Returns the call's receiver. Feed the reply body to
    /// [`Self::finish_enrollment`].
    ///
    /// Nothing here retries. The invite is single-use, so a silent
    /// retry would burn it and turn a legible refusal into an
    /// illegible `REPLAY`.
    pub fn begin_enrollment(
        &mut self,
        peer: NodeId,
        invite: &crate::enroll::Invite,
        device_name: &str,
        tags: &[String],
        timeout_ms: Option<u64>,
    ) -> Result<CallResult> {
        invite.validate_at(clock::now_unix_secs())?;
        let body = crate::enroll::build_join_request(&self.identity, device_name, tags, invite)?;

        // `call` subscribes to the reply channel itself — the
        // enrollment special case turned out to BE the general case
        // (MergedRunner's defect 1). The reply channel is
        // origin-bound: `authorize_subscribe` admits a subscriber
        // only to the name carrying its own origin, and §12's
        // allow-list checks the same equality, so the name is
        // derived and never taken from a caller.
        self.call(peer, crate::enroll::ENROLL_SERVICE, &body, timeout_ms)
    }

    /// Parse an enrollment reply and record the delegation chain.
    ///
    /// `Ok(chain)` means the anchor admitted this leaf and the
    /// session is no longer provisional. A refusal comes back as a
    /// typed [`LeafError::Identity`] naming the stable reject code
    /// and the operator's message — never as a timeout, and never
    /// swallowed into a "connected anyway".
    pub fn finish_enrollment(&mut self, reply: &[u8]) -> Result<Vec<u8>> {
        let chain = crate::enroll::JoinOutcome::decode(reply)?.into_chain()?;
        self.delegation_chain = Some(chain.clone());
        Ok(chain)
    }

    /// Whether this leaf holds an admitted delegation chain.
    ///
    /// `false` means the session is still provisional, which is the
    /// single fact that explains a `call` dying on its deadline with
    /// the service's handler never having run.
    pub fn is_enrolled(&self) -> bool {
        self.delegation_chain.is_some()
    }

    /// The delegation chain the anchor issued, if any.
    pub fn delegation_chain(&self) -> Option<&[u8]> {
        self.delegation_chain.as_deref()
    }

    /// Build and sign this leaf's announcement, bumping its version.
    ///
    /// The bytes are for
    /// [`ControlPlane::publish_announcement`](crate::control_plane::ControlPlane::publish_announcement);
    /// [`Self::announce_to_peer`] additionally sends them to one peer
    /// over the session, which is the route-learning path §7 reuses.
    pub fn build_announcement(&mut self, capabilities: &[String]) -> Result<Vec<u8>> {
        let version = self.announcement_version;
        self.announcement_version = self.announcement_version.saturating_add(1);
        announce::build_announcement(
            &self.identity,
            capabilities,
            version,
            clock::now_unix_nanos(),
            announce::DEFAULT_TTL_SECS,
        )
    }

    /// Send an already-signed announcement to one peer as a
    /// `0x0C00` capability-announcement frame.
    ///
    /// **Not a fold frame.** It used to ride `0x1000`, and the
    /// receiving anchor dropped every one of them: `0x1000` is routed
    /// to the fold router, which expects a fold envelope and refuses
    /// a bare announcement document at `debug` level — so the leaf
    /// announced, the anchor logged nothing an operator would see,
    /// and `find_best_node` never resolved the browser node.
    /// `0x0C00` is the subprotocol whose handler
    /// (`handle_capability_announcement`) verifies the signature,
    /// pins the entity and feeds the capability index that
    /// `find_best_node` reads. The stream id is the subprotocol id,
    /// which is what the core's own `build_subprotocol_packet` uses
    /// for control frames.
    pub fn announce_to_peer(&mut self, peer: NodeId, announcement: &[u8]) -> Result<()> {
        self.send_subprotocol(
            peer,
            u64::from(SUBPROTOCOL_CAPABILITY_ANN),
            SUBPROTOCOL_CAPABILITY_ANN,
            0,
            announcement,
            true,
        )
    }

    /// Ingest an announcement the control plane returned, verifying
    /// it first.
    ///
    /// `false` means it was refused or superseded — and a refusal
    /// moves [`DropReason::AnnouncementUnverified`].
    pub fn ingest_announcement(&mut self, bytes: &[u8]) -> bool {
        match announce::verify_announcement(bytes) {
            Ok(verified) => {
                let changed = self.announcements.ingest(verified.clone());
                if changed {
                    self.events.push(LeafEvent::Announcement(verified));
                }
                changed
            }
            Err(_) => {
                self.counters.drop_for(DropReason::AnnouncementUnverified);
                self.events.push(LeafEvent::Dropped {
                    reason: DropReason::AnnouncementUnverified,
                });
                false
            }
        }
    }

    /// Answer a capability query from verified state.
    pub fn query(&self, capability: &str) -> String {
        self.announcements.query_json(capability)
    }

    /// The announcement held for `node`, if this leaf verified one.
    pub fn announcement_for(&self, node: NodeId) -> Option<&VerifiedAnnouncement> {
        self.announcements.get(node)
    }

    // ───────────────────────────── inbound ──────────────────────────

    /// Feed one datagram from the transport.
    ///
    /// The whole receive path: routing-envelope discrimination,
    /// session lookup, AEAD open, receive-credit accounting,
    /// reassembly, per-stream reorder, subprotocol dispatch. Every
    /// refusal moves a counter.
    pub fn on_datagram(&mut self, peer: NodeId, bytes: Bytes, now: Instant) {
        self.counters.packet_in();
        // **The deadline is evaluated before anything this reading
        // admits or acknowledges.** A reassembly group past its TTL
        // is not an open group, and the receive path used to learn
        // that only *after* it had acknowledged the arriving piece:
        // `admits` answered `true` for the group because the map
        // still held it, the sequence was cumulatively
        // acknowledged — retiring the sender's last copy of the
        // head — and the reap then happened inside the very next
        // call, leaving the tail to open an orphan group nothing
        // could complete. Sweeping here, against this call's own
        // clock reading, means the stream's terminal disposition is
        // latched in `recv_failed` before the acknowledgement
        // decision is taken, so a tail can never be acknowledged
        // into a group that no longer has its head.
        self.sweep_reassemblies(now);
        let self_node = self.identity.node_id();
        let Some(inner) = dispatch::unwrap_routing(bytes, self_node, &self.counters) else {
            return;
        };

        let Some(session) = self.sessions.get(peer) else {
            self.counters.drop_for(DropReason::NoSession);
            return;
        };
        let opened = match session.open_packet(&inner) {
            Ok(opened) => opened,
            Err(LeafError::Wire(reason)) if reason.contains("replay") => {
                self.counters.drop_for(DropReason::Replay);
                return;
            }
            Err(_) => {
                self.counters.drop_for(DropReason::Unparsable);
                return;
            }
        };
        let incarnation = session.incarnation();
        let subprotocol_id = opened.subprotocol_id;

        // A stream whose receive half this leaf gave up on takes
        // nothing further: not a new reorder cursor (the hole that
        // ended it does not refill), not a partial reassembly
        // nothing will complete, and not credit or an ack — those
        // would tell the sender its bytes became progress. The
        // sender's own retransmit loop is already stuck on the
        // sequence this leaf never got, and that is what ends its
        // half. Dropped and counted, never silent.
        if !crate::session::is_stream_control(subprotocol_id)
            && self.recv_failed.contains(&(incarnation, opened.stream_id))
        {
            self.counters.drop_for(DropReason::StreamFailed);
            return;
        }

        // **Admission before acknowledgement.** A fragment the
        // reassembler will not retain must stay the peer's to send
        // again: acknowledging it cumulatively takes that freedom
        // away, and the group's bytes are then lost with nothing
        // left to rebuild them from. Refusing here leaves the
        // sequence outstanding, so the ordinary retransmit brings
        // the piece back once a slot frees — capacity pressure
        // becomes backpressure instead of silent loss. Counted by
        // `admits`, which owns the reason.
        if !self.reassembler.admits(
            incarnation,
            opened.fragment_id,
            opened.frag_flags,
            now,
            &self.counters,
        ) {
            return;
        }

        // **Feed the credit and acknowledgement loop.** Sharing a
        // `NetSession` does not advance its receive state; a leaf
        // that never did this let a native sender's window drain to
        // nothing on a loss-free DataChannel. The stream-control
        // subprotocols are exempt: crediting the credit loop would
        // be circular.
        if !crate::session::is_stream_control(subprotocol_id) {
            let owed = match session.note_received(
                opened.stream_id,
                opened.reliable,
                opened.mode_boundary,
                opened.sequence,
                event_frame_bytes(&opened.events),
            ) {
                Some(grant) => Some((grant, false)),
                // The reliability layer refused the sequence, so
                // this is a retransmit — the peer telling us our
                // ack never reached it. Repeat it unconditionally:
                // the cadence that was throttling it is exactly
                // what is not working.
                None if opened.reliable => {
                    session.repeat_ack(opened.stream_id).map(|ack| (ack, true))
                }
                None => None,
            };
            // **The ack says what arrived; the NACK says what is
            // missing, and only one of them a sender can act on.**
            // This arrival already mints a window frame carrying
            // `ack_seq` — "everything below N is mine" — which is
            // exactly the fact a sender holding descriptors for
            // N..M cannot use: a frontier that has not moved is
            // equally consistent with a lost packet and with
            // packets still in flight. When the arrival REVEALED a
            // hole, the receiver knows which sequence, and that is
            // the moment to say so; deferring it to the periodic
            // sweep costs a whole tick of recovery latency for
            // nothing, and on a promoted stream it costs more than
            // latency — the reliable region cannot be conceded, so
            // until the retransmit arrives every later record is
            // held.
            //
            // Bounded by the gap, not by the traffic: the flag is
            // taken, so eight arrivals stacked behind one loss
            // report that loss once, and only a new disjoint hole
            // reports again.
            let gap_nack = session
                .opened_gap_nack(opened.stream_id)
                .and_then(|nack| {
                    session
                        .build_packets(
                            net_wire::session::CONTROL_STREAM_ID,
                            SUBPROTOCOL_STREAM_NACK,
                            0,
                            self.identity.origin_hash(),
                            false,
                            &nack.encode(),
                        )
                        .ok()
                })
                .unwrap_or_default();
            match owed {
                Some((frame, true)) => self.send_stream_window(peer, incarnation, frame),
                Some((frame, false)) => self.maybe_grant(peer, incarnation, frame),
                None => {}
            }
            for packet in gap_nack {
                self.counters.packet_out();
                self.outbound.push_back(Outbound { peer, packet });
            }
        }

        // **The consumer's mode is decided at ADMISSION of the
        // packet, not at emission of a completed record.** A
        // reliable message can span fragments, and its head's
        // arrival is where its receive obligation begins: until the
        // group completes there is no record to carry the mode, so
        // a promotion deferred to completion left the consumer
        // fire-and-forget while the group assembled. A small
        // fire-and-forget message arriving between head and tail
        // then skipped the cursor past the reliable head's
        // sequence, and the assembled message — every fragment
        // present, nothing lost anywhere — was refused as a
        // duplicate of sequences the cursor had already walked
        // over.
        //
        // So the packet promotes the stream, and the same call
        // applies the sender's signalled boundary, which may
        // concede a fire-and-forget prefix and release what was
        // held behind it. Stream-control traffic is exempt for the
        // same reason it is exempt from the credit loop and from
        // reordering.
        let mut delivered: Vec<StreamRecord> = Vec::new();
        if opened.reliable && !crate::session::is_stream_control(subprotocol_id) {
            let stream = self
                .rx_streams
                .entry((incarnation, opened.stream_id))
                .or_insert_with(|| RxStream::new(Reliability::Reliable));
            delivered.extend(stream.promote(opened.mode_boundary, &self.counters));
        }

        // Reassembly is per (incarnation, fragment group): a
        // replaced session restarts its fragment ids at 1, so
        // keying on the peer would let a predecessor's partial
        // group absorb a successor's first fragment.
        let mut records: Vec<StreamRecord> = Vec::with_capacity(opened.events.len());
        for event in opened.events {
            let meta = PieceMeta {
                sequence: opened.sequence,
                stream_id: opened.stream_id,
                origin_hash: opened.origin_hash,
                channel_hash: opened.channel_hash,
                subprotocol_id,
                reliable: opened.reliable,
            };
            let Some(assembled) = self.reassembler.accept_piece(
                incarnation,
                meta,
                opened.fragment_id,
                opened.fragment_offset,
                opened.frag_flags,
                event,
                now,
                &self.counters,
            ) else {
                continue;
            };
            // Events that completed under the same sequence span
            // are one delivery record; a fragment group carries the
            // sequence, header, plane and mode of its FIRST
            // fragment, never of whichever packet happened to
            // complete it. Two payloads only share a record when
            // their whole provenance agrees.
            match records.iter_mut().find(|r| {
                r.seq == assembled.meta.sequence
                    && r.span == assembled.span
                    && r.subprotocol_id == assembled.meta.subprotocol_id
            }) {
                Some(record) => record.payloads.push(assembled.data),
                None => records.push(StreamRecord {
                    seq: assembled.meta.sequence,
                    span: assembled.span,
                    stream_id: assembled.meta.stream_id,
                    subprotocol_id: assembled.meta.subprotocol_id,
                    reliable: assembled.meta.reliable,
                    origin_hash: assembled.meta.origin_hash,
                    channel_hash: assembled.meta.channel_hash,
                    payloads: vec![assembled.data],
                }),
            }
        }
        // `delivered` may already hold what the promotion above
        // released — a boundary that conceded a fire-and-forget
        // prefix unblocks records held behind it whether or not
        // this packet also completed one.
        if records.is_empty() && delivered.is_empty() {
            self.dispose_abandoned_groups();
            return;
        }

        // **One sequence-disposition model.** A stream's sequence
        // space is shared by every subprotocol that rides real
        // stream ids, and `subscribe` puts a reliable `0x0A00`
        // membership frame on the *channel's publisher stream* —
        // sequence zero of exactly the stream the following
        // publication uses. Reordering only the event plane left
        // that zero consumed on the wire and unconsumed by the
        // consumer, so the publication at sequence one was held
        // against a hole nothing could ever fill: a subscribe
        // followed by a publish on one channel never delivered,
        // with no loss anywhere. So every non-feedback subprotocol
        // advances the cursor when it is consumed.
        //
        // The exemption is **feedback**, not "control": the four
        // stream-control messages (window/ack/nack/reset) are the
        // credit and reliability loop itself, ride
        // `CONTROL_STREAM_ID`, and holding one behind a gap would
        // deadlock the stream it exists to unblock. That is the
        // same set `is_stream_control` exempts from receive
        // accounting, and the same distinction the native side
        // makes.
        //
        // Dispatch is therefore deferred per record, not per
        // packet: each record carries the subprotocol that put it
        // on the stream, so a publication released by a later
        // membership frame is still decoded as a publication — and
        // a reassembled group carries its first fragment's plane
        // and mode, so the packet that completes it cannot move it
        // to another plane or change how its stream treats a gap.
        for record in records {
            if crate::session::is_stream_control(record.subprotocol_id) {
                delivered.push(record);
                continue;
            }
            let reliability = if record.reliable {
                Reliability::Reliable
            } else {
                Reliability::FireAndForget
            };
            let outcome = {
                let stream = self
                    .rx_streams
                    .entry((incarnation, record.stream_id))
                    .or_insert_with(|| RxStream::new(reliability));
                stream.accept(record, &self.counters)
            };
            match outcome {
                Ok(released) => delivered.extend(released),
                // Terminal. Everything released before it is
                // still in order and is still delivered; the
                // stream takes nothing more.
                Err(overflow) => {
                    self.fail_receive_half(peer, incarnation, overflow);
                    break;
                }
            }
        }
        for record in delivered {
            // A closed consumer still advances its cursor — the
            // peer's sequence was never rewound — but nothing is
            // delivered to it until a reopen.
            if self.rx_closed.contains(&(incarnation, record.stream_id)) {
                self.counters
                    .drop_n(DropReason::StreamClosed, record.payloads.len() as u64);
                continue;
            }
            for payload in record.payloads.clone() {
                self.handle_event(peer, &record, payload, now);
            }
        }
        // Whatever this arrival's reassembly gave up on is disposed
        // of after what did assemble is delivered: records released
        // in order stay in order, and the stream then ends typed.
        self.dispose_abandoned_groups();
    }

    /// Apply the reassembly deadline, and dispose of what it took.
    ///
    /// One place, so the sweep and the ownership it surrenders can
    /// never be separated by an acknowledgement.
    fn sweep_reassemblies(&mut self, now: Instant) {
        self.reassembler.expire(now, &self.counters);
        self.dispose_abandoned_groups();
    }

    /// Disposition every group the reassembler gave up on — by its
    /// own mode's contract.
    ///
    /// **An acknowledged RELIABLE group is owned.** The receive path
    /// acknowledges a fragment's sequence before the group is
    /// complete — it has to, or the sender's window never opens —
    /// and that acknowledgement retires the sender's only copy.
    /// When a bound then ends the group, there is nothing left to
    /// rebuild from and no wire gap left to NACK, because the
    /// sequences were consumed. Counting the loss and returning
    /// leaves the consumer waiting forever on a stream that has
    /// stopped delivering with no event at all. So the stream ends
    /// here, named and typed, exactly as a reorder overflow does.
    ///
    /// **A fire-and-forget group is a permitted loss.** Its
    /// sequences were never retransmittable, so no acknowledgement
    /// took anything away, and an incomplete group is precisely the
    /// outcome its mode's contract allows. Applying
    /// complete-or-terminal to it made one expected fragment loss
    /// permanently fatal to every later message on that stream id —
    /// a policy strictly worse than the loss it was reacting to, and
    /// one nobody asked for. It is counted where it is reaped
    /// ([`DropReason::ReassemblyExpired`] and the other reassembly
    /// reasons) and the stream carries on.
    fn dispose_abandoned_groups(&mut self) {
        let abandoned = self.reassembler.take_abandoned();
        if abandoned.iter().all(|group| !group.reliable) {
            return;
        }
        // A group's scope is its session's incarnation, which is
        // process-unique, so this names one peer or none.
        let owners: Vec<(u64, NodeId)> = self
            .sessions
            .peers()
            .filter_map(|peer| self.sessions.get(peer).map(|s| (s.incarnation(), peer)))
            .collect();
        for group in abandoned {
            if !group.reliable {
                continue;
            }
            // The session that owned the scope is gone. Its whole
            // receive lifetime was retired with it and the
            // consumer was told about that, so there is no live
            // stream left to end.
            let Some((_, peer)) = owners.iter().find(|(inc, _)| *inc == group.scope) else {
                continue;
            };
            self.end_receive_half(
                *peer,
                group.scope,
                group.stream_id,
                StreamFailure::ReassemblyAbandoned,
            );
        }
    }

    /// End the receive half of one reliable stream, typed.
    ///
    /// The reorder bound was reached with the head gap still open.
    /// The cursor is dropped, the id is marked so nothing rebuilds
    /// one, and the consumer is told which stream ended — the whole
    /// point of [`LeafEvent::StreamFailed`] over a drop counter.
    fn fail_receive_half(
        &mut self,
        peer: NodeId,
        incarnation: u64,
        overflow: crate::stream::ReorderOverflow,
    ) {
        self.end_receive_half(
            peer,
            incarnation,
            overflow.stream_id,
            StreamFailure::ReorderOverflow,
        );
    }

    /// The one terminal receive-half transition, whatever reached
    /// it: cursor dropped, id latched against a rebuild, consumer
    /// told once.
    ///
    /// Once, because the latch is the guard: a stream that has
    /// already ended does not end again, so a burst of groups
    /// reaped together on one clock reading produces one event per
    /// stream rather than one per group.
    fn end_receive_half(
        &mut self,
        peer: NodeId,
        incarnation: u64,
        stream_id: u64,
        reason: StreamFailure,
    ) {
        if !self.recv_failed.insert((incarnation, stream_id)) {
            return;
        }
        self.rx_streams.remove(&(incarnation, stream_id));
        self.counters.drop_for(DropReason::StreamFailed);
        self.events.push(LeafEvent::StreamFailed {
            peer_node: peer,
            stream_id,
            reason,
        });
    }

    /// Queue the stream-window frame `peer` is owed for what just
    /// arrived, when either half of it has something new to say.
    ///
    /// One frame carries two independent facts, and they do **not**
    /// share a cadence.
    ///
    /// * `total_consumed` is **credit**. Credit is a volume, and one
    ///   grant per inbound packet — what the wire's receive
    ///   accounting offers — would double this leaf's packet count
    ///   for no gain. Half a window is the cadence: the sender still
    ///   has the other half in hand when the grant is minted, so
    ///   sustained traffic never stalls, and grants are
    ///   authoritative, so a skipped one is subsumed by the next.
    ///
    /// * `ack_seq` is the **cumulative acknowledgement**, and it is
    ///   not a volume. The peer's retransmit window prunes against
    ///   it and its congestion window is capped by what is still
    ///   unacked: a native sender collapses that window to
    ///   `MIN_CWND` the first time a packet's RTO elapses unacked
    ///   and retires the packet after `DEFAULT_MAX_RETRIES`
    ///   elapses, resetting the stream. nRPC bodies are ~100 bytes,
    ///   so a leaf that only spoke every half window (32 KiB) never
    ///   acknowledged at all — the anchor's reply stream stalled one
    ///   RTO into a conversation and its `serve_rpc` publish began
    ///   failing with backpressure. An advance of the ack position
    ///   therefore goes out at once.
    ///
    /// A fire-and-forget stream has no cumulative ack (`ack_seq`
    /// stays 0), so its frames keep the volume cadence exactly.
    fn maybe_grant(&mut self, peer: NodeId, incarnation: u64, grant: StreamWindow) {
        let key = (incarnation, grant.stream_id);
        let acknowledged = self.acks_sent.get(&key).copied().unwrap_or(0);
        if grant.ack_seq <= acknowledged {
            let last = self.grants_sent.get(&key).copied().unwrap_or(0);
            let threshold = self
                .sessions
                .get(peer)
                .and_then(|s| s.wire().try_stream(grant.stream_id))
                .map(|s| u64::from(s.rx_credit().window_bytes()) / 2)
                .unwrap_or(0)
                .max(1);
            if grant.total_consumed.saturating_sub(last) < threshold {
                return;
            }
        }
        self.send_stream_window(peer, incarnation, grant);
    }

    /// Put one stream-window frame on the wire and record both
    /// watermarks it just made the peer's knowledge.
    ///
    /// The frame rides the control stream unreliably: it is
    /// authoritative (it carries the receiver's whole picture, not a
    /// delta), so the next one repairs a lost one, and retaining a
    /// retransmit descriptor for an ack would put the credit loop
    /// inside the machinery it exists to unblock.
    fn send_stream_window(&mut self, peer: NodeId, incarnation: u64, frame: StreamWindow) {
        let payload = frame.encode();
        if self
            .send_subprotocol(
                peer,
                net_wire::session::CONTROL_STREAM_ID,
                SUBPROTOCOL_STREAM_WINDOW,
                0,
                &payload,
                false,
            )
            .is_ok()
        {
            self.counters.credit_grant_sent();
            let key = (incarnation, frame.stream_id);
            self.grants_sent.insert(key, frame.total_consumed);
            self.acks_sent.insert(key, frame.ack_seq);
        }
    }

    /// One decoded event, dispatched under **the record's own**
    /// subprotocol rather than the arriving packet's: a record
    /// released from the reorder buffer was put there by a
    /// different frame than the one that unblocked it.
    fn handle_event(&mut self, peer: NodeId, record: &StreamRecord, payload: Bytes, now: Instant) {
        let Some(decoded) =
            dispatch::dispatch_event(record.subprotocol_id, payload, &self.counters)
        else {
            self.events.push(LeafEvent::Dropped {
                reason: DropReason::UnknownSubprotocol,
            });
            return;
        };
        match decoded {
            Decoded::Event(payload) => self.handle_event_plane(peer, record, payload),
            Decoded::Announcement(bytes) | Decoded::Fold(bytes) => {
                self.ingest_announcement(&bytes);
            }
            Decoded::Signal(envelope) => {
                self.accept_signal(envelope, now);
            }
            // The wire crate owns the credit arithmetic and the
            // retransmit window; the leaf is the driver that feeds
            // them what arrives and puts what they produce on the
            // wire.
            Decoded::StreamWindow(grant) => {
                self.counters.credit_grant_received();
                let Some(session) = self.sessions.get(peer) else {
                    return;
                };
                if let Some(stream) = session.wire().try_stream(grant.stream_id) {
                    // Including the clamp against our own
                    // `tx_bytes_sent` watermark that stops a hostile
                    // grant underflowing it.
                    stream.apply_authoritative_grant(grant.total_consumed);
                }
                session.apply_ack(grant.stream_id, grant.ack_seq);
            }
            Decoded::StreamAck(ack) => {
                if let Some(session) = self.sessions.get(peer) {
                    session.apply_ack_ranges(&ack);
                }
            }
            Decoded::StreamNack(nack) => {
                let Some(session) = self.sessions.get(peer) else {
                    return;
                };
                let packets = session.nack_retransmits(&nack);
                for packet in packets {
                    self.counters.packet_out();
                    self.outbound.push_back(Outbound { peer, packet });
                }
            }
            Decoded::StreamReset(reset) => {
                // The peer's send half gave up: nothing else is
                // coming under the sequences it was using, and it
                // may restart the id from zero — which is why the
                // *whole* receive half goes, the wire's cursor and
                // reliability ranges included
                // (`NetSession::reset_rx_stream`, the same call the
                // native ingress makes on a RESET). Dropping only
                // the leaf's own `RxStream` left the wire refusing
                // the restarted sequence zero as a duplicate.
                //
                // The stream's retained fragments go with it, and
                // that is not the same table: a reset starts a new
                // **receive lifetime** inside one session, while
                // reassembly is keyed only by incarnation, so a
                // head held from before the reset would otherwise
                // wait for an old delayed tail and complete after
                // it — delivering a payload from the lifetime that
                // just ended, against the fresh cursor, behind the
                // `PeerReset` the consumer was already given. Only
                // this stream's groups: another stream's are its
                // own lifetime, and the sequence-less control
                // groups belong to no stream at all.
                if let Some(session) = self.sessions.get(peer) {
                    let incarnation = session.incarnation();
                    session.wire().reset_rx_stream(reset.stream_id);
                    self.rx_streams.remove(&(incarnation, reset.stream_id));
                    self.reassembler
                        .retire_stream(incarnation, reset.stream_id, &self.counters);
                    // A fresh receive half is exactly what an
                    // overflow-terminated one needed, so this
                    // clears that verdict. The send half's is a
                    // different direction and is untouched.
                    self.recv_failed.remove(&(incarnation, reset.stream_id));
                }
                self.counters.drop_for(DropReason::StreamFailed);
                self.events.push(LeafEvent::StreamFailed {
                    peer_node: peer,
                    stream_id: reset.stream_id,
                    reason: StreamFailure::PeerReset,
                });
            }
            // A leaf serves no membership requests.
            Decoded::Membership(_) => {}
        }
    }

    /// An event-plane frame: an nRPC reply for one of our calls, or
    /// an application message.
    ///
    /// **Which of the two is decided by PLANE OWNERSHIP, never by
    /// the payload.** One event-plane subprotocol carries both, and
    /// the two earlier attempts to tell them apart both read the
    /// bytes:
    ///
    /// 1. *Parse it as a reply first.* That is not a discrimination,
    ///    it is a guess against application-chosen bytes, and it
    ///    lost data: `DISPATCH_RPC_DEADLINE_EXCEEDED` is the single
    ///    byte `0x13` at offset 0 of a frame with **no body at
    ///    all**, so every application payload of 24 B or more whose
    ///    first byte happened to be `0x13` decoded as a complete
    ///    deadline reply, went to the call table, matched no call,
    ///    and was counted away — acknowledged on the wire and never
    ///    delivered. The browser matrix showed it as one payload in
    ///    twelve missing from a reliable stream with
    ///    `unknown_call: 1` and every other counter at zero, at the
    ///    12-in-256 rate a burst of twelve consecutive first bytes
    ///    predicts; a subscribed channel was losing 1 message in
    ///    256 the same way, with the same single counter to show
    ///    for it.
    /// 2. *Also require the frame to name its own carrier* — the
    ///    route at offset 24..32 having `route_stream_id` equal to
    ///    the arrival stream. Narrower, and still the payload
    ///    deciding: bytes that are self-consistent are not an
    ///    identity. Opaque application bytes that begin `0x13` and
    ///    carry their own channel's canonical route — a forwarder
    ///    relaying RPC-shaped records, a log frame, a fixture —
    ///    satisfy it deterministically, with no hash collision and
    ///    nothing forged, and were refused as `UnknownCall`.
    ///
    /// So the question is not *what do these bytes look like* but
    /// **whose plane sent them**, and that is answered before the
    /// payload is touched. The nRPC plane's receive-side identity is
    /// the set of reply carriers it subscribed to for its own calls
    /// ([`LeafNode::rpc_reply_carriers`], written only by
    /// [`LeafNode::ensure_reply_subscription`]). A frame on one of
    /// those carriers is RPC-plane traffic; a frame on anything else
    /// is application bytes, delivered byte-exact whatever they look
    /// like. Nothing deliverable is turned away, because
    /// [`LeafNode::call`] registers the carrier **before** it
    /// registers the call, and
    /// [`CallTable::deliver`](crate::rpc::CallTable::deliver)
    /// accepts only a pending entry whose `carrier_stream_id` is
    /// that same derivation — so every reply the call table can
    /// accept arrives on a carrier this set holds.
    ///
    /// The route and carrier checks below are **kept**, unchanged,
    /// for the frames that genuinely are RPC: plane ownership says
    /// which plane a frame belongs to, those checks say which *call*
    /// within it. This replaces the sniffing; it relaxes no fencing.
    fn handle_event_plane(&mut self, peer: NodeId, record: &StreamRecord, payload: Bytes) {
        if !self.rpc_reply_carriers.contains(&(peer, record.stream_id)) {
            // No RPC plane owns this carrier, so these are
            // application bytes — and which surface they belong to
            // is the stream's business, not the payload's.
            match self.classify(peer, record.stream_id) {
                StreamKind::Stream => self.events.push(LeafEvent::StreamData {
                    stream_id: record.stream_id,
                    seq: record.seq,
                    payload,
                }),
                StreamKind::Channel => self.events.push(LeafEvent::ChannelMessage {
                    channel_hash: record.channel_hash,
                    origin_hash: record.origin_hash,
                    payload,
                }),
            }
            return;
        }
        let reply = match rpc_wire::decode_reply_frame(payload.clone()) {
            Ok(Some(frame)) => rpc_wire::decode_route(&payload)
                .filter(|route| route_stream_id(*route) == record.stream_id)
                .map(|route| (frame, route)),
            // Well-formed but not the client half, or not an nRPC
            // frame at all.
            Ok(None) | Err(_) => None,
        };
        let Some((frame, reply_route)) = reply else {
            // A reply carrier is not an application surface: this
            // leaf subscribed to it to be answered, and an answer is
            // the only thing that rides it. A frame here that is not
            // one is refused out loud rather than handed to a
            // consumer as a channel message it cannot interpret.
            self.drop_counted(DropReason::UnknownCall);
            return;
        };
        // What a reply must present to be *this* call's reply: the
        // authenticated peer it arrived from, the incarnation of
        // that session, the canonical reply route the frame
        // declares, **and the channel that actually carried it**.
        // The route field alone is a self-declared claim — a
        // contacted peer can stamp the expected reply route inside
        // a RESPONSE it publishes on any channel at all, and
        // before this the pending call completed on that unrelated
        // carrier. Four facts now, all checked before the entry is
        // removed.
        //
        // **Forwarded logical origin, stated rather than
        // assumed.** The carrier is bound by the *stream id* the
        // reply channel derives, not by the publisher: an anchor
        // forwarding a service's RESPONSE is the transport anchor,
        // and the service's own `origin_hash` travels inside the
        // frame. So the transport peer proves who handed the frame
        // over, the carrier stream proves which channel it was
        // published on, and neither is treated as proof of who
        // *produced* it. Equating the transport anchor with the
        // logical publisher would break every routed call;
        // equating an inner claim with the carrier is what let the
        // wrong channel answer.
        //
        // **A reply nothing can be done with is refused out
        // loud.** Both arms below consume a payload that was
        // already acknowledged to its sender, so both count and
        // raise [`LeafEvent::Dropped`]: a counter alone is a rate
        // nobody is watching, and a post-acknowledgement discard
        // the consumer cannot observe is precisely how the loss
        // above stayed invisible for three rounds.
        let Some(incarnation) = self.sessions.get(peer).map(|s| s.incarnation()) else {
            self.drop_counted(DropReason::UnknownCall);
            return;
        };
        if !self.calls.deliver(
            frame,
            CallOwner {
                peer,
                incarnation,
                reply_route,
                carrier_stream_id: record.stream_id,
            },
            &self.counters,
        ) {
            // `deliver` owns the counter; this is the event half
            // of the same refusal.
            self.events.push(LeafEvent::Dropped {
                reason: DropReason::UnknownCall,
            });
        }
    }

    /// What `stream_id` is, for `peer`.
    ///
    /// The registry answers first: an id this leaf opened as a
    /// stream, or subscribed to / published on as a channel, is what
    /// it registered. For an id the leaf has never named — a stream
    /// or channel the peer originated — the derivation decides, and
    /// the discriminating fact is **bit 48**, not bit 49:
    /// `MeshNode::publish_stream_id` sets bit 48 on every channel
    /// publication, while `stream_id_from_label` masks bits 48 and
    /// above out of its hash before setting bit 49. Bit 49 alone is
    /// not a namespace — roughly half of all channel hashes already
    /// carry it, which is exactly how an ordinary subscribed channel
    /// came out as `StreamData`.
    fn classify(&self, peer: NodeId, stream_id: u64) -> StreamKind {
        if let Some(kind) = self.stream_kinds.get(&(peer, stream_id)) {
            return *kind;
        }
        if stream_id & crate::channel::PUBLISH_STREAM_DISCRIMINATOR != 0 {
            StreamKind::Channel
        } else if stream_id & crate::stream::LEAF_STREAM_DISCRIMINATOR != 0 {
            StreamKind::Stream
        } else {
            StreamKind::Channel
        }
    }

    /// A `0x0D02` envelope: verify against the announcement we hold
    /// for the sender, then admit it once.
    ///
    /// `true` means admitted and [`LeafEvent::Signal`] was pushed;
    /// `false` means refused, with [`LeafEvent::Dropped`] pushed and
    /// either [`DropReason::SignalRejected`] or, under replay-set
    /// capacity pressure, [`DropReason::SignalCapacityRefused`]
    /// counted.
    ///
    /// **Public because the control plane is the other carrier.**
    /// §9 routes signalling through
    /// [`ControlPlane::signal`](crate::control_plane::ControlPlane::signal),
    /// not the data path, so an envelope can arrive without ever
    /// passing through the dispatcher. One implementation serves
    /// both carriers: a parallel one behind the control plane would
    /// be weaker than this one the first time a step was forgotten,
    /// and the four steps are the whole security argument —
    ///
    /// 1. an announcement must exist for `envelope.from`, or there
    ///    is nothing to verify against and accepting would make the
    ///    carrier trusted (§5 Layer 1: key discovery precedes
    ///    signalling in both the native and the serverless world);
    /// 2. [`signal::verify`] — the signature, `to == self_node`,
    ///    and both ends of the `not_after` window;
    /// 3. [`SeenSignals::admit_checked`] — one
    ///    `(from, dialog, kind, payload)` per window, so an observer
    ///    cannot re-offer a still-valid envelope and restart an
    ///    abandoned dialog, and a full replay set refuses **new**
    ///    work rather than forgetting a record it still needs;
    /// 4. any failure counts and surfaces, never silently drops.
    ///
    /// `now` is accepted and unused: the window is wall-clock
    /// (`not_after` is unix seconds) while the caller's reading is
    /// monotonic. Keeping the parameter means the signature does
    /// not change when a caller starts threading one clock read
    /// through a batch.
    pub fn accept_signal(&mut self, envelope: SignalEnvelope, _now: Instant) -> bool {
        let now_secs = clock::now_unix_secs();
        let Some(announcement) = self.announcements.get(envelope.from) else {
            // Step 1.
            self.reject_signal();
            return false;
        };
        let Ok(entity_bytes) = unhex(&announcement.entity_id) else {
            self.reject_signal();
            return false;
        };
        let Ok(entity_id) = <[u8; 32]>::try_from(entity_bytes.as_slice()) else {
            self.reject_signal();
            return false;
        };
        // Step 2.
        if signal::verify(&envelope, &entity_id, self.identity.node_id(), now_secs).is_err() {
            self.reject_signal();
            return false;
        }
        // Step 3. The two refusals are different facts and are
        // counted apart: a replay is a thing this leaf has already
        // acted on, while a capacity refusal is an envelope nothing
        // is wrong with, refused because admitting it would mean
        // forgetting a replay record that is still inside its own
        // validity window.
        match self.seen_signals.admit_checked(&envelope, now_secs) {
            signal::SignalAdmission::Fresh => {}
            signal::SignalAdmission::Replay => {
                self.reject_signal();
                return false;
            }
            signal::SignalAdmission::AtCapacity => {
                self.drop_counted(DropReason::SignalCapacityRefused);
                return false;
            }
        }
        self.events.push(LeafEvent::Signal(envelope));
        true
    }

    fn reject_signal(&mut self) {
        self.drop_counted(DropReason::SignalRejected);
    }

    /// Refuse one arrival: count it **and** tell the consumer.
    ///
    /// Both halves, always, because they answer different
    /// questions — the counter is the rate, the event is the
    /// occurrence — and a refusal with only one of them is how a
    /// dropped payload goes unnoticed.
    fn drop_counted(&mut self, reason: DropReason) {
        self.counters.drop_for(reason);
        self.events.push(LeafEvent::Dropped { reason });
    }

    /// Sign an outbound signalling envelope for `peer`.
    ///
    /// The control plane carries it; no session with `peer` is
    /// needed, which is the whole point of the envelope.
    pub fn sign_signal(
        &self,
        peer: NodeId,
        dialog: u64,
        kind: crate::control_plane::SignalKind,
        payload: Vec<u8>,
    ) -> SignalEnvelope {
        signal::sign(
            &self.identity,
            peer,
            dialog,
            kind,
            payload,
            clock::now_unix_secs() + signal::MAX_SIGNAL_LIFETIME_SECS / 2,
        )
    }
}

/// The stream id an nRPC frame for `route` rides.
fn route_stream_id(route: u64) -> u64 {
    crate::channel::publish_stream_id(route)
}

/// Standard padded base64, which is what the TS wrapper decodes.
fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn json_string_or_null(s: Option<&str>) -> String {
    match s {
        Some(s) => json_string(s),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::SignalKind;
    use crate::identity::EntityKeypair;
    use crate::rpc_wire::{EventMeta, RpcStatus, DISPATCH_RPC_RESPONSE};
    use net_wire::crypto::{handshake_prologue, NoiseHandshake, StaticKeypair};
    use net_wire::parsed_packet::ParsedPacket;
    use net_wire::pool::PacketBuilder;
    use net_wire::protocol::{EventFrame, PacketFlags};
    use net_wire::session::NetSession;

    const ANCHOR: NodeId = 0xAAAA_BBBB_CCCC_DDDD;
    const PSK: [u8; 32] = [0x4B; 32];

    fn identity(seed: u8) -> LeafIdentity {
        LeafIdentity::from_secrets(EntityKeypair::from_secret([seed; 32]), [seed ^ 0xFF; 32])
    }

    fn anchor_static() -> StaticKeypair {
        let secret = x25519_dalek::StaticSecret::from([7u8; 32]);
        let public = x25519_dalek::PublicKey::from(&secret);
        StaticKeypair::from_keys([7u8; 32], *public.as_bytes())
    }

    /// A node with a live session against a real responder session,
    /// which is what lets the whole receive path run natively.
    fn connected() -> (LeafNode, NetSession) {
        let mut node = LeafNode::new(identity(0x11), 0x1000);
        let anchor_key = anchor_static();
        let prologue = handshake_prologue(
            crate::session::routing_id(node.node_id()),
            crate::session::routing_id(ANCHOR),
        );
        let mut responder = NoiseHandshake::responder_with_prologue(&PSK, &anchor_key, &prologue)
            .expect("responder");

        let msg1 = node
            .begin_handshake(ANCHOR, &PSK, anchor_key.public_key(), 0)
            .expect("msg1");
        let parsed = ParsedPacket::parse(msg1, rtc_addr(0, 1)).expect("parses");
        responder.read_message(&parsed.payload).expect("reads msg1");
        let msg2 = responder.write_message(&[]).expect("msg2");
        let msg2_packet = PacketBuilder::new(&[0u8; 32], 0).build_handshake(&msg2);

        node.set_peer_rtc_addr(ANCHOR, Some("198.51.100.7:4433".into()));
        node.complete_handshake(ANCHOR, &msg2_packet)
            .expect("install");
        let keys = responder.into_session_keys().expect("keys");
        (node, NetSession::new(keys, rtc_addr(1, 1), 2, false))
    }

    /// Build a packet the anchor would send, so the node's whole
    /// inbound path is exercised.
    fn anchor_packet(
        anchor: &NetSession,
        stream_id: u64,
        subprotocol_id: u16,
        channel_hash: u16,
        reliable: bool,
        payload: &[u8],
    ) -> Bytes {
        anchor.open_stream_with(stream_id, reliable, 1);
        let seq = anchor.get_or_create_stream(stream_id).next_tx_seq();
        let events = [Bytes::copy_from_slice(payload)];
        let mut builder = anchor.thread_local_pool().get();
        builder.set_channel_hash(channel_hash);
        builder.set_origin_hash(0xFEED_FACE_0000_0001);
        builder.build_subprotocol(
            stream_id,
            seq,
            &events,
            if reliable {
                PacketFlags::RELIABLE
            } else {
                PacketFlags::NONE
            },
            subprotocol_id,
        )
    }

    /// Decrypt one packet the leaf built, as the anchor would.
    fn decrypt(anchor: &NetSession, parsed: &ParsedPacket) -> Bytes {
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        anchor
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("the anchor decrypts")
    }

    #[test]
    fn the_handshake_emits_a_connected_event_carrying_the_published_rtc_addr() {
        let (mut node, _anchor) = connected();
        assert!(node.has_session(ANCHOR));
        let events = node.drain_events();
        assert_eq!(
            events,
            vec![LeafEvent::Connected {
                node_id: node.node_id(),
                peer_node: ANCHOR,
                rtc_addr: Some("198.51.100.7:4433".into()),
            }],
            "the rtc_addr is the datum the UdpBlocked correction needs"
        );
        let json = events[0].to_json();
        assert!(json.contains("\"type\":\"connected\""), "{json}");
        assert!(
            json.contains(&format!("\"node_id\":\"{}\"", node.node_id())),
            "u64s must cross as decimal strings: {json}"
        );
        assert!(
            json.contains("\"rtc_addr\":\"198.51.100.7:4433\""),
            "{json}"
        );
    }

    /// The full outbound path: a publish is one packet whose header
    /// carries the derived stream id and channel hint, and whose
    /// payload is the caller's bytes verbatim.
    #[test]
    fn a_publish_rides_the_derived_stream_and_carries_the_payload_verbatim() {
        let (mut node, anchor) = connected();
        node.publish(ANCHOR, "sensors/lidar", b"frame bytes")
            .expect("publish");
        let out = node.take_outbound();
        assert_eq!(out.len(), 1, "one Batch per packet");
        assert_eq!(out[0].peer, ANCHOR);

        let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        let channel = Channel::new("sensors/lidar").expect("valid");
        assert_eq!(parsed.header.stream_id, channel.publish_stream_id());
        assert_eq!(parsed.header.channel_hash, channel.wire_hash());
        assert_eq!(parsed.header.subprotocol_id, 0, "the event plane");
        assert_eq!(
            parsed.header.origin_hash,
            node.identity().origin_hash(),
            "the receiver maps this back to our node id"
        );

        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        let plain = anchor
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("the anchor decrypts");
        let events = EventFrame::read_events(plain, parsed.header.event_count);
        assert_eq!(&events[0][..], b"frame bytes", "no added framing");
    }

    #[test]
    fn a_subscribe_goes_out_under_the_membership_subprotocol() {
        let (mut node, _anchor) = connected();
        let nonce = node
            .subscribe(ANCHOR, "net.mesh.enroll.replies.0000000000000001")
            .expect("subscribe");
        assert_eq!(nonce, 1);
        let out = node.take_outbound();
        let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(parsed.header.subprotocol_id, 0x0A00);
    }

    /// A call goes out as `EventMeta ‖ route ‖ payload` and its reply
    /// resolves the caller's future.
    #[test]
    fn a_call_round_trips_and_resolves_the_callers_future() {
        let (mut node, anchor) = connected();
        let mut rx = node
            .call(ANCHOR, "net.mesh.enroll", b"join request", Some(5_000))
            .expect("call");
        let out = node.take_outbound();
        // TWO packets: the reply-channel Subscribe, then the
        // REQUEST. MergedRunner's defect 1 was this first packet
        // missing — the anchor forwards a RESPONSE only to
        // subscribers, so the handler ran and the reply stranded,
        // and the caller saw a bare deadline.
        assert_eq!(out.len(), 2, "a first call must subscribe before it asks");
        let subscribe = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(
            subscribe.header.subprotocol_id, 0x0A00,
            "the membership frame must be ordered ahead of the REQUEST"
        );

        // Read the call_id off the wire, exactly as the anchor would.
        let parsed = ParsedPacket::parse(out[1].packet.clone(), rtc_addr(0, 1)).expect("parses");
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
        let plain = anchor
            .rx_cipher()
            .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
            .expect("decrypts");
        let frames = EventFrame::read_events(plain, parsed.header.event_count);
        let meta = EventMeta::from_bytes(&frames[0]).expect("meta");
        assert_eq!(meta.dispatch, crate::rpc_wire::DISPATCH_RPC_REQUEST);
        let request_channel = Channel::new("net.mesh.enroll.requests").expect("valid");
        assert_eq!(
            rpc_wire::decode_route(&frames[0]),
            Some(request_channel.canonical()),
            "the route discriminator selects exactly one dispatcher"
        );
        assert_eq!(parsed.header.stream_id, request_channel.publish_stream_id());

        // The anchor answers on the reply channel.
        let reply_name = node.reply_channel_for("net.mesh.enroll").expect("name");
        let reply = Channel::new(&reply_name).expect("valid");
        let mut response = Vec::new();
        response.extend_from_slice(
            &EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 1, meta.seq_or_ts, 0).to_bytes(),
        );
        response.extend_from_slice(&reply.canonical().to_le_bytes());
        response.extend_from_slice(&RpcStatus::Ok.to_wire().to_le_bytes());
        response.push(0);
        response.extend_from_slice(&4u32.to_le_bytes());
        response.extend_from_slice(b"NMO1");

        let packet = anchor_packet(
            &anchor,
            reply.publish_stream_id(),
            0,
            reply.wire_hash(),
            true,
            &response,
        );
        node.on_datagram(ANCHOR, packet, clock::now());

        let outcome = rx
            .try_recv()
            .expect("the sender is alive")
            .expect("resolved");
        assert_eq!(outcome.expect("Ok"), Bytes::from_static(b"NMO1"));
        assert_eq!(node.counters().total_drops(), 0);
    }

    /// **The cadence, which is the other half of defect 1's fix.**
    ///
    /// Subscribing per call would be correct and unusable: a
    /// membership frame per call is traffic §2's admission should
    /// refuse under load. Subscribing once and never again after a
    /// reconnect would be the original bug with extra steps, because
    /// the anchor's roster entry dies with the session. Both
    /// properties in one test.
    #[test]
    fn the_reply_channel_is_subscribed_once_per_service_and_again_after_a_reconnect() {
        let (mut node, _anchor) = connected();
        node.drain_events();

        let _first = node
            .call(ANCHOR, "app.orders", b"a", Some(60_000))
            .expect("call");
        assert_eq!(
            node.take_outbound().len(),
            2,
            "the first call subscribes then asks"
        );

        let _second = node
            .call(ANCHOR, "app.orders", b"b", Some(60_000))
            .expect("call");
        assert_eq!(
            node.take_outbound().len(),
            1,
            "a second call on the same service must NOT re-subscribe"
        );

        // A different service is a different reply channel.
        let _other = node
            .call(ANCHOR, "app.invoices", b"c", Some(60_000))
            .expect("call");
        assert_eq!(
            node.take_outbound().len(),
            2,
            "each service has its own reply channel"
        );

        // The session dies: the anchor's roster entry died with it,
        // so a reconnect must re-subscribe or every later reply
        // strands silently.
        node.drop_session(ANCHOR, "channel closed");
        node.drain_events();
        let (mut node, _anchor) = connected();
        node.drain_events();
        let _after = node
            .call(ANCHOR, "app.orders", b"d", Some(60_000))
            .expect("call");
        assert_eq!(
            node.take_outbound().len(),
            2,
            "a reconnected session must subscribe again"
        );
    }

    /// The §8 rule end to end: losing the session fails the call
    /// typed, and does not re-issue it.
    #[test]
    fn losing_the_session_fails_an_in_flight_call_typed() {
        let (mut node, _anchor) = connected();
        let mut rx = node
            .call(ANCHOR, "net.mesh.enroll", b"join", Some(60_000))
            .expect("call");
        node.take_outbound();

        node.drop_session(ANCHOR, "the DataChannel closed");
        assert_eq!(
            rx.try_recv().expect("alive").expect("resolved"),
            Err(RpcError::SessionLost)
        );
        assert!(!node.has_session(ANCHOR));
        assert!(
            node.take_outbound().is_empty(),
            "a lost session must never re-issue the request"
        );
    }

    #[test]
    fn a_deadline_sweep_times_out_the_call_and_emits_a_cancel() {
        let (mut node, _anchor) = connected();
        let mut rx = node
            .call(ANCHOR, "net.mesh.enroll", b"join", Some(10))
            .expect("call");
        node.take_outbound();

        let later = clock::now() + core::time::Duration::from_millis(50);
        assert_eq!(node.tick(later), 1);
        assert_eq!(
            rx.try_recv().expect("alive").expect("resolved"),
            Err(RpcError::Timeout)
        );
        let cancels = node.take_outbound();
        assert_eq!(
            cancels.len(),
            1,
            "the server must be told, not left running work nobody awaits"
        );
    }

    /// An over-cap payload fragments on the way out and reassembles
    /// on the way in — the S0c decision, end to end through a real
    /// session pair.
    #[test]
    fn an_over_cap_payload_survives_fragmentation_and_reassembly() {
        let (mut node, anchor) = connected();
        let big: Vec<u8> = (0..crate::frame::MAX_FRAGMENT_PAYLOAD * 2 + 7)
            .map(|i| (i % 251) as u8)
            .collect();
        node.publish(ANCHOR, "bulk/frames", &big).expect("publish");
        let out = node.take_outbound();
        assert_eq!(out.len(), 3, "three packets, none over the cap");
        assert_eq!(node.counters().drops(DropReason::Unparsable), 0);

        // Decrypt each and reassemble on the anchor side using the
        // leaf's own reassembler, which is the receiving half a
        // native node still lacks (named in the report).
        let c = LeafCounters::new();
        let mut reassembler = Reassembler::new();
        let mut whole = None;
        for packet in out {
            let parsed = ParsedPacket::parse(packet.packet, rtc_addr(0, 1)).expect("parses");
            assert!(parsed.header.validate(), "no packet may fail validate()");
            let aad = parsed.header.aad();
            let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().expect("nonce"));
            let plain = anchor
                .rx_cipher()
                .decrypt_to_bytes(counter, &aad, parsed.payload.clone())
                .expect("decrypts");
            let events = EventFrame::read_events(plain, parsed.header.event_count);
            whole = reassembler.accept(
                ANCHOR,
                parsed.header.fragment_id,
                parsed.header.fragment_offset,
                parsed.header.frag_flags,
                events[0].clone(),
                clock::now(),
                &c,
            );
        }
        assert_eq!(whole.expect("completes").as_ref(), &big[..]);
    }

    /// The non-forwarding role, observed on the node rather than in
    /// the dispatcher unit test.
    #[test]
    fn a_routed_envelope_for_someone_else_is_dropped_with_a_counter() {
        use net_wire::route_codec::{RoutingHeader, _MAX_TTL};
        let (mut node, anchor) = connected();
        node.drain_events(); // the Connected event
        let inner = anchor_packet(&anchor, 1, 0, 0, true, b"not for us");
        let header = RoutingHeader::new(0x1234_5678, node.node_id() as u32, _MAX_TTL);
        let mut wire = header.to_bytes().to_vec();
        wire.extend_from_slice(&inner);

        node.on_datagram(ANCHOR, Bytes::from(wire), clock::now());
        assert_eq!(node.counters().drops(DropReason::NotAddressedToUs), 1);
        assert!(
            node.drain_events().is_empty(),
            "a forwarded packet produces no application event"
        );
    }

    // ──────── the relayed leaf ↔ leaf path (plan §9 steps 2–3) ──────

    /// The peer that forwards for a pair with no channel of their
    /// own. Not a party to either session, which is the point.
    const RELAY: NodeId = 0x9999_8888_7777_6666;

    /// Two leaves that have **discovered** each other and nothing
    /// more: each holds the other's verified announcement, neither
    /// holds a session, and neither has been told anything by a
    /// carrier.
    fn discovered() -> (LeafNode, LeafNode) {
        let mut a = LeafNode::new(identity(0x71), 11);
        let mut b = LeafNode::new(identity(0x72), 12);
        let from_a = a
            .build_announcement(&["chat".to_string()])
            .expect("a signs its own announcement");
        let from_b = b
            .build_announcement(&["chat".to_string()])
            .expect("b signs its own announcement");
        assert!(b.ingest_announcement(&from_a), "b verifies a's");
        assert!(a.ingest_announcement(&from_b), "a verifies b's");
        a.drain_events();
        b.drain_events();
        (a, b)
    }

    /// Move everything `from` queued for `peer` to `to`, exactly as a
    /// blind relay would: the bytes are forwarded verbatim, and the
    /// relay is told nothing and asked nothing.
    ///
    /// Returns how many packets crossed.
    fn forward(from: &mut LeafNode, to: &mut LeafNode) -> usize {
        let out = from.take_outbound();
        let n = out.len();
        for packet in out {
            assert_eq!(
                packet.peer, RELAY,
                "a relayed packet leaves addressed to the relay, not to its destination"
            );
            match to.classify_datagram(RELAY, packet.packet) {
                Inbound::Session {
                    from: src,
                    packet,
                    relayed,
                } => {
                    assert!(relayed, "it arrived through the relay");
                    to.on_datagram(src, packet, clock::now());
                }
                other => panic!("expected a session packet, got {other:?}"),
            }
        }
        n
    }

    /// §9 step 2: the routed A↔B session, over a relay that never
    /// sees inside it.
    ///
    /// Everything the pair needs comes from discovery: B's Noise
    /// static from its signed announcement, and A's full node id
    /// resolved from B's copy of A's announcement — the routing
    /// header carries only a 32-bit projection, so this is the step
    /// that would be impossible if either side had skipped Layer 1.
    #[test]
    fn a_relayed_handshake_installs_a_session_on_both_sides_from_discovery_alone() {
        let (mut a, mut b) = discovered();
        let (aid, bid) = (a.node_id(), b.node_id());
        // From discovery, and from nowhere else: this is the datum
        // first contact genuinely requires, and it is read out of the
        // announcement A verified itself.
        let b_noise = a
            .announcement_for(bid)
            .expect("a discovered b")
            .noise_pubkey
            .expect("a leaf's announcement carries its noise_pubkey");

        a.set_peer_relay(bid, RELAY);
        let msg1 = a.begin_handshake(bid, &PSK, &b_noise, 0).expect("msg1");
        let out = a.route_outbound(bid, msg1);
        assert_eq!(out.peer, RELAY);

        // A leaf that is merely carrying this refuses it: not
        // addressed to us, not forwarded, counted.
        let carrier = LeafNode::new(identity(0x73), 13);
        assert_eq!(
            carrier.classify_datagram(aid, out.packet.clone()),
            Inbound::Refused,
            "the relay is not the destination, and a leaf never forwards"
        );
        assert_eq!(carrier.counters().drops(DropReason::NotAddressedToUs), 1);

        let msg2 = match b.classify_datagram(RELAY, out.packet) {
            Inbound::Handshake {
                from,
                packet,
                relayed,
            } => {
                assert_eq!(
                    from, aid,
                    "the sender is resolved from the announcement b verified, \
                     never from the carrier"
                );
                assert!(relayed);
                b.set_peer_relay(from, RELAY);
                b.accept_handshake(from, &PSK, &packet, 0)
                    .expect("the responder half completes on message 1")
            }
            other => panic!("expected message 1, got {other:?}"),
        };

        let back = b.route_outbound(aid, msg2);
        assert_eq!(back.peer, RELAY);
        match a.classify_datagram(RELAY, back.packet) {
            Inbound::Handshake {
                from,
                packet,
                relayed,
            } => {
                assert_eq!(from, bid);
                assert!(relayed);
                a.complete_handshake(from, &packet)
                    .expect("the initiator installs from message 2");
            }
            other => panic!("expected message 2, got {other:?}"),
        }

        assert!(a.has_session(bid) && b.has_session(aid));
        assert_eq!(a.peer_relay(bid), Some(RELAY));
        assert_eq!(b.peer_relay(aid), Some(RELAY));
    }

    /// §9 step 3: the signed `0x0D02` envelope rides that session,
    /// and the receiver verifies it on its own.
    #[test]
    fn a_signed_offer_rides_the_relayed_session_and_the_receiver_verifies_it() {
        let (mut a, mut b) = relayed_pair();
        let (aid, bid) = (a.node_id(), b.node_id());

        let envelope = a.sign_signal(
            bid,
            0x5109,
            SignalKind::Offer,
            b"v=0\r\no=- 1 1 IN".to_vec(),
        );
        a.send_signal_frame(bid, &envelope)
            .expect("the offer goes out on the relayed session");
        assert_eq!(forward(&mut a, &mut b), 1);

        assert_eq!(
            b.drain_events(),
            vec![LeafEvent::Signal(envelope)],
            "the envelope arrives verified: signature against a's announcement, \
             inside its window, unreplayed"
        );
        assert_eq!(b.counters().drops(DropReason::SignalRejected), 0);
        assert_eq!(a.node_id(), aid);
    }

    /// Application data takes the same relayed path — which is what
    /// §10 part 1 measures on the anchor, and what part 2 then
    /// expects to go flat.
    #[test]
    fn application_data_rides_the_relayed_session_and_stops_when_the_pair_goes_direct() {
        let (mut a, mut b) = relayed_pair();
        let bid = b.node_id();

        let handle = a
            .open_stream(bid, "positions", Reliability::FireAndForget, None, None)
            .expect("stream");
        a.stream_send(handle, b"frame one").expect("send");
        assert_eq!(forward(&mut a, &mut b), 1, "it went through the relay");

        // §9 step 4's addressing half: the pair now has its own
        // channel, so nothing more is wrapped.
        assert!(a.clear_peer_relay(bid));
        a.stream_send(handle, b"frame two").expect("send");
        let out = a.take_outbound();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].peer, bid,
            "a direct packet is addressed to the peer, with no routing envelope"
        );
        assert_eq!(
            u16::from_le_bytes([out[0].packet[0], out[0].packet[1]]),
            net_wire::protocol::MAGIC,
            "and it carries the Net magic, not the routing magic"
        );
    }

    /// A relayed source this leaf cannot resolve is refused, not
    /// guessed at.
    ///
    /// The fail-closed half of "keys from discovery only": with no
    /// verified announcement for the projection a packet claims,
    /// there is no key to try and no identity to key a session
    /// under, so the frame is dropped and counted.
    #[test]
    fn a_relayed_packet_from_an_undiscovered_source_is_refused() {
        let (mut a, _b) = discovered();
        let stranger = LeafNode::new(identity(0x7E), 14);
        let aid = a.node_id();

        let header = RoutingHeader::new(aid, stranger.node_id() as u32, RELAY_TTL);
        let mut wire = header.to_bytes().to_vec();
        wire.extend_from_slice(&[0u8; net_wire::protocol::HEADER_SIZE]);
        assert_eq!(
            a.classify_datagram(RELAY, Bytes::from(wire)),
            Inbound::Refused
        );
        assert_eq!(a.counters().drops(DropReason::NoSession), 1);
        assert!(
            a.drain_events().is_empty(),
            "an unresolvable relayed packet produces no application event"
        );
    }

    /// Two leaves with a live relayed session, the state §9 step 3
    /// starts from.
    fn relayed_pair() -> (LeafNode, LeafNode) {
        let (mut a, mut b) = discovered();
        let (aid, bid) = (a.node_id(), b.node_id());
        let b_noise = a
            .announcement_for(bid)
            .expect("discovered")
            .noise_pubkey
            .expect("noise key");
        a.set_peer_relay(bid, RELAY);
        b.set_peer_relay(aid, RELAY);
        let msg1 = a.begin_handshake(bid, &PSK, &b_noise, 0).expect("msg1");
        let wrapped = a.route_outbound(bid, msg1);
        let msg2 = match b.classify_datagram(RELAY, wrapped.packet) {
            Inbound::Handshake { from, packet, .. } => b
                .accept_handshake(from, &PSK, &packet, 0)
                .expect("responder"),
            other => panic!("{other:?}"),
        };
        let wrapped = b.route_outbound(aid, msg2);
        match a.classify_datagram(RELAY, wrapped.packet) {
            Inbound::Handshake { from, packet, .. } => {
                a.complete_handshake(from, &packet).expect("initiator");
            }
            other => panic!("{other:?}"),
        }
        a.drain_events();
        b.drain_events();
        (a, b)
    }

    #[test]
    fn an_unknown_subprotocol_from_a_live_session_is_dropped_and_surfaced() {
        let (mut node, anchor) = connected();
        node.drain_events(); // the Connected event
        let packet = anchor_packet(&anchor, 1, 0x0F00, 0, true, b"meshdb frame");
        node.on_datagram(ANCHOR, packet, clock::now());
        assert_eq!(node.counters().drops(DropReason::UnknownSubprotocol), 1);
        assert_eq!(
            node.drain_events(),
            vec![LeafEvent::Dropped {
                reason: DropReason::UnknownSubprotocol
            }]
        );
    }

    #[test]
    fn a_packet_from_a_peer_with_no_session_is_dropped() {
        let (mut node, anchor) = connected();
        let packet = anchor_packet(&anchor, 1, 0, 0, true, b"hello");
        node.on_datagram(0xDEAD, packet, clock::now());
        assert_eq!(node.counters().drops(DropReason::NoSession), 1);
    }

    /// An announcement must verify before it can answer a query.
    #[test]
    fn query_answers_only_from_verified_announcements() {
        let (mut node, anchor) = connected();
        let peer = identity(0x22);
        let signed = announce::build_announcement(
            &peer,
            &["gpu".to_string()],
            1,
            clock::now_unix_nanos(),
            300,
        )
        .expect("build");

        // Tampered: refused, counted, and invisible to query.
        let mut tampered = signed.clone();
        let at = tampered.len() / 2;
        tampered[at] ^= 0x01;
        let packet = anchor_packet(&anchor, 0x1000, 0x1000, 0, true, &tampered);
        node.on_datagram(ANCHOR, packet, clock::now());
        assert_eq!(node.counters().drops(DropReason::AnnouncementUnverified), 1);
        assert_eq!(node.query("gpu"), "[]");

        // Genuine: ingested and queryable.
        let packet = anchor_packet(&anchor, 0x1000, 0x1000, 0, true, &signed);
        node.on_datagram(ANCHOR, packet, clock::now());
        assert!(
            node.query("gpu").contains(&peer.node_id().to_string()),
            "a verified announcement must answer its capability"
        );
        assert!(node.announcement_for(peer.node_id()).is_some());
    }

    /// A leaf's own announcement carries the role tags and no
    /// `reflex_addr`, and its version advances.
    #[test]
    fn the_leafs_announcement_is_versioned_and_role_tagged() {
        let (mut node, _anchor) = connected();
        let first = node
            .build_announcement(&["stage5.browser".into()])
            .expect("build");
        let verified = announce::verify_announcement(&first).expect("verifies");
        assert_eq!(verified.version, 1, "the fold rejects generation 0");
        assert!(verified.capabilities.contains(&"leaf".to_string()));
        assert!(verified.capabilities.contains(&"transport:rtc".to_string()));
        assert_eq!(verified.rtc_addr, None);

        let second = node.build_announcement(&[]).expect("build");
        assert_eq!(
            announce::verify_announcement(&second)
                .expect("verifies")
                .version,
            2,
            "each announcement must supersede the last"
        );

        node.announce_to_peer(ANCHOR, &first).expect("send");
        let out = node.take_outbound();
        let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(
            parsed.header.subprotocol_id, SUBPROTOCOL_CAPABILITY_ANN,
            "an announcement must ride 0x0C00: that is the subprotocol whose handler \
             verifies it and feeds the receiver's capability index. 0x1000 reaches the \
             fold router instead, which refuses a bare announcement document — the \
             anchor then resolves nothing for this node and says so only at debug level"
        );
    }

    /// Signalling: verified against the sender's announcement, and
    /// admitted once.
    #[test]
    fn a_signal_envelope_needs_a_verified_announcement_and_is_admitted_once() {
        let (mut node, anchor) = connected();
        let peer = identity(0x33);
        let envelope = signal::sign(
            &peer,
            node.node_id(),
            0x99,
            SignalKind::Offer,
            b"v=0".to_vec(),
            clock::now_unix_secs() + 10,
        );
        let bytes = signal::encode(&envelope).expect("encode");

        // No announcement for the sender: refused.
        let packet = anchor_packet(&anchor, 0x0D02, 0x0D02, 0, true, &bytes);
        node.on_datagram(ANCHOR, packet, clock::now());
        assert_eq!(node.counters().drops(DropReason::SignalRejected), 1);

        // With its announcement ingested, the same envelope lands.
        let signed = announce::build_announcement(&peer, &[], 1, clock::now_unix_nanos(), 300)
            .expect("build");
        assert!(node.ingest_announcement(&signed));
        node.drain_events();
        let packet = anchor_packet(&anchor, 0x0D02, 0x0D02, 0, true, &bytes);
        node.on_datagram(ANCHOR, packet, clock::now());
        assert_eq!(
            node.drain_events(),
            vec![LeafEvent::Signal(envelope.clone())],
            "a verified envelope reaches the application"
        );

        // Replayed: refused by the seen-set.
        let packet = anchor_packet(&anchor, 0x0D02, 0x0D02, 0, true, &bytes);
        node.on_datagram(ANCHOR, packet, clock::now());
        assert_eq!(node.counters().drops(DropReason::SignalRejected), 2);
    }

    /// **The bug MergedRunner found, as a regression test.**
    ///
    /// Before this, `connect()` completed the handshake and stopped,
    /// so the anchor left the peer PROVISIONAL and §12 refused every
    /// call, publish and announcement above the transport — which
    /// reached the caller as nothing at all and then a deadline.
    /// This asserts the exchange that promotes the session: exactly
    /// two packets, in order, with the shapes §12's allow-list
    /// admits.
    #[test]
    fn enrollment_emits_the_subscribe_then_the_request_and_promotes_on_admission() {
        let (mut node, anchor) = connected();
        node.drain_events();
        assert!(
            !node.is_enrolled(),
            "a freshly handshaken session is provisional"
        );

        let invite = crate::enroll::Invite {
            root: [0x77; 32],
            nonce: [0x5A; 16],
            expires_at: clock::now_unix_secs() + 600,
            rendezvous: "https://anchor.example/rtc".into(),
        };
        let mut rx = node
            .begin_enrollment(ANCHOR, &invite, "chrome-tab", &[], Some(5_000))
            .expect("begin enrollment");

        let out = node.take_outbound();
        assert_eq!(
            out.len(),
            2,
            "enrollment is a Subscribe then a REQUEST — the anchor cannot \
             publish the reply before our roster entry exists"
        );

        // Packet 1: the 0x0A00 Subscribe, on OUR origin-bound reply
        // channel, with no token and no queue group. Anything else
        // and §12 refuses it.
        let first = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(first.header.subprotocol_id, 0x0A00);
        let expected_reply = format!(
            "net.mesh.enroll.replies.{:016x}",
            node.identity().origin_hash()
        );
        let reply = Channel::new(&expected_reply).expect("valid");
        assert_eq!(first.header.channel_hash, reply.wire_hash());
        let plain = decrypt(&anchor, &first);
        let events = EventFrame::read_events(plain, first.header.event_count);
        match net_wire::channel::membership::decode(&events[0]).expect("decodes") {
            net_wire::channel::membership::MembershipMsg::Subscribe {
                channel,
                token,
                queue_group,
                ..
            } => {
                assert_eq!(channel.as_str(), expected_reply);
                assert!(token.is_none(), "§12 refuses a Subscribe carrying a token");
                assert!(
                    queue_group.is_none(),
                    "§12 refuses a Subscribe carrying a queue group"
                );
            }
            other => panic!("expected a Subscribe, got {other:?}"),
        }

        // Packet 2: the nRPC REQUEST for net.mesh.enroll, whose body
        // is a signed JoinRequest under the 16 KiB bound.
        let second = ParsedPacket::parse(out[1].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(second.header.subprotocol_id, 0, "the event plane");
        let plain = decrypt(&anchor, &second);
        let frames = EventFrame::read_events(plain, second.header.event_count);
        let meta = EventMeta::from_bytes(&frames[0]).expect("meta");
        assert_eq!(meta.dispatch, crate::rpc_wire::DISPATCH_RPC_REQUEST);
        assert_eq!(
            meta.origin_hash,
            node.identity().origin_hash(),
            "§12 derives the permitted reply channel from THIS field"
        );
        let body = &frames[0][crate::rpc_wire::RPC_FRAME_BODY_OFFSET..];
        let service_len = body[0] as usize;
        assert_eq!(
            &body[1..1 + service_len],
            crate::enroll::ENROLL_SERVICE.as_bytes(),
            "the one service a provisional session may call"
        );

        // The anchor admits, on the reply channel.
        let mut outcome = Vec::new();
        outcome.extend_from_slice(b"NMO1");
        outcome.push(0);
        outcome.extend_from_slice(&(11u32).to_le_bytes());
        outcome.extend_from_slice(b"chain-bytes");

        let mut response = Vec::new();
        response.extend_from_slice(
            &EventMeta::new(DISPATCH_RPC_RESPONSE, 0, 1, meta.seq_or_ts, 0).to_bytes(),
        );
        response.extend_from_slice(&reply.canonical().to_le_bytes());
        response.extend_from_slice(&RpcStatus::Ok.to_wire().to_le_bytes());
        response.push(0);
        response.extend_from_slice(&(outcome.len() as u32).to_le_bytes());
        response.extend_from_slice(&outcome);

        let packet = anchor_packet(
            &anchor,
            reply.publish_stream_id(),
            0,
            reply.wire_hash(),
            true,
            &response,
        );
        node.on_datagram(ANCHOR, packet, clock::now());

        let body = rx
            .try_recv()
            .expect("alive")
            .expect("resolved")
            .expect("admitted");
        let chain = node.finish_enrollment(&body).expect("admitted");
        assert_eq!(chain, b"chain-bytes".to_vec());
        assert!(
            node.is_enrolled(),
            "an admitted outcome must promote the session out of provisional"
        );
    }

    /// A refusal is typed and does not promote. The four witnesses
    /// this bug reddened all read as deadlines; a rejection must
    /// read as a rejection.
    #[test]
    fn a_rejected_enrollment_is_typed_and_does_not_promote() {
        let (mut node, _anchor) = connected();
        node.drain_events();
        let mut outcome = Vec::new();
        outcome.extend_from_slice(b"NMO1");
        outcome.push(1);
        outcome.extend_from_slice(&5u16.to_le_bytes()); // REPLAY
        outcome.extend_from_slice(&(6u32).to_le_bytes());
        outcome.extend_from_slice(b"redone");

        let err = node
            .finish_enrollment(&outcome)
            .expect_err("a rejection is an error, not a silent continue");
        let text = format!("{err}");
        assert!(text.contains("replay"), "{text}");
        assert!(text.contains("redone"), "{text}");
        assert!(!node.is_enrolled());
    }

    /// An expired invite is refused before anything hits the wire,
    /// so the failure names the invite instead of arriving as a
    /// rejection minutes later.
    #[test]
    fn an_expired_invite_is_refused_before_a_packet_is_built() {
        let (mut node, _anchor) = connected();
        node.drain_events();
        let invite = crate::enroll::Invite {
            root: [0x77; 32],
            nonce: [0x5A; 16],
            expires_at: 1,
            rendezvous: String::new(),
        };
        let err = node
            .begin_enrollment(ANCHOR, &invite, "tab", &[], None)
            .expect_err("an expired invite must be refused");
        assert!(format!("{err}").contains("expired"), "{err}");
        assert!(
            node.take_outbound().is_empty(),
            "nothing may reach the wire for an invite that cannot be redeemed"
        );
    }

    /// **A far-end test, added because the rule earned it.**
    ///
    /// `open_stream`'s `stream_id` / `channel_hash` overrides exist
    /// for one reason: without them a fire-and-forget stream's
    /// payload never reaches a native node's real handler, because
    /// the anchor dispatches on the stream id and channel hint the
    /// publish contract derives — so the witness would observe a
    /// counter instead of a handler. Asserting the returned
    /// `StreamHandle` would be a NEAR-end test and would pass even
    /// if the values were dropped on the way to the header. This
    /// asserts them where they have to arrive: on the wire.
    #[test]
    fn stream_overrides_arrive_on_the_packet_header_not_just_on_the_handle() {
        let (mut node, anchor) = connected();
        node.drain_events();

        let handle = node
            .open_stream(
                ANCHOR,
                "ignored-when-overridden",
                Reliability::FireAndForget,
                Some(0x0001_0000_DEAD_BEEF),
                Some(0xC0DE),
            )
            .expect("open");
        node.stream_send(handle, b"loss-witness payload")
            .expect("send");

        let out = node.take_outbound();
        assert_eq!(out.len(), 1);
        let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(
            parsed.header.stream_id, 0x0001_0000_DEAD_BEEF,
            "the caller's stream id must ride verbatim, or the anchor never \
             dispatches the frame"
        );
        assert_eq!(
            parsed.header.channel_hash, 0xC0DE,
            "the caller's channel hint must ride verbatim"
        );
        assert!(
            !parsed.header.flags.is_reliable(),
            "fire-and-forget must clear the wire RELIABLE flag, or injected \
             loss is retransmitted away and the witness passes for the \
             wrong reason"
        );
        let plain = decrypt(&anchor, &parsed);
        let events = EventFrame::read_events(plain, parsed.header.event_count);
        assert_eq!(
            &events[0][..],
            b"loss-witness payload",
            "the payload rides as one event with no leaf-added framing"
        );

        // And with no overrides, the derivation applies instead —
        // the bit-49 space that cannot alias a publish stream.
        let derived = node
            .open_stream(ANCHOR, "app/telemetry", Reliability::Reliable, None, None)
            .expect("open");
        node.stream_send(derived, b"x").expect("send");
        let out = node.take_outbound();
        let parsed = ParsedPacket::parse(out[0].packet.clone(), rtc_addr(0, 1)).expect("parses");
        assert_eq!(
            parsed.header.stream_id,
            crate::stream::stream_id_from_label("app/telemetry")
        );
        assert!(parsed.header.flags.is_reliable());
    }

    #[test]
    fn the_event_json_keeps_every_u64_as_a_decimal_string() {
        let event = LeafEvent::StreamData {
            stream_id: u64::MAX,
            seq: 9_007_199_254_740_993,
            payload: Bytes::from_static(b"\x00\xFF"),
        };
        let json = event.to_json();
        assert!(
            json.contains(&format!("\"stream_id\":\"{}\"", u64::MAX)),
            "{json}"
        );
        assert!(json.contains("\"seq\":\"9007199254740993\""), "{json}");
        assert!(json.contains("\"payload\":\"AP8=\""), "base64: {json}");
    }

    /// Two real leaves with a session installed on both sides.
    fn pair() -> (LeafNode, LeafNode) {
        let mut a = LeafNode::new(identity(0x61), 1);
        let mut b = LeafNode::new(identity(0x62), 2);
        let (aid, bid) = (a.node_id(), b.node_id());
        let msg1 = a
            .begin_handshake(bid, &PSK, b.identity().noise().public_key(), 10)
            .expect("msg1");
        let msg2 = b.accept_handshake(aid, &PSK, &msg1, 10).expect("msg2");
        a.complete_handshake(bid, &msg2).expect("install");
        a.drain_events();
        b.drain_events();
        (a, b)
    }

    /// Move everything `from` has queued into `to`.
    fn pump(from: &mut LeafNode, to: &mut LeafNode) -> usize {
        let id = from.node_id();
        let out = from.take_outbound();
        let n = out.len();
        for packet in out {
            to.on_datagram(id, packet.packet, clock::now());
        }
        n
    }

    fn delivered(node: &mut LeafNode) -> Vec<Vec<u8>> {
        node.drain_events()
            .into_iter()
            .filter_map(|e| match e {
                LeafEvent::StreamData { payload, .. } => Some(payload.to_vec()),
                _ => None,
            })
            .collect()
    }

    /// **R3's recovery witness.** One reliable packet is deliberately
    /// lost and the two that follow it arrive out of order. The wire
    /// machinery has to do all of it: the sender must have retained a
    /// retransmit descriptor, the receiver must notice the gap and
    /// ask for it, and the sender must rebuild the packet with a
    /// fresh AEAD counter — a replayed ciphertext is refused by the
    /// replay window, so a "retransmit" that resent the bytes would
    /// prove nothing.
    #[test]
    fn a_lost_reliable_packet_is_nacked_retransmitted_and_delivered_in_order() {
        let (mut a, mut b) = pair();
        let bid = b.node_id();
        let handle = a
            .open_stream(
                bid,
                "recovery",
                Reliability::Reliable,
                Some(crate::stream::LEAF_STREAM_DISCRIMINATOR | 9),
                Some(9),
            )
            .expect("open");
        for body in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            a.stream_send(handle, body).expect("send");
        }
        let packets = a.take_outbound();
        assert_eq!(packets.len(), 3);
        assert!(
            a.sessions.get(bid).expect("session").wire().has_unacked(),
            "a reliable send must leave a retransmit owner behind"
        );

        // Loss and reorder: 2 arrives first, 0 second, 1 never.
        let aid = a.node_id();
        b.on_datagram(aid, packets[2].packet.clone(), clock::now());
        b.on_datagram(aid, packets[0].packet.clone(), clock::now());
        assert_eq!(
            delivered(&mut b),
            vec![b"one".to_vec()],
            "the reorder buffer must hold 2 behind the missing 1"
        );

        // The receiver asks for the hole; the sender rebuilds it.
        b.tick(clock::now());
        assert!(pump(&mut b, &mut a) > 0, "the gap must produce a NACK");
        assert!(
            pump(&mut a, &mut b) > 0,
            "the NACK must produce a retransmit"
        );

        assert_eq!(
            delivered(&mut b),
            vec![b"two".to_vec(), b"three".to_vec()],
            "recovery must release the held sequence too, in order"
        );
        assert!(
            !b.drain_events()
                .iter()
                .any(|e| matches!(e, LeafEvent::StreamFailed { .. })),
            "the hole was recovered, so the stream must not have been failed"
        );
    }

    /// **Admission before acknowledgement.** A fragment the
    /// reassembler has no slot for must stay the sender's to send
    /// again. Fill the reassembly table with partial groups, offer a
    /// ninth message's head into the refusal, and require that the
    /// ordinary retransmit still delivers it once the slots free —
    /// which it can only do if the refused sequence was never
    /// acknowledged.
    #[test]
    fn a_fragment_refused_for_capacity_is_recovered_by_the_ordinary_retransmit() {
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let filler = vec![0x11u8; crate::frame::MAX_FRAGMENT_PAYLOAD + 1];
        let wanted = vec![0x22u8; crate::frame::MAX_FRAGMENT_PAYLOAD + 1];

        // Every group the table holds, opened by a head whose tail is
        // withheld. One stream each so no single stream's send
        // credit, rather than the reassembly bound, is what refuses.
        for n in 0..crate::frame::MAX_OUTSTANDING_REASSEMBLIES {
            let hold = a
                .open_stream(bid, &format!("hold/{n}"), Reliability::Reliable, None, None)
                .expect("open");
            a.stream_send(hold, &filler).expect("send");
            let out = a.take_outbound();
            assert_eq!(out.len(), 2, "two fragments per message");
            b.on_datagram(aid, out[0].packet.clone(), clock::now());
        }
        assert!(delivered(&mut b).is_empty(), "no group is complete yet");
        assert_eq!(b.counters().drops(DropReason::ReassemblyRefused), 0);

        let ninth = a
            .open_stream(bid, "ninth", Reliability::Reliable, None, None)
            .expect("open");
        a.stream_send(ninth, &wanted).expect("send");
        let offered = a.take_outbound();
        assert_eq!(offered.len(), 2);
        b.on_datagram(aid, offered[0].packet.clone(), clock::now());
        assert_eq!(
            b.counters().drops(DropReason::ReassemblyRefused),
            1,
            "the ninth group must be refused and counted"
        );

        // Everything the receive side has to say goes back, which is
        // what makes this discriminating: an acknowledgement that
        // covered the refused sequence would retire the sender's
        // only copy of it.
        pump(&mut b, &mut a);

        // The sender's own timer rebuilds what it was never told
        // arrived. The receive side is given a reading past the
        // reassembly TTL, so the slots the filler heads hold are
        // reaped as those rebuilt pieces arrive.
        std::thread::sleep(core::time::Duration::from_millis(80));
        a.tick(clock::now());
        let later =
            clock::now() + core::time::Duration::from_millis(crate::frame::REASSEMBLY_TTL_MS + 1);
        let rebuilt = a.take_outbound();
        assert!(
            !rebuilt.is_empty(),
            "the retransmit timer must have rebuilt the unacknowledged pieces"
        );
        for packet in rebuilt {
            b.on_datagram(aid, packet.packet, later);
        }
        assert!(
            delivered(&mut b).contains(&wanted),
            "a fragment refused for capacity was acknowledged and lost instead of resent"
        );
    }

    /// The browser runner's `gen_bytes`, byte for byte.
    ///
    /// Its first byte is `seed & 0xFF` — which is the whole reason
    /// the witness below is a *deterministic* reproduction of a
    /// flake: a burst of `n` payloads from consecutive seeds covers
    /// `n` consecutive first bytes.
    fn gen_bytes(seed: u64, len: usize) -> Vec<u8> {
        (0..len as u64)
            .map(|i| (seed.wrapping_add(i * 167).wrapping_add((i >> 8) * 13) & 0xFF) as u8)
            .collect()
    }

    /// **The flake that outlived R3's retransmit-budget fix.**
    /// `stage5_direct_stream_carries_native_bytes_to_callback_and_iterator`
    /// pushes 12 × 512 B of generated bytes down one reliable
    /// application stream; about once in twenty runs it delivered
    /// **11**, with the sender's ledger reading `tx_seq=12
    /// pending=false ack_frontier=Some(12) abandoned=None
    /// outstanding_gap=None` — every sequence acknowledged, nothing
    /// outstanding — and every drop counter on the receiving leaf
    /// at zero except `unknown_call: 1`. So a payload was
    /// acknowledged and then consumed *inside* the leaf.
    ///
    /// It was consumed by **content sniffing on the event plane**.
    /// One stream id carries application bytes and nRPC replies,
    /// and `handle_event_plane` decided which by parsing the
    /// payload: `DISPATCH_RPC_DEADLINE_EXCEEDED` is a single byte,
    /// `0x13`, at offset 0 of a frame that carries **no body at
    /// all**, so any application blob of 24 B or more whose first
    /// byte is `0x13` decoded as a complete deadline reply, went to
    /// the call table, matched no call, and was counted away. The
    /// burst covers twelve consecutive first bytes, so 12 seeds in
    /// 256 lose exactly one payload — the rate the matrix showed.
    ///
    /// The plane is decided by **ownership** now: the RPC plane owns
    /// exactly the reply carriers it subscribed to for its own
    /// calls, and this leaf makes no call here — so every id in this
    /// test is application-owned and its bytes are delivered opaque,
    /// whatever dispatch byte they open on.
    ///
    /// `0x2108` is the harness's own seed form with the low byte
    /// that puts `0x11` (RESPONSE) at payload 9 and `0x13` at
    /// payload 11.
    #[test]
    fn an_application_stream_payload_is_never_decoded_as_an_nrpc_reply() {
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let id = crate::stream::LEAF_STREAM_DISCRIMINATOR | 0x5a01;
        let handle = a
            .open_stream(bid, "abi-direct", Reliability::Reliable, Some(id), None)
            .expect("open");
        let sent: Vec<Vec<u8>> = (0..12u64).map(|n| gen_bytes(0x2108 + n, 512)).collect();
        assert_eq!(
            (sent[9][0], sent[11][0]),
            (
                crate::rpc_wire::DISPATCH_RPC_RESPONSE,
                crate::rpc_wire::DISPATCH_RPC_DEADLINE_EXCEEDED
            ),
            "the seed must put both server-to-caller dispatch bytes in the \
             burst, or this witness proves nothing"
        );
        for payload in &sent {
            a.stream_send(handle, payload).expect("send");
        }
        assert_eq!(pump(&mut a, &mut b), 12, "one packet per payload");

        let incarnation = b.sessions.get(aid).expect("session").incarnation();
        let rx = b
            .rx_streams
            .get(&(incarnation, id))
            .expect("the arrivals built a receive cursor");
        let (next_expected, held, mode) = (rx.next_expected(), rx.held(), rx.reliability());
        let unknown_call = b.counters().drops(DropReason::UnknownCall);
        let got = delivered(&mut b);
        assert_eq!(
            got.len(),
            sent.len(),
            "every acknowledged payload must reach the consumer: the receive \
             cursor is at {next_expected} in {mode:?} with {held} held (so \
             nothing is waiting on a gap and no mode upgrade is mid-flight), \
             unknown_call={unknown_call}, duplicate_sequence={}, \
             stream_closed={}, unparsable={}",
            b.counters().drops(DropReason::DuplicateSequence),
            b.counters().drops(DropReason::StreamClosed),
            b.counters().drops(DropReason::Unparsable),
        );
        assert_eq!(got, sent, "byte-exact and in order");
        assert_eq!(
            unknown_call, 0,
            "application bytes must never be routed to the call table"
        );
    }

    /// The same root cause on the surface with the bigger blast
    /// radius: **pub/sub**. A subscribed channel's publications
    /// ride the event plane too, so before the carrier check one
    /// channel message in 256 — every payload whose first byte was
    /// `0x13` — was parsed as a deadline reply and counted away
    /// instead of delivered. This is the byte that used to do it,
    /// on the real `subscribe`/`publish` pair.
    #[test]
    fn a_channel_message_that_begins_with_a_reply_dispatch_byte_is_still_delivered() {
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        b.subscribe(aid, "app.sink").expect("subscribe");
        b.take_outbound();

        let mut payload = vec![crate::rpc_wire::DISPATCH_RPC_DEADLINE_EXCEEDED];
        payload.extend_from_slice(&[0x5Au8; 63]);
        a.publish(bid, "app.sink", &payload).expect("publish");
        assert_eq!(pump(&mut a, &mut b), 1);

        let events = b.drain_events();
        let got: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                LeafEvent::ChannelMessage { payload, .. } => Some(payload.to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![payload],
            "a subscribed channel's message must be delivered whatever byte \
             it opens on; events were {events:?}"
        );
        assert_eq!(b.counters().drops(DropReason::UnknownCall), 0);
    }

    /// **The other half of the defect.** A payload consumed after
    /// this leaf acknowledged it must be observable by the
    /// consumer, not just by a counter nobody polls — a frame on a
    /// carrier the RPC plane **owns** that matches no pending call
    /// is the one remaining way an acknowledged payload is
    /// discarded, and it raises [`LeafEvent::Dropped`] as well as
    /// moving `unknown_call`.
    ///
    /// The ownership is established the only way it can be: this
    /// leaf calls a service, which subscribes it to that service's
    /// reply carrier, and the call then ends on its own deadline.
    /// The carrier stays RPC-owned; the reply that lands on it
    /// belongs to no pending call. That is a late reply — the exact
    /// case `unknown_call` names — and it is refused out loud
    /// instead of being handed to a consumer that never asked for
    /// bytes on a reply channel.
    #[test]
    fn a_reply_that_matches_no_call_is_surfaced_as_well_as_counted() {
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        // One real call, so the RPC plane owns its reply carrier —
        // a frame on a carrier no plane owns is application bytes by
        // contract, and this witness is about the other case.
        let service = "late.reply.svc";
        let _pending = b.call(aid, service, b"q", Some(1)).expect("call");
        let later = clock::now() + std::time::Duration::from_millis(2);
        assert_eq!(
            b.tick(later),
            1,
            "the call must end on its deadline, so the reply below \
             matches nothing while its carrier stays RPC-owned"
        );
        b.take_outbound();
        b.drain_events();

        // The carrier that call subscribed to, and the route a reply
        // on it declares.
        let route = Channel::new(&b.reply_channel_for(service).expect("reply channel"))
            .expect("valid channel name")
            .canonical();
        let carrier = route_stream_id(route);
        let mut frame = crate::rpc_wire::EventMeta::new(
            crate::rpc_wire::DISPATCH_RPC_DEADLINE_EXCEEDED,
            0,
            7,
            9,
            0,
        )
        .to_bytes()
        .to_vec();
        frame.extend_from_slice(&route.to_le_bytes());
        let handle = a
            .open_stream(
                bid,
                "reply-carrier",
                Reliability::Reliable,
                Some(carrier),
                None,
            )
            .expect("open");
        a.stream_send(handle, &frame).expect("send");
        assert_eq!(pump(&mut a, &mut b), 1);

        assert_eq!(
            b.counters().drops(DropReason::UnknownCall),
            1,
            "no call is pending, so the reply is refused"
        );
        assert!(
            b.drain_events().iter().any(|e| matches!(
                e,
                LeafEvent::Dropped {
                    reason: DropReason::UnknownCall
                }
            )),
            "an acknowledged payload this leaf consumed must reach the \
             consumer as a typed refusal, or the next silent loss takes \
             another three rounds to find"
        );
    }

    /// **P4's recovery witness: one stream id, two modes.** A leaf
    /// publishing nRPC requests derives the stream id from the
    /// channel, so a fire-and-forget leg and a reliable leg on the
    /// same service ride ONE id — which is exactly what the browser
    /// matrix does with `app.stage5.sink`.
    ///
    /// `NetSession::open_stream_full` was first-open-wins for the
    /// reliability mode as well as for the window, so the reliable
    /// leg was admitted onto the fire-and-forget machinery:
    /// `FireAndForget::on_send` retains nothing, so the sender had
    /// no descriptor to rebuild from, the receiver kept no gap state
    /// to NACK against, and a lost packet was silent loss — no
    /// retransmit, and no terminal failure to report it either.
    /// RELIABLE is now authoritative and monotonic on both halves.
    #[test]
    fn a_reliable_send_recovers_on_an_id_that_first_carried_fire_and_forget() {
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let id = crate::stream::LEAF_STREAM_DISCRIMINATOR | 0x5b;

        // The fire-and-forget leg. It opens the id on both halves.
        let faf = a
            .open_stream(bid, "shared", Reliability::FireAndForget, Some(id), Some(9))
            .expect("open");
        a.stream_send(faf, b"faf").expect("send");
        assert_eq!(pump(&mut a, &mut b), 1);
        assert_eq!(delivered(&mut b), vec![b"faf".to_vec()]);
        assert!(
            !a.sessions.get(bid).expect("session").wire().has_unacked(),
            "fire-and-forget retains no retransmit owner"
        );

        // The same id, now RELIABLE. Three packets, the middle one
        // lost and the last one arriving early.
        let rel = a
            .open_stream(bid, "shared", Reliability::Reliable, Some(id), Some(9))
            .expect("open");
        for body in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
            a.stream_send(rel, body).expect("send");
        }
        let packets = a.take_outbound();
        assert_eq!(packets.len(), 3);
        assert!(
            a.sessions.get(bid).expect("session").wire().has_unacked(),
            "a reliable send must leave a retransmit owner behind whatever \
             mode opened the id first"
        );

        b.on_datagram(aid, packets[2].packet.clone(), clock::now());
        b.on_datagram(aid, packets[0].packet.clone(), clock::now());
        assert_eq!(
            delivered(&mut b),
            vec![b"one".to_vec()],
            "the reorder buffer must hold 'three' behind the missing 'two'"
        );

        b.tick(clock::now());
        assert!(pump(&mut b, &mut a) > 0, "the gap must produce a NACK");
        assert!(
            pump(&mut a, &mut b) > 0,
            "the NACK must produce a retransmit"
        );
        assert_eq!(
            delivered(&mut b),
            vec![b"two".to_vec(), b"three".to_vec()],
            "recovery must release the held sequence too, in order"
        );
    }

    /// **R3's replenishment witness, leaf to leaf.** More bytes than
    /// one send window, through the ordinary send path. Without the
    /// receiver returning grants the sender's credit reaches zero and
    /// the send is refused; with them it completes and every message
    /// arrives.
    #[test]
    fn sustained_traffic_past_the_send_window_is_replenished_by_receiver_grants() {
        let (mut a, mut b) = pair();
        let bid = b.node_id();
        let handle = a
            .open_stream(
                bid,
                "bulk",
                Reliability::Reliable,
                Some(crate::stream::LEAF_STREAM_DISCRIMINATOR | 11),
                Some(11),
            )
            .expect("open");

        // Three windows' worth, in chunks that each cost a packet.
        let window = u64::from(net_wire::stream::DEFAULT_STREAM_WINDOW_BYTES);
        let body = vec![0x7Eu8; 4_000];
        let messages = (3 * window / 4_000) as usize + 1;
        let mut sent = 0usize;
        for _ in 0..messages {
            a.stream_send(handle, &body)
                .expect("the window must be replenished, not exhausted");
            sent += 1;
            pump(&mut a, &mut b);
            // The grants the receiver minted go back the other way.
            pump(&mut b, &mut a);
        }

        assert!(
            u64::from(sent as u32) * 4_000 > window,
            "the witness must exceed one window"
        );
        assert_eq!(delivered(&mut b).len(), sent, "every message must arrive");
        let grants = a
            .sessions
            .get(bid)
            .expect("session")
            .wire()
            .try_stream(crate::stream::LEAF_STREAM_DISCRIMINATOR | 11)
            .expect("stream")
            .credit_grants_received();
        assert!(
            grants > 0,
            "the sender must have observed replenishment, not merely not stalled"
        );
    }

    /// The inverse of the witness above, stated as the property that
    /// makes it discriminating: with no grants coming back, the
    /// window runs out and the refusal is typed and synchronous
    /// rather than a queue that grows.
    #[test]
    fn an_unreplenished_window_refuses_the_send_synchronously() {
        let (mut a, b) = pair();
        let bid = b.node_id();
        let handle = a
            .open_stream(
                bid,
                "bulk",
                Reliability::Reliable,
                Some(crate::stream::LEAF_STREAM_DISCRIMINATOR | 13),
                Some(13),
            )
            .expect("open");
        let body = vec![0x7Eu8; 4_000];
        let mut refusal = None;
        for _ in 0..64 {
            if let Err(e) = a.stream_send(handle, &body) {
                refusal = Some(e);
                break;
            }
        }
        match refusal {
            Some(LeafError::Backpressure {
                stream_id,
                remaining,
                ..
            }) => {
                assert_eq!(stream_id, crate::stream::LEAF_STREAM_DISCRIMINATOR | 13);
                assert!(remaining < 4_100, "the window is what ran out: {remaining}");
            }
            other => panic!("expected a typed bounded refusal, got {other:?}"),
        }
    }

    /// **Byte credit and packet ownership are different bounds**, and
    /// the one tiny reliable messages reach is the second.
    ///
    /// One-byte sends cost ~89 on-wire bytes each, so ~736 of them
    /// fit inside the 64 KiB window while the retransmit window
    /// tracks 128 descriptors. Admitting on bytes alone therefore
    /// accepted hundreds of packets the stream could not own:
    /// `ReliableStream::on_send` evicts the oldest unacknowledged
    /// descriptor to make room, the packet stays on the wire, and
    /// nothing can rebuild it afterwards. `has_unacked()` still
    /// reported `true` throughout — it says *something* is tracked,
    /// not that everything accepted is.
    ///
    /// So the refusal is asserted at the bound, and then the
    /// ownership it protects is asserted where it has to hold: the
    /// FIRST packet of the burst is withheld, the receiver's real
    /// NACK asks for it, and the sender must still be able to
    /// rebuild it.
    #[test]
    fn a_tiny_reliable_burst_is_refused_at_the_descriptor_bound_with_every_packet_still_owned() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 15;
        let (mut a, mut b) = pair();
        let bid = b.node_id();
        let handle = a
            .open_stream(bid, "tiny", Reliability::Reliable, Some(STREAM), Some(15))
            .expect("open");

        let mut admitted = 0usize;
        let mut refusal = None;
        for _ in 0..1_000 {
            match a.stream_send(handle, b"x") {
                Ok(()) => admitted += 1,
                Err(e) => {
                    refusal = Some(e);
                    break;
                }
            }
        }

        let bound = net_wire::reliability::ReliableStream::max_pending_for_window(
            net_wire::stream::DEFAULT_STREAM_WINDOW_BYTES,
        );
        assert_eq!(
            admitted, bound,
            "admission must stop at the retransmit window's packet capacity"
        );
        match refusal {
            Some(LeafError::ReliableWindowFull {
                stream_id,
                needed,
                remaining,
            }) => {
                assert_eq!(stream_id, STREAM);
                assert_eq!(
                    (needed, remaining),
                    (1, 0),
                    "the refusal must name packet capacity, not bytes"
                );
            }
            other => panic!("expected a typed packet-capacity refusal, got {other:?}"),
        }
        let credit = a
            .sessions
            .get(bid)
            .expect("session")
            .wire()
            .try_stream(STREAM)
            .expect("stream")
            .tx_credit_remaining();
        assert!(
            credit > 4_000,
            "bytes were never the bound — {credit} credit was still unspent, \
             which is exactly why admitting on bytes alone overran the window"
        );

        // Every admitted packet is still owned. Withhold the first
        // and deliver a reorder-bounded run behind it, so the
        // receiver's own gap detection asks for sequence 0.
        let packets = a.take_outbound();
        assert_eq!(packets.len(), admitted);
        let run = 50;
        let aid = a.node_id();
        for packet in &packets[1..=run] {
            b.on_datagram(aid, packet.packet.clone(), clock::now());
        }
        assert!(
            delivered(&mut b).is_empty(),
            "everything must be held behind the withheld sequence 0"
        );
        b.tick(clock::now());
        pump(&mut b, &mut a);
        pump(&mut a, &mut b);
        assert_eq!(
            delivered(&mut b).len(),
            run + 1,
            "sequence 0 must have survived {admitted} sends, been rebuilt on the \
             real NACK, and released the whole held run in order"
        );
    }

    /// Build one piece of a fragment group the anchor would send,
    /// with every header field the group binds under the caller's
    /// control. The production builder emits one plane and one mode
    /// per group by construction, so a witness for *inconsistent*
    /// pieces has to set the fields itself — from a real
    /// session-authenticated builder, with real AEAD.
    #[expect(
        clippy::too_many_arguments,
        reason = "the six header fields a group binds plus the three \
                  fragment-position fields; a struct would only move the list"
    )]
    fn anchor_fragment(
        anchor: &NetSession,
        stream_id: u64,
        subprotocol_id: u16,
        channel_hash: u16,
        reliable: bool,
        fragment_id: u16,
        offset: u16,
        frag_flags: u8,
        payload: &[u8],
    ) -> Bytes {
        anchor.open_stream_with(stream_id, reliable, 1);
        let seq = anchor.get_or_create_stream(stream_id).next_tx_seq();
        let events = [Bytes::copy_from_slice(payload)];
        let mut builder = anchor.thread_local_pool().get();
        builder.set_channel_hash(channel_hash);
        builder.set_origin_hash(0xFEED_FACE_0000_0001);
        builder.set_fragment(fragment_id, offset, frag_flags);
        builder.build_subprotocol(
            stream_id,
            seq,
            &events,
            if reliable {
                PacketFlags::RELIABLE
            } else {
                PacketFlags::NONE
            },
            subprotocol_id,
        )
    }

    /// Both stream dispositions of **one** drain: what was
    /// delivered, and what ended.
    ///
    /// One drain, because `drain_events` empties the queue: asking
    /// for the data and then for the failures discards whichever
    /// answer came second, which turns "no stream failed" into a
    /// tautology.
    fn drained(node: &mut LeafNode) -> (Vec<Vec<u8>>, Vec<(u64, StreamFailure)>) {
        let mut data = Vec::new();
        let mut failed = Vec::new();
        for event in node.drain_events() {
            match event {
                LeafEvent::StreamData { payload, .. } => data.push(payload.to_vec()),
                LeafEvent::StreamFailed {
                    stream_id, reason, ..
                } => failed.push((stream_id, reason)),
                _ => {}
            }
        }
        (data, failed)
    }

    /// **An acknowledged group is owned.** The receive path has to
    /// acknowledge a fragment's sequence before its group is
    /// complete, or the sender's window never opens — and that
    /// acknowledgement retires the sender's only copy. So when the
    /// TTL then reaps the group there is nothing left to rebuild
    /// from and no wire gap left to NACK, and counting the loss
    /// leaves a consumer waiting forever on a stream that simply
    /// stopped delivering.
    ///
    /// Two halves, and both are the repair: the stream ends typed
    /// and named, **and** the delayed tail is no longer acknowledged
    /// into a group that no longer has its head — which is what
    /// used to let the sender retire its last descriptor too.
    #[test]
    fn an_acknowledged_group_reaped_at_the_ttl_ends_its_stream_and_stops_acknowledging() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 21;
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let handle = a
            .open_stream(bid, "ttl", Reliability::Reliable, Some(STREAM), Some(21))
            .expect("open");
        let body = vec![0x5Au8; crate::frame::MAX_FRAGMENT_PAYLOAD + 1];
        a.stream_send(handle, &body).expect("send");
        let packets = a.take_outbound();
        assert_eq!(packets.len(), 2, "two fragments");

        // The head arrives and is positively acknowledged: the
        // feedback is real and it reaches the sender.
        let t0 = clock::now();
        b.on_datagram(aid, packets[0].packet.clone(), t0);
        assert!(
            delivered(&mut b).is_empty(),
            "one fragment is not a message"
        );
        let feedback = b.take_outbound();
        assert!(!feedback.is_empty(), "the head must be acknowledged");
        for packet in feedback {
            a.on_datagram(bid, packet.packet, t0);
        }

        // The deadline passes on the receiver's own clock reading.
        let late = t0 + core::time::Duration::from_millis(crate::frame::REASSEMBLY_TTL_MS + 1);
        b.tick(late);
        let (data, failed) = drained(&mut b);
        assert!(data.is_empty(), "nothing was ever complete");
        assert_eq!(
            failed,
            vec![(STREAM, StreamFailure::ReassemblyAbandoned)],
            "the reap must name the stream whose acknowledged bytes it took"
        );
        assert_eq!(
            StreamFailure::ReassemblyAbandoned.as_str(),
            "reassembly_abandoned"
        );

        // The tail now finds no head. It must not be acknowledged:
        // an ack here retires the sender's last descriptor for a
        // message that can never be delivered.
        b.on_datagram(aid, packets[1].packet.clone(), late);
        assert!(
            b.take_outbound().is_empty(),
            "a tail whose group was reaped must not be acknowledged"
        );
        assert!(
            drained(&mut b).0.is_empty(),
            "and it must not assemble anything either"
        );
    }

    /// The same ownership question with **no tick at all**, which is
    /// where the ordering matters. A late piece used to be
    /// acknowledged and then find its own group reaped inside the
    /// very next call: the open-group shortcut answered "admitted"
    /// because the map still held the expired group, the sequence
    /// was acknowledged — retiring the sender's last descriptor for
    /// the whole message — and the reap happened afterwards. So the
    /// deadline is evaluated against this arrival's own clock
    /// reading, before its admission decision.
    #[test]
    fn a_late_tail_is_not_acknowledged_into_a_group_reaped_on_its_own_arrival() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 28;
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let handle = a
            .open_stream(
                bid,
                "no-tick",
                Reliability::Reliable,
                Some(STREAM),
                Some(28),
            )
            .expect("open");
        let body = vec![0x3Cu8; crate::frame::MAX_FRAGMENT_PAYLOAD + 1];
        a.stream_send(handle, &body).expect("send");
        let packets = a.take_outbound();
        assert_eq!(packets.len(), 2, "two fragments");

        let t0 = clock::now();
        b.on_datagram(aid, packets[0].packet.clone(), t0);
        let feedback = b.take_outbound();
        assert!(!feedback.is_empty(), "the head must be acknowledged");
        for packet in feedback {
            a.on_datagram(bid, packet.packet, t0);
        }
        assert_eq!(drained(&mut b), (Vec::new(), Vec::new()));

        // No `tick`. The tail's own arrival is what carries the
        // clock reading past the deadline.
        let late = t0 + core::time::Duration::from_millis(crate::frame::REASSEMBLY_TTL_MS + 1);
        b.on_datagram(aid, packets[1].packet.clone(), late);
        assert!(
            b.take_outbound().is_empty(),
            "the tail's sequence must stay outstanding: acknowledging it here \
             retires the sender's last copy of a message that can never arrive"
        );
        assert_eq!(
            drained(&mut b),
            (
                Vec::new(),
                vec![(STREAM, StreamFailure::ReassemblyAbandoned)]
            ),
            "and the acknowledged head it lost is named, not counted"
        );
    }

    /// The other half of the same bound: inside the TTL the group
    /// completes and nothing is ended. Without this the repair above
    /// could be "fail every fragmented message".
    #[test]
    fn an_unreaped_group_completes_and_ends_no_stream() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 22;
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let handle = a
            .open_stream(bid, "ttl-ok", Reliability::Reliable, Some(STREAM), Some(22))
            .expect("open");
        let body = vec![0x5Au8; crate::frame::MAX_FRAGMENT_PAYLOAD + 1];
        a.stream_send(handle, &body).expect("send");
        let packets = a.take_outbound();
        let t0 = clock::now();
        b.on_datagram(aid, packets[0].packet.clone(), t0);
        for packet in b.take_outbound() {
            a.on_datagram(bid, packet.packet, t0);
        }
        let inside = t0 + core::time::Duration::from_millis(crate::frame::REASSEMBLY_TTL_MS - 1);
        b.tick(inside);
        b.on_datagram(aid, packets[1].packet.clone(), inside);
        assert_eq!(drained(&mut b), (vec![body], Vec::new()));
    }

    /// **Close is local, and the peer's sequence is not rewound by
    /// it.** Deleting the receive cursor made a reopen of the same id
    /// wait for sequence zero while the peer sent sequence one: the
    /// record was accepted and acknowledged on the wire and then held
    /// against a gap nothing could fill, so a successful
    /// close/reopen silently stranded everything after it.
    ///
    /// The cursor therefore survives close, delivery does not, and a
    /// reopen resumes where the peer actually is. The send half is
    /// asserted separately, because the wrong repair here is to
    /// retire the whole bidirectional stream.
    #[test]
    fn a_closed_stream_delivers_nothing_and_a_reopen_resumes_at_the_peers_next_sequence() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 23;
        let (mut a, mut b) = pair();
        let (aid, bid) = (a.node_id(), b.node_id());
        let receive = a
            .open_stream(bid, "reopen", Reliability::Reliable, Some(STREAM), Some(23))
            .expect("open");
        let send = b
            .open_stream(aid, "reopen", Reliability::Reliable, Some(STREAM), Some(23))
            .expect("open");

        b.stream_send(send, b"before").expect("send");
        pump(&mut b, &mut a);
        assert_eq!(delivered(&mut a), vec![b"before".to_vec()]);
        pump(&mut a, &mut b);

        a.close_stream(receive).expect("close");
        b.stream_send(send, b"while-closed").expect("send");
        pump(&mut b, &mut a);
        assert!(
            delivered(&mut a).is_empty(),
            "a closed consumer is not delivered to"
        );
        assert_eq!(
            a.counters().drops(DropReason::StreamClosed),
            1,
            "and the record it did not get is counted, not silent"
        );
        pump(&mut a, &mut b);

        let reopened = a
            .open_stream(bid, "reopen", Reliability::Reliable, Some(STREAM), Some(23))
            .expect("reopen");
        b.stream_send(send, b"after").expect("send");
        pump(&mut b, &mut a);
        assert_eq!(
            drained(&mut a),
            (vec![b"after".to_vec()], Vec::new()),
            "the reopened consumer must resume at the peer's next sequence, \
             not wait for a zero the peer sent long ago, and nothing failed"
        );

        // The send half was never the receive half's to retire.
        a.stream_send(reopened, b"reply").expect("send");
        pump(&mut a, &mut b);
        assert_eq!(delivered(&mut b), vec![b"reply".to_vec()]);
    }

    /// **A RESET starts a new receive lifetime inside one session.**
    /// Reassembly is keyed only by incarnation, so a head retained
    /// from before the reset used to wait for its old delayed tail
    /// and complete afterwards — delivering a payload from the
    /// lifetime that just ended, against the fresh cursor, behind the
    /// `PeerReset` the consumer had already been given.
    #[test]
    fn a_reset_retires_the_streams_partial_groups_and_leaves_the_send_half() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 24;
        let (mut node, anchor) = connected();
        node.drain_events();

        let head = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            24,
            true,
            9,
            0,
            crate::frame::FRAG_FRAGMENTED,
            b"head-",
        );
        let tail = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            24,
            true,
            9,
            5,
            crate::frame::FRAG_FRAGMENTED | crate::frame::FRAG_LAST,
            b"tail",
        );
        node.on_datagram(ANCHOR, head, clock::now());
        assert_eq!(drained(&mut node), (Vec::new(), Vec::new()));

        let reset = anchor_packet(
            &anchor,
            net_wire::session::CONTROL_STREAM_ID,
            SUBPROTOCOL_STREAM_RESET,
            0,
            false,
            &StreamReset { stream_id: STREAM }.encode(),
        );
        node.on_datagram(ANCHOR, reset, clock::now());
        assert_eq!(
            drained(&mut node),
            (Vec::new(), vec![(STREAM, StreamFailure::PeerReset)]),
            "the reset is the disposition, and it is the only one"
        );

        node.on_datagram(ANCHOR, tail, clock::now());
        assert_eq!(
            drained(&mut node),
            (Vec::new(), Vec::new()),
            "a payload from the retired receive lifetime must not be delivered \
             after the reset that ended it, and nothing new ends either"
        );

        // The send half is a different direction: the id stays
        // usable and the reset did not rewind this leaf's own
        // sequence space. The feedback the three arrivals queued is
        // cleared first, so what is counted is this send.
        node.take_outbound();
        let handle = node
            .open_stream(
                ANCHOR,
                "after-reset",
                Reliability::Reliable,
                Some(STREAM),
                Some(24),
            )
            .expect("the send half survives the peer's reset");
        node.stream_send(handle, b"mine").expect("send");
        assert_eq!(node.take_outbound().len(), 1);
    }

    /// **A group's plane belongs to its first fragment.** Same
    /// stream, origin, channel and contiguous sequences, changing
    /// only the completing piece's subprotocol: the payload used to
    /// be assembled and dispatched under that last arrival's plane,
    /// so an event body could be decoded as channel membership after
    /// both planes' sequences were acknowledged.
    #[test]
    fn a_fragment_group_cannot_be_completed_under_another_plane() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 25;
        let (mut node, anchor) = connected();
        node.drain_events();

        let head = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            25,
            true,
            11,
            0,
            crate::frame::FRAG_FRAGMENTED,
            b"head-",
        );
        // Everything the group binds is identical except the plane.
        let tail = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_MEMBERSHIP,
            25,
            true,
            11,
            5,
            crate::frame::FRAG_FRAGMENTED | crate::frame::FRAG_LAST,
            b"tail",
        );
        node.on_datagram(ANCHOR, head, clock::now());
        node.on_datagram(ANCHOR, tail, clock::now());
        assert_eq!(
            node.counters().drops(DropReason::ReassemblyInconsistent),
            1,
            "the contradicting piece is refused and counted"
        );
        assert_eq!(
            drained(&mut node),
            (
                Vec::new(),
                vec![(STREAM, StreamFailure::ReassemblyAbandoned)]
            ),
            "a mixed-plane group must not be dispatched at all, and the head it \
             destroyed was already acknowledged, so the stream ends typed \
             rather than stopping silently"
        );
    }

    /// The control the plane witness needs: one plane throughout,
    /// arriving out of order, still assembles and still delivers.
    /// Without it "refuse every fragmented group" would pass above.
    #[test]
    fn a_consistent_group_arriving_out_of_order_still_assembles() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 26;
        let (mut node, anchor) = connected();
        node.drain_events();

        let head = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            26,
            true,
            13,
            0,
            crate::frame::FRAG_FRAGMENTED,
            b"head-",
        );
        let tail = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            26,
            true,
            13,
            5,
            crate::frame::FRAG_FRAGMENTED | crate::frame::FRAG_LAST,
            b"tail",
        );
        node.on_datagram(ANCHOR, tail, clock::now());
        node.on_datagram(ANCHOR, head, clock::now());
        assert_eq!(node.counters().drops(DropReason::ReassemblyInconsistent), 0);
        assert_eq!(
            drained(&mut node),
            (vec![b"head-tail".to_vec()], Vec::new()),
            "one plane throughout still assembles, whatever order it arrives in"
        );
    }

    /// The mode is bound the same way, and it is not cosmetic: it is
    /// what the consumer cursor's gap disposition is chosen from, so
    /// a group whose pieces disagree about it has no single answer.
    #[test]
    fn a_fragment_group_cannot_change_its_mode_between_pieces() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 27;
        let (mut node, anchor) = connected();
        node.drain_events();

        let head = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            27,
            true,
            15,
            0,
            crate::frame::FRAG_FRAGMENTED,
            b"head-",
        );
        let tail = anchor_fragment(
            &anchor,
            STREAM,
            SUBPROTOCOL_EVENT_PLANE,
            27,
            false,
            15,
            5,
            crate::frame::FRAG_FRAGMENTED | crate::frame::FRAG_LAST,
            b"tail",
        );
        node.on_datagram(ANCHOR, head, clock::now());
        node.on_datagram(ANCHOR, tail, clock::now());
        assert_eq!(
            node.counters().drops(DropReason::ReassemblyInconsistent),
            1,
            "a piece claiming another mode is not a piece of this group"
        );
        assert_eq!(
            drained(&mut node),
            (
                Vec::new(),
                vec![(STREAM, StreamFailure::ReassemblyAbandoned)]
            ),
        );
    }

    /// **X2: the terminal disposition belongs to the shared admission
    /// path.** `open_stream` and `check_handle` refused a stream whose
    /// send half had failed, but `publish` never looks at a handle —
    /// it derives its own stream id and went straight to
    /// `build_packets`, so a channel could keep queueing publications
    /// onto a sequence space the peer had already been told to reset.
    ///
    /// Driven by real exhaustion, not an injected `send_failed` entry:
    /// the publication is queued, never delivered and therefore never
    /// acknowledged, and the wire's own RTO/retry machinery
    /// (`ReliableStream::DEFAULT_RTO`, `DEFAULT_MAX_RETRIES`) gives up
    /// on it. What is asserted is the refusal a caller sees, that
    /// nothing new was queued, and that another channel on the same
    /// session is untouched — a per-stream verdict, not a session one.
    #[test]
    fn an_exhausted_channel_refuses_publish_while_another_channel_still_sends() {
        let (mut a, b) = pair();
        let bid = b.node_id();
        let alpha = crate::channel::Channel::new("alpha").expect("channel");

        a.publish(bid, "alpha", b"one").expect("first publish");
        assert!(
            !a.take_outbound().is_empty(),
            "precondition: the publication reached the transport queue"
        );

        // Nothing is ever delivered to `b`, so nothing is ever
        // acknowledged. Retransmits are driven by the wire clock, so
        // this waits on it rather than reaching into the window — and
        // it waits the wire's OWN give-up horizon (the doubling RTO
        // ladder, `DEFAULT_MAX_RETRIES` attempts) rather than a magic
        // iteration count, so a change to that pacing moves this test
        // with it instead of breaking it.
        let horizon = net_wire::reliability::ReliableStream::give_up_horizon(
            net_wire::reliability::ReliableStream::DEFAULT_RTO,
            net_wire::reliability::ReliableStream::DEFAULT_MAX_RETRIES,
        );
        // Through the seam, not `std::time::Instant`: the boundary
        // test denies a direct clock read anywhere in this file,
        // including test code, because a `#[cfg(test)]` exception
        // would be one grep away from becoming a production one.
        let started = clock::now();
        let mut failure = None;
        while started.elapsed() < horizon * 2 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            a.tick(clock::now());
            a.take_outbound();
            let (_, failed) = drained(&mut a);
            if let Some(hit) = failed
                .iter()
                .find(|(stream_id, _)| *stream_id == alpha.publish_stream_id())
            {
                failure = Some(*hit);
                break;
            }
        }
        assert_eq!(
            failure,
            Some((
                alpha.publish_stream_id(),
                StreamFailure::RetransmitsExhausted
            )),
            "precondition: the channel's send half must fail terminally on its own"
        );

        let refused = a.publish(bid, "alpha", b"two");
        assert!(
            matches!(refused, Err(LeafError::Session(_))),
            "a publication on an exhausted channel must be refused, got {refused:?}"
        );
        assert!(
            a.take_outbound().is_empty(),
            "and nothing may be queued for a sequence space the peer has reset"
        );

        a.publish(bid, "beta", b"three")
            .expect("another channel on the same session must remain usable");
        assert!(
            !a.take_outbound().is_empty(),
            "the verdict is per stream, not per session"
        );
    }

    /// **L5: a terminal send half must give its stamp reservation
    /// back.** Every retained reliable descriptor also needs a header
    /// stamp, and the stamps live in ONE session-wide table whose cap
    /// X4 made deliberately non-evicting: `build_packets` refuses a
    /// message it cannot stamp rather than deleting another stream's
    /// ownership. The only paths that released a slot were a
    /// cumulative ack and destroying the session — and a stream whose
    /// retransmits ran out will never be acked. So streams that had
    /// already ended terminally kept their slots forever while owning
    /// no descriptor at all, and enough of them closed the session's
    /// reliable admission permanently, refusing exactly the "use
    /// another stream id" recovery `ReliableWindowFull` offers.
    ///
    /// Driven end-to-end on the real mechanisms: the table is filled
    /// through the public handle-send path until the session itself
    /// refuses (the stream count is discovered, not assumed), the
    /// terminal verdict comes from the wire's own RTO/retry ladder
    /// with nothing ever delivered or acknowledged, and what is
    /// asserted is what a caller can observe — a fresh id admitted
    /// again after the owners ended.
    ///
    /// The cap itself is untouched, and
    /// `LeafSession::retire_stream_stamps` is bounded to the one
    /// `stream_id` that died; that nothing live is ever evicted is
    /// X4's `a_full_stamp_table_refuses_a_new_stream_rather_than_evicting_an_owned_one`.
    #[test]
    fn an_exhausted_stream_returns_its_stamps_so_a_fresh_id_is_admitted() {
        let (mut a, b) = pair();
        let bid = b.node_id();
        let base = 0x0002_0000_0000_0020u64;
        let per_stream = net_wire::reliability::ReliableStream::max_pending_for_window(
            net_wire::stream::DEFAULT_STREAM_WINDOW_BYTES,
        );

        // Fill the shared table: successive ids, each kept inside its
        // own per-stream descriptor window so no per-stream check is
        // what refuses, until the session-wide stamp budget is what
        // does.
        let mut owners: Vec<u64> = Vec::new();
        let refusal = loop {
            let id = base + owners.len() as u64;
            let handle = a
                .open_stream(bid, "", Reliability::Reliable, Some(id), None)
                .expect("the session is established");
            let mut refusal = None;
            for _ in 0..per_stream {
                if let Err(e) = a.stream_send(handle, b"x") {
                    refusal = Some(e);
                    break;
                }
            }
            a.take_outbound();
            match refusal {
                Some(e) => break e,
                None => owners.push(id),
            }
            assert!(
                owners.len() < 64,
                "the session-wide stamp cap must be reachable this way"
            );
        };
        assert!(
            matches!(refusal, LeafError::ReliableWindowFull { .. }),
            "precondition: the stamp table must be full, got {refusal:?}"
        );
        assert!(
            owners.len() > 1,
            "precondition: the defect needs more than one stream in the table"
        );

        // Nothing is ever delivered to `b`, so nothing is ever
        // acknowledged, and the wire's own doubling-RTO ladder gives
        // up. Waited against `give_up_horizon` rather than an
        // iteration count, and read through the clock seam — the
        // dependency-boundary test denies a direct `Instant` anywhere
        // in this file, test code included.
        let horizon = net_wire::reliability::ReliableStream::give_up_horizon(
            net_wire::reliability::ReliableStream::DEFAULT_RTO,
            net_wire::reliability::ReliableStream::DEFAULT_MAX_RETRIES,
        );
        let started = clock::now();
        let mut dead: Vec<u64> = Vec::new();
        while started.elapsed() < horizon * 3 && dead.len() < owners.len() {
            std::thread::sleep(std::time::Duration::from_millis(30));
            a.tick(clock::now());
            a.take_outbound();
            let (_, failed) = drained(&mut a);
            for (stream_id, reason) in failed {
                if owners.contains(&stream_id) && reason == StreamFailure::RetransmitsExhausted {
                    dead.push(stream_id);
                }
            }
        }
        assert_eq!(
            dead.len(),
            owners.len(),
            "precondition: every owner's send half must fail terminally on its own"
        );

        // The recovery the refusal advertises. A fresh id needs a
        // stamp, and the whole budget was held by streams that can
        // never rebuild anything again.
        let fresh = base + owners.len() as u64 + 1;
        let handle = a
            .open_stream(bid, "", Reliability::Reliable, Some(fresh), None)
            .expect("a fresh id is not a failed one");
        a.stream_send(handle, b"x")
            .expect("ended streams must not hold the session's stamp budget forever");
        assert!(
            !a.take_outbound().is_empty(),
            "and the admitted message must actually be queued"
        );
    }

    /// **After promotion, a still-open fire-and-forget handle's
    /// send is delivered under the reliable contract.** Kyra's R3-3
    /// closure is disjunctive — older producers inherit the stream's
    /// mode, *or* a fire-and-forget send on a promoted stream is
    /// refused typed — and this leaf inherits. The observable
    /// consequence, and the whole reason the choice matters, is what
    /// happens to that send when an earlier reliable sequence is
    /// lost: it is HELD behind the gap and retained for retransmit,
    /// so both messages arrive in sequence order. Left
    /// fire-and-forget it would have been released immediately, past
    /// a hole its own stream still owed, and the reliable message
    /// behind it would then have been refused as a duplicate of a
    /// sequence the cursor had already walked over.
    ///
    /// Nothing here inspects a flag: the recovery runs entirely
    /// through production paths — the receiver's gap report, the
    /// sender's retained descriptor, a rebuild with a fresh AEAD
    /// counter — and the assertion is the application's own byte
    /// stream, in order.
    #[test]
    fn a_fire_and_forget_send_after_promotion_is_held_and_recovered_in_order() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 0x5B;
        let (mut a, mut b) = pair();
        let bid = b.node_id();
        let faf = a
            .open_stream(bid, "", Reliability::FireAndForget, Some(STREAM), None)
            .expect("fire-and-forget handle");
        let reliable = a
            .open_stream(bid, "", Reliability::Reliable, Some(STREAM), None)
            .expect("a reliable handle promotes the stream");

        // The promoting send, lost on the wire.
        a.stream_send(reliable, b"reliable-head").expect("send");
        assert_eq!(a.take_outbound().len(), 1, "one packet, and it is lost");

        // The older handle is still valid and still asks for
        // fire-and-forget. The stream's mode is the contract.
        a.stream_send(faf, b"through-the-old-handle").expect("send");
        assert_eq!(pump(&mut a, &mut b), 1);
        assert!(
            delivered(&mut b).is_empty(),
            "a send on a promoted stream must not be released past a \
             reliable hole the stream still owes"
        );

        // Production recovery: b's gap report reaches a, a rebuilds
        // the descriptor it retained, and b releases both in order.
        assert!(pump(&mut b, &mut a) > 0, "the receiver must ask");
        assert!(pump(&mut a, &mut b) > 0, "the sender must answer");
        assert_eq!(
            delivered(&mut b),
            vec![
                b"reliable-head".to_vec(),
                b"through-the-old-handle".to_vec()
            ],
            "both messages, in sequence order, after recovery"
        );
    }

    /// **The boundary signal is applied at admission of the head,
    /// not at emission of the completed message.** A reliable
    /// fragment head produces no delivery record — the group is not
    /// yet complete — so a receive path that learns the mode
    /// boundary from records never learns it from a fragmented
    /// message's first packet. Here the prefix the boundary
    /// concedes is a genuinely lost fire-and-forget sequence: no
    /// retransmit can ever produce it, so a consumer that has not
    /// been told where the reliable region starts holds the
    /// assembled message forever behind a hole nothing can fill.
    #[test]
    fn a_boundary_on_a_fragment_head_concedes_the_lost_prefix_before_the_group_completes() {
        const STREAM: u64 = crate::stream::LEAF_STREAM_DISCRIMINATOR | 0x5C;
        let (mut a, mut b) = pair();
        let bid = b.node_id();
        let faf = a
            .open_stream(bid, "", Reliability::FireAndForget, Some(STREAM), None)
            .expect("fire-and-forget handle");
        a.stream_send(faf, b"faf-0").expect("send");
        assert_eq!(pump(&mut a, &mut b), 1);
        assert_eq!(delivered(&mut b), vec![b"faf-0".to_vec()]);

        // Sequence 1, fire-and-forget and genuinely lost: its sender
        // retained nothing, so it is unrecoverable by construction.
        a.stream_send(faf, b"faf-1").expect("send");
        assert_eq!(a.take_outbound().len(), 1, "dropped, never rebuilt");

        // The promotion arrives as a two-fragment reliable message,
        // so the boundary rides a packet that completes nothing.
        let reliable = a
            .open_stream(bid, "", Reliability::Reliable, Some(STREAM), None)
            .expect("reliable handle");
        let body = vec![0x5a; 9000];
        a.stream_send(reliable, &body).expect("send");
        let pieces = a.take_outbound();
        assert_eq!(pieces.len(), 2, "the boundary is on a fragment head");
        for piece in pieces {
            b.on_datagram(a.node_id(), piece.packet, clock::now());
        }
        assert_eq!(
            delivered(&mut b),
            vec![body],
            "the head's boundary concedes the lost fire-and-forget \
             prefix, so the completed reliable message is delivered"
        );
    }
}
