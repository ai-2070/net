//! `RtcLeafTransport` — one `RTCPeerConnection` and one
//! `RTCDataChannel` per peer, on the main thread.
//!
//! **Main thread, not a worker.** S0b confirmed
//! `RTCPeerConnection` is `ReferenceError: not defined` in both a
//! dedicated `Worker` and a `SharedWorker` on Chromium 149. That is
//! why this module is not `Send`, why the
//! [`ControlPlane`] trait is not
//! `Send`, and why §8 elects a leader tab instead of moving the node
//! off the UI thread.
//!
//! # The §2 send rules, as they land in a browser
//!
//! §2 separates a **hard bound** from an **advisory reading**:
//!
//! - the hard bound is reserved slots and bytes, accounted at
//!   admission (`SEND_QUEUE_PACKETS`, `SEND_QUEUE_BYTES`).
//!   Exceeding it refuses from the send call itself, with nothing
//!   enqueued;
//! - the advisory is the SCTP buffered-amount reading
//!   (`BUFFERED_AMOUNT_ADVISORY`, 96 KiB — Stage 3's corrected
//!   default, below str0m's non-configurable 128 KiB cap, which is
//!   why the original 256 KiB could never fire). It may refuse
//!   earlier; it never defines the bound.
//!
//! One thing is genuinely better here than on the native side.
//! str0m's `Channel::buffered_amount` needs `&mut Rtc`, so a native
//! reading is a snapshot stale by up to one drain (S0b measured
//! 8 089 B). `RTCDataChannel.bufferedAmount` is a live property
//! readable at any time, so the leaf's advisory precheck is exact at
//! the moment of the call. The hard bound still exists, because
//! `send` can throw after a passing precheck.
//!
//! **Retention, not drop.** S0b's drop policy silently lost 19 476
//! packets. A packet accepted at admission is retained and retried
//! on `bufferedamountlow`; only a channel that closes discards, and
//! the count discarded at close is counted.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit,
    RtcDataChannelState, RtcDataChannelType, RtcIceCandidate, RtcIceCandidateInit,
    RtcIceConnectionState, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
    RtcSessionDescriptionInit,
};

use crate::control_plane::{IceCandidate, NodeId, Sdp};
use crate::counters::RtcLinkSnapshot;
use crate::error::{LeafError, Result, RtcError};
use crate::retry::{IceLinkState, IceWatch};

/// The live half of [`RtcLinkSnapshot`]: the transport's own ledger,
/// in the field names the NATIVE `RtcStats` uses.
///
/// `Cell`, not atomics, for the reason [`crate::counters`] gives:
/// this crate runs on the browser main thread and an atomic would
/// buy nothing on a per-packet path.
///
/// Every term here is one the native driver also records, under the
/// same name. The ones a leaf has no mechanism for are NOT fields —
/// [`crate::counters::RTC_STATS_NOT_APPLICABLE`] names them and says
/// why, because a counter frozen at zero reads as an observation.
#[derive(Debug, Default)]
pub struct RtcLinkCounters {
    accepted: Cell<u64>,
    written: Cell<u64>,
    write_false: Cell<u64>,
    discarded_at_close: Cell<u64>,
    max_buffered: Cell<u64>,
    admission_refused_slots: Cell<u64>,
    admission_refused_bytes: Cell<u64>,
    admission_refused_advisory: Cell<u64>,
    admission_refused_unknown_peer: Cell<u64>,
    ingress_delivered: Cell<u64>,
}

impl RtcLinkCounters {
    /// One packet admitted into a peer's retained queue. Past this
    /// line the transport owns it (§2).
    #[inline]
    fn note_accepted(&self) {
        bump(&self.accepted);
    }

    /// One packet `RTCDataChannel.send` took.
    #[inline]
    fn note_written(&self) {
        bump(&self.written);
    }

    /// `send` threw after a passing precheck.
    ///
    /// Native spells this `write_false` because str0m's
    /// `Channel::write` returns `Ok(false)`; the browser throws
    /// instead. Same fact, same name: **not a loss** — the packet is
    /// still at the head of the queue and `bufferedamountlow`
    /// retries it.
    #[inline]
    fn note_write_false(&self) {
        bump(&self.write_false);
    }

    /// `n` packets were still retained when a channel closed. The
    /// one place an admitted packet is lost, and it is counted.
    #[inline]
    fn note_discarded_at_close_n(&self, n: u64) {
        self.discarded_at_close
            .set(self.discarded_at_close.get().saturating_add(n));
    }

