//! `RtcLeafTransport` — one `RTCPeerConnection` and one
//! `RTCDataChannel` per peer, on the main thread.
//!
//! **Main thread, not a worker.** S0b confirmed
//! `RTCPeerConnection` is `ReferenceError: not defined` in both a
//! dedicated `Worker` and a `SharedWorker` on Chromium 149. That is
//! why this module is not `Send`, why the
//! [`ControlPlane`](crate::control_plane::ControlPlane) trait is not
//! `Send`, and why §8 elects a leader tab instead of moving the node
//! off the UI thread.
//!
//! # The §2 send rules, as they land in a browser
//!
//! §2 separates a **hard bound** from an **advisory reading**:
//!
//! - the hard bound is reserved slots and bytes, accounted at
//!   admission ([`SEND_QUEUE_PACKETS`], [`SEND_QUEUE_BYTES`]).
//!   Exceeding it refuses from the send call itself, with nothing
//!   enqueued;
//! - the advisory is the SCTP buffered-amount reading
//!   ([`BUFFERED_AMOUNT_ADVISORY`], 96 KiB — Stage 3's corrected
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

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelInit, RtcDataChannelState,
    RtcDataChannelType, RtcIceCandidate, RtcIceCandidateInit, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit,
};

use crate::control_plane::{IceCandidate, NodeId, Sdp};
use crate::error::{LeafError, Result, RtcError};

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
}

impl RtcLeafTransport {
    /// A transport that delivers every inbound message to `inbound`.
    pub fn new(inbound: InboundSink) -> Self {
        Self {
            peers: Rc::new(RefCell::new(HashMap::new())),
            inbound,
            local_candidates: Rc::new(RefCell::new(VecDeque::new())),
            next_slot: Rc::new(core::cell::Cell::new(0)),
        }
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
    pub async fn create_offer(&self, peer: NodeId, ice_servers: &[String]) -> Result<Sdp> {
        let config = RtcConfiguration::new();
        if !ice_servers.is_empty() {
            let servers = js_sys::Array::new();
            for url in ice_servers {
                let server = js_sys::Object::new();
                let urls = js_sys::Array::new();
                urls.push(&JsValue::from_str(url));
                js_sys::Reflect::set(&server, &JsValue::from_str("urls"), &urls)
                    .map_err(|e| unsupported("iceServers", &e))?;
                servers.push(&server);
            }
            config.set_ice_servers(&servers);
        }
        let connection = RtcPeerConnection::new_with_configuration(&config)
            .map_err(|e| unsupported("RTCPeerConnection", &e))?;

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

        let message = self.install_message_handler(peer, &channel);
        let ice = self.install_ice_handler(peer, &connection);
        let low = self.install_low_water_handler(peer, &channel);

        self.peers.borrow_mut().insert(
            peer,
            PeerLink {
                connection: connection.clone(),
                channel: Some(channel),
                retained: VecDeque::new(),
                retained_bytes: 0,
                discarded_at_close: 0,
                _closures: vec![low],
                _message: Some(message),
                _ice: Some(ice),
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
        let link = peers
            .get_mut(&peer)
            .ok_or_else(|| LeafError::Session(format!("no transport for {peer:#x}")))?;
        let channel = link
            .channel
            .as_ref()
            .ok_or_else(|| LeafError::Rtc(RtcError::ChannelClosed("no channel".into())))?
            .clone();
        if channel.ready_state() != RtcDataChannelState::Open {
            return Err(LeafError::Rtc(RtcError::ChannelClosed(format!(
                "channel for {peer:#x} is {:?}",
                channel.ready_state()
            ))));
        }

        // Hard bound first: it is the one that actually bounds.
        if link.retained.len() >= SEND_QUEUE_PACKETS
            || link.retained_bytes + packet.len() > SEND_QUEUE_BYTES
        {
            return Err(LeafError::Wire(format!(
                "send queue for {peer:#x} is full ({} packets, {} bytes retained)",
                link.retained.len(),
                link.retained_bytes
            )));
        }
        // Advisory: may refuse earlier, never defines the bound.
        if channel.buffered_amount() >= BUFFERED_AMOUNT_ADVISORY {
            return Err(LeafError::Wire(format!(
                "SCTP buffered amount for {peer:#x} is {} bytes, at or over the \
                 {BUFFERED_AMOUNT_ADVISORY}-byte advisory",
                channel.buffered_amount()
            )));
        }

        link.retained_bytes += packet.len();
        link.retained.push_back(packet);
        flush_link(link, &channel);
        Ok(())
    }

    /// Close `peer`'s connection, reporting how many retained
    /// packets were discarded.
    pub fn close(&self, peer: NodeId) -> u64 {
        let Some(mut link) = self.peers.borrow_mut().remove(&peer) else {
            return 0;
        };
        link.discarded_at_close += link.retained.len() as u64;
        if let Some(channel) = &link.channel {
            channel.close();
        }
        link.connection.close();
        link.discarded_at_close
    }

    /// Close everything.
    pub fn close_all(&self) {
        let peers: Vec<NodeId> = self.peers.borrow().keys().copied().collect();
        for peer in peers {
            self.close(peer);
        }
    }

    fn install_message_handler(
        &self,
        peer: NodeId,
        channel: &RtcDataChannel,
    ) -> Closure<dyn FnMut(MessageEvent)> {
        let inbound = Rc::clone(&self.inbound);
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
            inbound(peer, Bytes::from(bytes));
        }) as Box<dyn FnMut(MessageEvent)>);
        channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
        closure
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

    fn install_low_water_handler(
        &self,
        peer: NodeId,
        channel: &RtcDataChannel,
    ) -> Closure<dyn FnMut(JsValue)> {
        let peers = Rc::clone(&self.peers);
        let closure = Closure::wrap(Box::new(move |_event: JsValue| {
            let mut peers = peers.borrow_mut();
            if let Some(link) = peers.get_mut(&peer) {
                if let Some(channel) = link.channel.clone() {
                    flush_link(link, &channel);
                }
            }
        }) as Box<dyn FnMut(JsValue)>);
        channel.set_onbufferedamountlow(Some(closure.as_ref().unchecked_ref()));
        closure
    }
}

/// Write as much of the retained queue as the channel will take.
///
/// `send` throwing is not a lost packet: the packet stays at the head
/// of the queue and the next `bufferedamountlow` retries it. That is
/// the retain-and-retry policy S0b's 19 476 lost packets bought.
fn flush_link(link: &mut PeerLink, channel: &RtcDataChannel) {
    while let Some(front) = link.retained.front() {
        if channel.buffered_amount() >= BUFFERED_AMOUNT_ADVISORY {
            return;
        }
        // `send_with_u8_array` copies into the SCTP buffer, so the
        // borrow ends with the call.
        match channel.send_with_u8_array(front) {
            Ok(()) => {
                let sent = link.retained.pop_front().map_or(0, |p| p.len());
                link.retained_bytes = link.retained_bytes.saturating_sub(sent);
            }
            Err(_) => return,
        }
    }
}

fn unsupported(what: &str, error: &JsValue) -> LeafError {
    LeafError::Rtc(RtcError::Unsupported(format!(
        "{what}: {}",
        error.as_string().unwrap_or_else(|| format!("{error:?}"))
    )))
}
