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

/// Which end of a reliable stream gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFailure {
    /// This leaf resent a packet `max_retries` times and the peer
    /// never acknowledged it.
    RetransmitsExhausted,
    /// The peer sent a `StreamReset`: its own retransmits are
    /// exhausted, so the rest of the stream is never coming.
    PeerReset,
}

impl StreamFailure {
    /// The stable string the JSON event carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetransmitsExhausted => "retransmits_exhausted",
            Self::PeerReset => "peer_reset",
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

/// An open application stream's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHandle {
    /// The peer.
    pub peer: NodeId,
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
            next_nonce: 1,
            reply_subscriptions: std::collections::HashSet::new(),
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
        self.reassembler.expire(now, &self.counters);
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
                self.counters.drop_for(DropReason::StreamFailed);
                self.events.push(LeafEvent::StreamFailed {
                    peer_node: peer,
                    stream_id,
                    reason: StreamFailure::RetransmitsExhausted,
                });
            }
        }
    }

    /// Take everything queued for the transport.
    pub fn take_outbound(&mut self) -> Vec<Outbound> {
        self.outbound.drain(..).collect()
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
    pub fn open_stream(
        &mut self,
        peer: NodeId,
        label: &str,
        reliability: Reliability,
        stream_id: Option<u64>,
        channel_hash: Option<u16>,
    ) -> Result<StreamHandle> {
        if self.sessions.get(peer).is_none() {
            return Err(LeafError::Session(format!("no session with {peer:#x}")));
        }
        let stream_id = stream_id.unwrap_or_else(|| stream_id_from_label(label));
        // The registry is what the receive path classifies on: an
        // id this leaf opened as a stream is a stream, whatever
        // bits its hash happens to carry.
        self.stream_kinds
            .insert((peer, stream_id), StreamKind::Stream);
        Ok(StreamHandle {
            peer,
            stream_id,
            channel_hash: channel_hash.unwrap_or(0),
            reliability,
        })
    }

    /// Send on an open stream.
    pub fn stream_send(&mut self, handle: StreamHandle, payload: &[u8]) -> Result<()> {
        self.send_event_plane(
            handle.peer,
            handle.stream_id,
            handle.channel_hash,
            payload,
            handle.reliability.is_reliable(),
        )
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

        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS);
        let (call_id, receiver) = self
            .calls
            .register(
                CallOwner {
                    peer,
                    incarnation,
                    reply_route,
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
            match owed {
                Some((frame, true)) => self.send_stream_window(peer, incarnation, frame),
                Some((frame, false)) => self.maybe_grant(peer, incarnation, frame),
                None => {}
            }
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
            // sequence and header of its FIRST fragment, never of
            // whichever packet happened to complete it.
            match records
                .iter_mut()
                .find(|r| r.seq == assembled.meta.sequence && r.span == assembled.span)
            {
                Some(record) => record.payloads.push(assembled.data),
                None => records.push(StreamRecord {
                    seq: assembled.meta.sequence,
                    span: assembled.span,
                    stream_id: assembled.meta.stream_id,
                    origin_hash: assembled.meta.origin_hash,
                    channel_hash: assembled.meta.channel_hash,
                    payloads: vec![assembled.data],
                }),
            }
        }
        if records.is_empty() {
            return;
        }

        // The consumer-side reorder, per stream. Control
        // subprotocols ride the control stream and are not
        // reordered: a credit grant held behind a gap would deadlock
        // the very stream it is trying to refill.
        let delivered: Vec<StreamRecord> = if subprotocol_id == SUBPROTOCOL_EVENT_PLANE {
            let reliability = if opened.reliable {
                Reliability::Reliable
            } else {
                Reliability::FireAndForget
            };
            let mut out = Vec::new();
            for record in records {
                let stream = self
                    .rx_streams
                    .entry((incarnation, record.stream_id))
                    .or_insert_with(|| RxStream::new(reliability));
                out.extend(stream.accept(record, &self.counters));
            }
            out
        } else {
            records
        };
        for record in delivered {
            for payload in record.payloads.clone() {
                self.handle_event(peer, subprotocol_id, &record, payload, now);
            }
        }
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

    /// One decoded event.
    fn handle_event(
        &mut self,
        peer: NodeId,
        subprotocol_id: u16,
        record: &StreamRecord,
        payload: Bytes,
        now: Instant,
    ) {
        let Some(decoded) = dispatch::dispatch_event(subprotocol_id, payload, &self.counters)
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
                // The sender gave up. Nothing else is coming on that
                // stream, so the consumer is told rather than left
                // waiting on a hole it will never see filled.
                if let Some(incarnation) = self.sessions.get(peer).map(|s| s.incarnation()) {
                    self.rx_streams.remove(&(incarnation, reset.stream_id));
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
    fn handle_event_plane(&mut self, peer: NodeId, record: &StreamRecord, payload: Bytes) {
        match rpc_wire::decode_reply_frame(payload.clone()) {
            Ok(Some(frame)) => {
                // The reply's claim to the call is the triple, not
                // the call id: the authenticated peer it arrived
                // from, the incarnation of that session, and the
                // canonical reply route the frame declares. A frame
                // that carries no route cannot present one, so it
                // cannot end a call either.
                let Some(incarnation) = self.sessions.get(peer).map(|s| s.incarnation()) else {
                    self.counters.drop_for(DropReason::UnknownCall);
                    return;
                };
                let Some(reply_route) = rpc_wire::decode_route(&payload) else {
                    self.counters.drop_for(DropReason::UnknownCall);
                    return;
                };
                self.calls.deliver(
                    frame,
                    CallOwner {
                        peer,
                        incarnation,
                        reply_route,
                    },
                    &self.counters,
                );
            }
            // Well-formed but not the client half, or not an nRPC
            // frame at all: an application message.
            Ok(None) | Err(_) => match self.classify(peer, record.stream_id) {
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
            },
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
    /// `false` means refused, with [`DropReason::SignalRejected`]
    /// counted and [`LeafEvent::Dropped`] pushed.
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
    /// 3. [`SeenSignals::admit`] — one `(from, dialog, kind)` per
    ///    window, so an observer cannot re-offer a still-valid
    ///    envelope and restart an abandoned dialog;
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
        // Step 3.
        if !self.seen_signals.admit(&envelope, now_secs) {
            self.reject_signal();
            return false;
        }
        self.events.push(LeafEvent::Signal(envelope));
        true
    }

    fn reject_signal(&mut self) {
        self.counters.drop_for(DropReason::SignalRejected);
        self.events.push(LeafEvent::Dropped {
            reason: DropReason::SignalRejected,
        });
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
        assert_eq!(
            b.counters().drops(DropReason::ReorderBufferFull),
            0,
            "the hole was recovered, not abandoned"
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
}