    /// Record an observed `bufferedAmount`.
    ///
    /// Exact here, where native's is a snapshot stale by up to one
    /// drain: `RTCDataChannel.bufferedAmount` is a live property.
    #[inline]
    fn observe_buffered(&self, amount: u32) {
        self.max_buffered
            .set(self.max_buffered.get().max(u64::from(amount)));
    }

    /// Admission refused: the reserved packet slots are exhausted.
    #[inline]
    fn note_refused_slots(&self) {
        bump(&self.admission_refused_slots);
    }

    /// Admission refused: the reserved byte budget is exhausted.
    #[inline]
    fn note_refused_bytes(&self) {
        bump(&self.admission_refused_bytes);
    }

    /// Admission refused: the live `bufferedAmount` is at or over
    /// the advisory threshold.
    #[inline]
    fn note_refused_advisory(&self) {
        bump(&self.admission_refused_advisory);
    }

    /// Admission refused for a peer this transport cannot address.
    ///
    /// Native's term is "a peer the driver does not hold — a closed
    /// session, or a handle whose generation is spent", and both
    /// leaf cases are that peer: no link at all, or a link whose
    /// channel is not open. They are distinguished in the typed
    /// error the caller receives; the counter is one, as it is
    /// natively.
    #[inline]
    fn note_refused_unknown_peer(&self) {
        bump(&self.admission_refused_unknown_peer);
    }

    /// One DataChannel message handed to the node.
    #[inline]
    fn note_ingress_delivered(&self) {
        bump(&self.ingress_delivered);
    }

    /// One coherent reading, with `retained` — a gauge, not a
    /// counter — supplied by the caller that can see the queues.
    fn snapshot(&self, retained: u64) -> RtcLinkSnapshot {
        RtcLinkSnapshot {
            accepted: self.accepted.get(),
            written: self.written.get(),
            write_false: self.write_false.get(),
            retained,
            discarded_at_close: self.discarded_at_close.get(),
            max_buffered: self.max_buffered.get(),
            admission_refused_slots: self.admission_refused_slots.get(),
            admission_refused_bytes: self.admission_refused_bytes.get(),
            admission_refused_advisory: self.admission_refused_advisory.get(),
            admission_refused_unknown_peer: self.admission_refused_unknown_peer.get(),
            ingress_delivered: self.ingress_delivered.get(),
        }
    }
}

#[inline]
fn bump(cell: &Cell<u64>) {
    cell.set(cell.get().saturating_add(1));
}

/// The advisory SCTP buffered-amount threshold, in bytes.
///
/// Stage 3's corrected default. Below str0m's 128 KiB
/// `MAX_BUFFERED_ACROSS_STREAMS`, so it can actually fire against a
/// native peer.
pub const BUFFERED_AMOUNT_ADVISORY: u32 = 96 * 1024;

/// `bufferedAmountLowThreshold`: where the browser tells us to
/// resume. Half the advisory, so a resume leaves room for a burst
/// rather than immediately re-tripping the precheck.
pub const BUFFERED_AMOUNT_LOW: u32 = BUFFERED_AMOUNT_ADVISORY / 2;

/// Hard bound: retained packets per peer.
pub const SEND_QUEUE_PACKETS: usize = 1_024;

/// Hard bound: retained bytes per peer.
pub const SEND_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// The DataChannel label both halves agree on.
///
/// One channel per peer (§3), named so a native peer's driver can
/// recognise it.
pub const CHANNEL_LABEL: &str = "net";

/// What the transport hands back for each inbound message.
pub type InboundSink = Rc<dyn Fn(NodeId, Bytes)>;

/// One peer's connection.
struct PeerLink {
    connection: RtcPeerConnection,
    channel: Option<RtcDataChannel>,
    /// Retained packets accepted at admission but not yet written.
    retained: VecDeque<Bytes>,
    retained_bytes: usize,
    discarded_at_close: u64,
    /// Kept alive for the connection's lifetime; dropping a closure
    /// would detach the JS callback.
    _closures: Vec<Closure<dyn FnMut(JsValue)>>,
    _message: Option<Closure<dyn FnMut(MessageEvent)>>,
    _ice: Option<Closure<dyn FnMut(RtcPeerConnectionIceEvent)>>,
    /// The answerer's `ondatachannel` handler. `None` on the
    /// offering side, which creates the channel itself.
    _data_channel: Option<Closure<dyn FnMut(RtcDataChannelEvent)>>,
}

impl Drop for PeerLink {
    /// A link that goes away closes what it owns.
    ///
    /// The link **is** the ownership of an `RTCPeerConnection` and
    /// its channel, so releasing it has to be the close — not a
    /// step some caller remembers to take first. Every path that
    /// drops a link is a path on which the browser must stop
    /// gathering, stop keeping an ICE agent and stop holding an
    /// SCTP association for a peer nobody is talking to any more:
    /// [`RtcLeafTransport::close`] removing one, the whole map
    /// going away with the transport, and — the case this exists
    /// for — a **cancelled** bootstrap, whose future is simply
    /// dropped. `wasm::LeafNode::connect` creates the link before
    /// its first suspension, so a promotion cancelled while it
    /// waited on an answer, a channel or enrollment left a live
    /// `RTCPeerConnection` behind with no owner left to close it,
    /// and did so *before* releasing the origin's lock to a
    /// successor.
    ///
    /// `close()` on an already-closed connection is a no-op in
    /// every engine, so the explicit path and this one compose.
    fn drop(&mut self) {
        if let Some(channel) = &self.channel {
            channel.close();
        }
        self.connection.close();
    }
}

/// The leaf's RTC transport.
///
/// `Clone` is a refcount bump on shared interior state, and it is
/// load-bearing: the async methods below hold a handle across an
/// `await`, so a caller that reached the transport through a
/// `RefCell` must clone it out and drop the borrow first. Holding a
/// `RefCell` borrow across an await would deadlock against the pump
/// on the very next tick.
#[derive(Clone)]
pub struct RtcLeafTransport {
    peers: Rc<RefCell<HashMap<NodeId, PeerLink>>>,
    inbound: InboundSink,
    /// Locally gathered candidates, drained by the caller and given
    /// to the control plane.
    local_candidates: Rc<RefCell<VecDeque<(NodeId, IceCandidate)>>>,
    next_slot: Rc<core::cell::Cell<u32>>,
    /// The transport's own `RtcStats`, in the native field names.
    stats: Rc<RtcLinkCounters>,
    /// Peers whose ICE walked `disconnected` → `failed`, filed for
    /// the caller to drain.
    ///
    /// A queue rather than a call into the node, for the reason
    /// `local_candidates` is one: this is written from inside a JS
    /// event callback, which must not re-enter a `RefCell` the pump
    /// may already hold. [`crate::wasm`] drains it on the tick and
    /// files each entry as a trigger, so the ICE source and the
    /// `online` source reach ONE owner
    /// ([`crate::retry::RetryPolicy`]) rather than deciding for
    /// themselves.
    ice_failures: Rc<RefCell<VecDeque<NodeId>>>,
}

impl RtcLeafTransport {
    /// A transport that delivers every inbound message to `inbound`.
    pub fn new(inbound: InboundSink) -> Self {
        Self {
            peers: Rc::new(RefCell::new(HashMap::new())),
            inbound,
            local_candidates: Rc::new(RefCell::new(VecDeque::new())),
            next_slot: Rc::new(core::cell::Cell::new(0)),
            stats: Rc::new(RtcLinkCounters::default()),
            ice_failures: Rc::new(RefCell::new(VecDeque::new())),
        }
    }

    /// The transport's ledger, in the native `RtcStats` field names.
    ///
    /// `retained` is a GAUGE and is counted here rather than
    /// tracked: it is exactly what is in the queues right now, so
    /// reading it from them cannot drift from them. Native carries
    /// the same term for the same reason — the retry slot is finite
    /// storage outside the queue reservation, so
    /// `accepted == written + discarded_at_close` is not the
    /// conservation law and this is the missing term.
    pub fn link_snapshot(&self) -> RtcLinkSnapshot {
        let retained = self
            .peers
            .borrow()
            .values()
            .map(|link| link.retained.len() as u64)
            .sum();
        self.stats.snapshot(retained)
    }

    /// Take the peers whose ICE walked `disconnected` → `failed`
    /// since the last call.
    pub fn take_ice_failures(&self) -> Vec<NodeId> {
        self.ice_failures.borrow_mut().drain(..).collect()
    }

    /// `peer`'s live `iceConnectionState`, as the engine spells it.
    ///
    /// Read off the `RTCPeerConnection` rather than remembered, so a
    /// report says what the browser believes and not what this
    /// module last heard.
    pub fn ice_connection_state(&self, peer: NodeId) -> Option<&'static str> {
        self.peers
            .borrow()
            .get(&peer)
            .map(|link| match link.connection.ice_connection_state() {
                RtcIceConnectionState::New => "new",
                RtcIceConnectionState::Checking => "checking",
                RtcIceConnectionState::Connected => "connected",
                RtcIceConnectionState::Completed => "completed",
                RtcIceConnectionState::Disconnected => "disconnected",
                RtcIceConnectionState::Failed => "failed",
                RtcIceConnectionState::Closed => "closed",
                _ => "unknown",
            })
    }

    /// The slot a new peer's `PeerAddr::Rtc` handle takes.
    pub fn next_slot(&self) -> u32 {
        let slot = self.next_slot.get();
        self.next_slot.set(slot.wrapping_add(1));
        slot
    }

    /// Create the connection and the channel for `peer`, and produce
    /// the local offer.
    ///
    /// The channel is created **before** the offer, which is what
    /// puts the SCTP m-line in the SDP; an offer without it would
    /// negotiate a connection with nothing to carry.
    pub async fn create_offer(&self, peer: NodeId, ice_servers: &[IceServer]) -> Result<Sdp> {
        let connection = new_connection(ice_servers)?;

        // Reliable, ordered: the Net layer's own reliability rides
        // inside, and an unordered channel would hand the consumer
        // reorder work the stream layer already does — but at the
        // packet level, where the AEAD replay window would refuse
        // it.
        let init = RtcDataChannelInit::new();
        init.set_ordered(true);
        let channel = connection.create_data_channel_with_data_channel_dict(CHANNEL_LABEL, &init);
        channel.set_binary_type(RtcDataChannelType::Arraybuffer);
        channel.set_buffered_amount_low_threshold(BUFFERED_AMOUNT_LOW);

        let message = message_handler(
            Rc::clone(&self.inbound),
            Rc::clone(&self.stats),
            peer,
            &channel,
        );
        let ice = self.install_ice_handler(peer, &connection);
        let ice_state = self.install_ice_state_handler(peer, &connection);
        let low = low_water_handler(
            Rc::downgrade(&self.peers),
            Rc::clone(&self.stats),
            peer,
            &channel,
        );

        self.peers.borrow_mut().insert(
            peer,
            PeerLink {
                connection: connection.clone(),
                channel: Some(channel),
                retained: VecDeque::new(),
                retained_bytes: 0,
                discarded_at_close: 0,
                _closures: vec![low, ice_state],
                _message: Some(message),
                _ice: Some(ice),
                _data_channel: None,
            },
        );

        let offer = wasm_bindgen_futures::JsFuture::from(connection.create_offer())
            .await
            .map_err(|e| unsupported("createOffer", &e))?;
        let sdp = js_sys::Reflect::get(&offer, &JsValue::from_str("sdp"))
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| LeafError::Rtc(RtcError::Unsupported("offer carried no sdp".into())))?;

        let description = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
        description.set_sdp(&sdp);
        wasm_bindgen_futures::JsFuture::from(connection.set_local_description(&description))
            .await
            .map_err(|e| unsupported("setLocalDescription", &e))?;
        Ok(Sdp(sdp))
    }

    /// Answer a peer's offer, and produce the local answer.
    ///
    /// The counterpart of [`Self::create_offer`], and §9's browser ↔
    /// browser attempt needs it: with no anchor in the middle, one
    /// of the two leaves must be the answerer.
    ///
    /// **No channel is created here.** The offerer created it, so
    /// the answerer installs its handlers when `ondatachannel`
    /// fires; creating a second one would negotiate two SCTP
    /// streams and make §3's "one channel per peer" false. Until
    /// that event arrives [`Self::is_open`] is `false` and
    /// [`Self::send`] refuses with
    /// [`RtcError::ChannelClosed`] — the same refusal the offering
    /// side gets before its channel opens, and every §2 bound
    /// (reserved slots, reserved bytes, the buffered-amount
    /// advisory) applies afterwards through the same `send`.
    pub async fn accept_offer(
        &self,
        peer: NodeId,
        offer: &Sdp,
        ice_servers: &[IceServer],
    ) -> Result<Sdp> {
        let connection = new_connection(ice_servers)?;
        let ice = self.install_ice_handler(peer, &connection);
        let ice_state = self.install_ice_state_handler(peer, &connection);

        // **`Weak`.** The handler lives in the `PeerLink` this map
        // holds, so a strong clone here would be a cycle — map →
        // link → closure → map — and a cycle is a map whose
        // refcount never reaches zero, i.e. a transport that can be
        // dropped without closing a single connection.
        let peers = Rc::downgrade(&self.peers);
        let inbound = Rc::clone(&self.inbound);
        let stats = Rc::clone(&self.stats);
        let on_data_channel = Closure::wrap(Box::new(move |event: RtcDataChannelEvent| {
            let channel = event.channel();
            channel.set_binary_type(RtcDataChannelType::Arraybuffer);
            channel.set_buffered_amount_low_threshold(BUFFERED_AMOUNT_LOW);
            let Some(peers) = peers.upgrade() else {
                return;
            };
            let message = message_handler(Rc::clone(&inbound), Rc::clone(&stats), peer, &channel);
            // The low-water handler is what retries a retained
            // packet; an accepted channel without it would retain
            // for ever after the first refusal.
            let low = low_water_handler(Rc::downgrade(&peers), Rc::clone(&stats), peer, &channel);
            let mut links = peers.borrow_mut();
            if let Some(link) = links.get_mut(&peer) {
                // The first channel the peer opens is the Net one:
                // a leaf offers exactly one and accepts exactly one
                // (§3), so matching on the label as well would turn
                // a label disagreement into a silent hang instead of
                // a handshake that fails with a reason.
                if link.channel.is_none() {
                    link._message = Some(message);
                    link._closures.push(low);
                    link.channel = Some(channel);
                }
            }
        }) as Box<dyn FnMut(RtcDataChannelEvent)>);
        connection.set_ondatachannel(Some(on_data_channel.as_ref().unchecked_ref()));

        self.peers.borrow_mut().insert(
            peer,
            PeerLink {
                connection: connection.clone(),
                channel: None,
                retained: VecDeque::new(),
                retained_bytes: 0,
                discarded_at_close: 0,
                _closures: vec![ice_state],
                _message: None,
                _ice: Some(ice),
                _data_channel: Some(on_data_channel),
            },
        );

        // The remote description first: `createAnswer` has nothing
        // to answer without it.
        let remote = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
        remote.set_sdp(&offer.0);
        wasm_bindgen_futures::JsFuture::from(connection.set_remote_description(&remote))
            .await
            .map_err(|e| unsupported("setRemoteDescription", &e))?;

        let answer = wasm_bindgen_futures::JsFuture::from(connection.create_answer())
            .await
            .map_err(|e| unsupported("createAnswer", &e))?;
        let sdp = js_sys::Reflect::get(&answer, &JsValue::from_str("sdp"))
            .ok()
            .and_then(|v| v.as_string())
            .ok_or_else(|| LeafError::Rtc(RtcError::Unsupported("answer carried no sdp".into())))?;

        let description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        description.set_sdp(&sdp);
        wasm_bindgen_futures::JsFuture::from(connection.set_local_description(&description))
            .await
            .map_err(|e| unsupported("setLocalDescription", &e))?;
        Ok(Sdp(sdp))
    }

    /// Apply the peer's answer.
    pub async fn accept_answer(&self, peer: NodeId, answer: &Sdp) -> Result<()> {
        let connection = self
            .peers
            .borrow()
            .get(&peer)
            .map(|link| link.connection.clone())
            .ok_or_else(|| LeafError::Session(format!("no attempt for {peer:#x}")))?;
        let description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        description.set_sdp(&answer.0);
        wasm_bindgen_futures::JsFuture::from(connection.set_remote_description(&description))
            .await
            .map_err(|e| unsupported("setRemoteDescription", &e))?;
        Ok(())
    }

    /// Add one remote candidate.
    pub async fn add_remote_candidate(&self, peer: NodeId, candidate: &IceCandidate) -> Result<()> {
        let connection = self
            .peers
            .borrow()
            .get(&peer)
            .map(|link| link.connection.clone())
            .ok_or_else(|| LeafError::Session(format!("no attempt for {peer:#x}")))?;
        let init = RtcIceCandidateInit::new(&candidate.candidate);
        init.set_sdp_mid(Some(&candidate.mid));
        let candidate = RtcIceCandidate::new(&init).map_err(|e| unsupported("candidate", &e))?;
        wasm_bindgen_futures::JsFuture::from(
            connection.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&candidate)),
        )
        .await
        .map_err(|e| unsupported("addIceCandidate", &e))?;
        Ok(())
    }

    /// Take the candidates gathered locally since the last call.
    pub fn take_local_candidates(&self) -> Vec<(NodeId, IceCandidate)> {
        self.local_candidates.borrow_mut().drain(..).collect()
    }

    /// Whether `peer`'s channel is open.
    pub fn is_open(&self, peer: NodeId) -> bool {
        self.peers
            .borrow()
            .get(&peer)
            .and_then(|link| link.channel.as_ref())
            .is_some_and(|channel| channel.ready_state() == RtcDataChannelState::Open)
    }

    /// Send one packet under the §2 rules.
    ///
    /// Refuses at admission when the hard bound or the advisory
    /// threshold says so, with nothing enqueued. Once accepted the
    /// packet is the transport's: it is written now or retained and
    /// retried, never dropped while the channel lives.
    pub fn send(&self, peer: NodeId, packet: Bytes) -> Result<()> {
        let mut peers = self.peers.borrow_mut();
        let link = peers.get_mut(&peer).ok_or_else(|| {
            self.stats.note_refused_unknown_peer();
            LeafError::Session(format!("no transport for {peer:#x}"))
        })?;
        let channel = link
            .channel
            .as_ref()
            .ok_or_else(|| {
                self.stats.note_refused_unknown_peer();
                LeafError::Rtc(RtcError::ChannelClosed("no channel".into()))
            })?
            .clone();
        if channel.ready_state() != RtcDataChannelState::Open {
            self.stats.note_refused_unknown_peer();
            return Err(LeafError::Rtc(RtcError::ChannelClosed(format!(
                "channel for {peer:#x} is {:?}",
                channel.ready_state()
            ))));
        }

        // Hard bound first: it is the one that actually bounds. The
        // two halves are counted apart, as they are natively, so a
        // witness expecting "no refusals" can say which bound fired.
        if link.retained.len() >= SEND_QUEUE_PACKETS {
            self.stats.note_refused_slots();
            return Err(LeafError::Wire(format!(
                "send queue for {peer:#x} is full ({} packets, {} bytes retained)",
                link.retained.len(),
                link.retained_bytes
            )));
        }
        if link.retained_bytes + packet.len() > SEND_QUEUE_BYTES {
            self.stats.note_refused_bytes();
            return Err(LeafError::Wire(format!(
                "send queue for {peer:#x} is full ({} packets, {} bytes retained)",
                link.retained.len(),
                link.retained_bytes
            )));
        }
        // Advisory: may refuse earlier, never defines the bound.
        let buffered = channel.buffered_amount();
        self.stats.observe_buffered(buffered);
        if buffered >= BUFFERED_AMOUNT_ADVISORY {
            self.stats.note_refused_advisory();
            return Err(LeafError::Wire(format!(
                "SCTP buffered amount for {peer:#x} is {buffered} bytes, at or over the \
                 {BUFFERED_AMOUNT_ADVISORY}-byte advisory"
            )));
        }

        self.stats.note_accepted();
        link.retained_bytes += packet.len();
        link.retained.push_back(packet);
        flush_link(link, &channel, &self.stats);
        Ok(())
    }

    /// Close `peer`'s connection, reporting how many retained
    /// packets were discarded.
    pub fn close(&self, peer: NodeId) -> u64 {
        let Some(mut link) = self.peers.borrow_mut().remove(&peer) else {
            return 0;
        };
        link.discarded_at_close += link.retained.len() as u64;
        self.stats
            .note_discarded_at_close_n(link.retained.len() as u64);
        // Taking the link out of the map is the close: `PeerLink`'s
        // `Drop` shuts the channel and the connection.
        link.discarded_at_close
    }

    /// The browser's own `RTCPeerConnection` for `peer`, when this
    /// transport has one.
    ///
    /// The single seam through which a caller can observe what the
    /// engine believes about a connection this transport owns — its
    /// `connectionState`, its effective configuration — rather than
    /// about the arguments we passed in. That distinction is the
    /// whole of "the cancelled attempt's connection was really
    /// closed": a transport-side count of zero peers is equally
    /// consistent with a map that was merely emptied.
    pub fn peer_connection(&self, peer: NodeId) -> Option<RtcPeerConnection> {
        self.peers
            .borrow()
            .get(&peer)
            .map(|link| link.connection.clone())
    }

    /// Close everything.
    pub fn close_all(&self) {
        let peers: Vec<NodeId> = self.peers.borrow().keys().copied().collect();
        for peer in peers {
            self.close(peer);
        }
    }

    fn install_ice_handler(
        &self,
        peer: NodeId,
        connection: &RtcPeerConnection,
    ) -> Closure<dyn FnMut(RtcPeerConnectionIceEvent)> {
        let pending = Rc::clone(&self.local_candidates);
        let closure = Closure::wrap(Box::new(move |event: RtcPeerConnectionIceEvent| {
            if let Some(candidate) = event.candidate() {
                let line = candidate.candidate();
                if line.is_empty() {
                    // End-of-candidates.
                    return;
                }
                pending.borrow_mut().push_back((
                    peer,
                    IceCandidate {
                        mid: candidate.sdp_mid().unwrap_or_default(),
                        candidate: line,
                    },
                ));
            }
        }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
        connection.set_onicecandidate(Some(closure.as_ref().unchecked_ref()));
        closure
    }

    /// Watch `peer`'s ICE for the one transition that is a network
    /// change: `disconnected` → `failed`.
    ///
    /// The decision is [`IceWatch`]'s, not this closure's — it is a
    /// rule about transitions rather than states, and it is
    /// asserted on the host ([`crate::retry`]) rather than hoped for
    /// in a browser. All this does is map the engine's enum onto the
    /// one the rule reads and file the peer when the rule fires.
    ///
    /// **Filed, not acted on.** This runs inside a JS event
    /// callback, and the owner it feeds lives behind the same
    /// `RefCell` the pump may already hold. So it queues, and
    /// [`crate::wasm`] drains on the tick — which is also what makes
    /// the `online` source and this one ONE owner rather than two
    /// callbacks that each decide.
    fn install_ice_state_handler(
        &self,
        peer: NodeId,
        connection: &RtcPeerConnection,
    ) -> Closure<dyn FnMut(JsValue)> {
        let failures = Rc::downgrade(&self.ice_failures);
        let watched = connection.clone();
        let mut watch = IceWatch::default();
        let closure = Closure::wrap(Box::new(move |_event: JsValue| {
            let state = match watched.ice_connection_state() {
                RtcIceConnectionState::New => IceLinkState::New,
                RtcIceConnectionState::Checking => IceLinkState::Checking,
                RtcIceConnectionState::Connected => IceLinkState::Connected,
                RtcIceConnectionState::Completed => IceLinkState::Completed,
                RtcIceConnectionState::Disconnected => IceLinkState::Disconnected,
                RtcIceConnectionState::Failed => IceLinkState::Failed,
                // `closed`, and anything a future engine adds: not a
                // loss. A state this build does not know is not
                // evidence of one.
                _ => IceLinkState::Closed,
            };
            if !watch.observe(state) {
                return;
            }
            if let Some(failures) = failures.upgrade() {
                failures.borrow_mut().push_back(peer);
            }
        }) as Box<dyn FnMut(JsValue)>);
        connection.set_oniceconnectionstatechange(Some(closure.as_ref().unchecked_ref()));
        closure
    }
}

/// Write as much of the retained queue as the channel will take.
///
/// `send` throwing is not a lost packet: the packet stays at the head
/// of the queue and the next `bufferedamountlow` retries it. That is
/// the retain-and-retry policy S0b's 19 476 lost packets bought.
fn flush_link(link: &mut PeerLink, channel: &RtcDataChannel, stats: &RtcLinkCounters) {
    while let Some(front) = link.retained.front() {
        let buffered = channel.buffered_amount();
        stats.observe_buffered(buffered);
        if buffered >= BUFFERED_AMOUNT_ADVISORY {
            return;
        }
        // `send_with_u8_array` copies into the SCTP buffer, so the
        // borrow ends with the call.
        match channel.send_with_u8_array(front) {
            Ok(()) => {
                stats.note_written();
                let sent = link.retained.pop_front().map_or(0, |p| p.len());
                link.retained_bytes = link.retained_bytes.saturating_sub(sent);
            }
            Err(_) => {
                // Native spells this `write_false`. NOT a loss: the
                // packet is still at the head of the queue and
                // `bufferedamountlow` retries it.
                stats.note_write_false();
                return;
            }
        }
    }
}

/// One ICE server, exactly as much of `RTCIceServer` as the leaf
/// configures.
///
/// The page-facing option is a web `RTCIceServer` — `urls` is a
/// string *or* an array of them, and a TURN entry carries
/// `username`/`credential`. Stage 5 shipped a `Vec<String>` here,
/// which meant a TURN server configured by a page reached the
/// `RTCPeerConnection` without its credentials, i.e. not at all.
/// The parse lives at the wasm boundary
/// ([`crate::wasm::LeafNode::effective_ice_servers`]); this is the
/// checked shape it produces and the only one the transport reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IceServer {
    /// One or more URLs for the same server. Never empty.
    pub urls: Vec<String>,
    /// TURN username, when the entry has one.
    pub username: Option<String>,
    /// TURN credential, when the entry has one.
    pub credential: Option<String>,
}

/// A peer connection configured with `ice_servers`.
///
/// Shared by the offering and the answering path: two connections
/// built from two copies of this would be two chances to configure
/// ICE differently, and a browser ↔ browser attempt where only one
/// side has a STUN server is a connectivity bug with no symptom
/// except a deadline.
///
/// Public because it is the single point where a page's declared ICE
/// configuration becomes the browser's *effective* one, and that is
/// a property worth asserting against a real `RTCPeerConnection`
/// rather than against the argument we passed in.
pub fn new_connection(ice_servers: &[IceServer]) -> Result<RtcPeerConnection> {
    let config = RtcConfiguration::new();
    if !ice_servers.is_empty() {
        let servers = js_sys::Array::new();
        for entry in ice_servers {
            let server = js_sys::Object::new();
            let urls = js_sys::Array::new();
            for url in &entry.urls {
                urls.push(&JsValue::from_str(url));
            }
            js_sys::Reflect::set(&server, &JsValue::from_str("urls"), &urls)
                .map_err(|e| unsupported("iceServers", &e))?;
            for (key, value) in [
                ("username", &entry.username),
                ("credential", &entry.credential),
            ] {
                let Some(value) = value else { continue };
                js_sys::Reflect::set(&server, &JsValue::from_str(key), &JsValue::from_str(value))
                    .map_err(|e| unsupported("iceServers", &e))?;
            }
            servers.push(&server);
        }
        config.set_ice_servers(&servers);
    }
    RtcPeerConnection::new_with_configuration(&config)
        .map_err(|e| unsupported("RTCPeerConnection", &e))
}

/// The inbound handler for one peer's channel.
fn message_handler(
    inbound: InboundSink,
    stats: Rc<RtcLinkCounters>,
    peer: NodeId,
    channel: &RtcDataChannel,
) -> Closure<dyn FnMut(MessageEvent)> {
    let closure = Closure::wrap(Box::new(move |event: MessageEvent| {
        let data = event.data();
        let bytes = if let Some(buffer) = data.dyn_ref::<js_sys::ArrayBuffer>() {
            Uint8Array::new(buffer).to_vec()
        } else if let Some(array) = data.dyn_ref::<Uint8Array>() {
            array.to_vec()
        } else {
            // A text frame on the Net channel is not ours.
            return;
        };
        stats.note_ingress_delivered();
        inbound(peer, Bytes::from(bytes));
    }) as Box<dyn FnMut(MessageEvent)>);
    channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure
}

/// The `bufferedamountlow` handler: the retain-and-retry half of
/// the §2 send rules.
fn low_water_handler(
    peers: Weak<RefCell<HashMap<NodeId, PeerLink>>>,
    stats: Rc<RtcLinkCounters>,
    peer: NodeId,
    channel: &RtcDataChannel,
) -> Closure<dyn FnMut(JsValue)> {
    let closure = Closure::wrap(Box::new(move |_event: JsValue| {
        // `Weak`, and that is load-bearing rather than defensive:
        // this closure is retained by the very `PeerLink` the map
        // holds, so a strong reference would make the map immortal
        // and every connection it owns unclosable by drop.
        let Some(peers) = peers.upgrade() else {
            return;
        };
        let mut peers = peers.borrow_mut();
        if let Some(link) = peers.get_mut(&peer) {
            if let Some(channel) = link.channel.clone() {
                flush_link(link, &channel, &stats);
            }
        }
    }) as Box<dyn FnMut(JsValue)>);
    channel.set_onbufferedamountlow(Some(closure.as_ref().unchecked_ref()));
    closure
}

fn unsupported(what: &str, error: &JsValue) -> LeafError {
    LeafError::Rtc(RtcError::Unsupported(format!(
        "{what}: {}",
        error.as_string().unwrap_or_else(|| format!("{error:?}"))
    )))
}
