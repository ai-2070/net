//! The `wasm-bindgen` surface: `LeafNode` and `LeafStream` as
//! JavaScript sees them, and `AnchorControlPlane` behind them.
//!
//! ```text
//! class LeafNode {
//!   static connect(opts): Promise<LeafNode>
//!   node_id_hex(): string
//!   call(service, payload, timeout_ms?): Promise<Uint8Array>
//!   subscribe(channel): Promise<void>
//!   publish(channel, payload): Promise<void>
//!   open_stream(opts): LeafStream
//!   announce(capabilities): Promise<void>
//!   query(capability): Promise<string>
//!   on_event(cb): void
//!   close(): void
//! }
//! ```
//!
//! Errors cross as `JsError` whose message is
//! [`LeafError`]'s `Display`, and the TS
//! wrapper re-types them. Every `u64` crosses as a decimal
//! **string**: `JSON.parse` rounds integers above 2^53, so a numeric
//! `channel_hash` would silently name the wrong channel.
//!
//! # The control-plane boundary
//!
//! Everything that is not a Net packet crosses
//! [`ControlPlane`], whose one
//! v1 implementation is
//! `AnchorControlPlane`:
//! the anchor info and its pinned-key refusal, the offer, the
//! candidate trickle in both directions, and the end of the
//! attempt. This module **drives** that trait and
//! owns no `fetch`, no `WebSocket` and no SDP transport of its own —
//! `tests/control_plane_boundary.rs` asserts that from the outside,
//! because one inlined HTTP call here is how a boundary stops being
//! one.
//!
//! What deliberately stays on this side: the credential (the page's
//! own input), the Noise handshake, and the **enrollment exchange**.
//! Enrollment is an nRPC call on the session that was just
//! installed — the data path — and a control plane that carried it
//! would be forwarding Net packets.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::anchor_control_plane::AnchorControlPlane;
use crate::bootstrap::{classify_ice_failure, gloo_timer_sleep, stun_probe_failed, Credential};
use crate::clock;
use crate::control_plane::{ControlEvent, ControlPlane, DialogId, NodeId, SignalKind};
use crate::error::LeafError;
use crate::identity::{EntityKeypair, LeafIdentity};
use crate::node::{LeafEvent, StreamHandle};
use crate::rtc::{IceServer, RtcLeafTransport};
use crate::stream::Reliability;

/// How long `connect` waits for the DataChannel to open.
///
/// Past it, the attempt fails with the corrected typing: an ICE
/// timeout, upgraded to `UdpBlocked` only if the STUN probe against
/// the anchor's published `rtc_addr` also failed.
pub const ICE_DEADLINE_MS: i32 = 10_000;

/// Poll interval for the connect wait and the periodic tick.
const TICK_MS: i32 = 50;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(s: &str);
}

/// Route panics to `console.error`, so a wasm surprise is legible in
/// the browser log instead of an opaque `unreachable`.
#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        console_error(&format!("net-mesh-leaf PANIC: {info}"));
    }));
}

/// The shared interior. One per node; the transport's inbound
/// closure holds a `Weak` to it, so a closed node's callbacks cannot
/// resurrect it.
struct Inner {
    node: crate::node::LeafNode,
    transport: RtcLeafTransport,
    anchor: NodeId,
    /// The boundary. Every exchange that is not a Net packet goes
    /// through this and nothing else.
    control: AnchorControlPlane,
    /// The bootstrap dialog, as the control plane numbered it.
    dialog: DialogId,
    /// The credential's invite, for the enrollment exchange. Held
    /// because `enroll` is also callable on its own.
    invite: crate::enroll::Invite,
    /// Inbound bytes the transport delivered but the pump has not
    /// processed. A queue, not direct dispatch: the closure runs
    /// inside a JS callback and must not re-enter a `RefCell` the
    /// pump may already hold.
    inbox: VecDeque<(NodeId, Bytes)>,
    /// Event JSON the node produced and no listener has seen yet.
    ///
    /// The outbound twin of `inbox`, and it exists for the same
    /// reason: a listener must not be called while this cell is
    /// borrowed. It used to be — `flush` emitted straight out of
    /// `&mut self` — so the documented
    /// `stream.onMessage(p => stream.send(p))` re-entered
    /// `borrow_mut` and trapped, and so did closing the node from a
    /// callback. Collected here, handed out by
    /// [`dispatch_events`] with no borrow held.
    outbox: Vec<String>,
    /// Whether a dispatch is already walking the outbox.
    ///
    /// A callback that sends re-enters [`dispatch_events`] through
    /// the pump it triggers. Without this the events that send
    /// produced would be delivered *inside* the listener that caused
    /// them — a listener seeing its own consequence before returning
    /// — and a chatty callback would recurse as deep as it sent.
    /// The inner call leaves them in the outbox and the outer loop
    /// picks them up, so order is arrival order either way.
    dispatching: bool,
    listeners: Vec<js_sys::Function>,
    closed: bool,
}

impl Inner {
    /// Refuse an outbound operation on a node that has been closed
    /// or retired.
    ///
    /// The fence at the node's own boundary. A leadership stand-down
    /// closes the node **before** it lets go of the lock, so anything
    /// that still holds a handle to it — a spawned operation whose
    /// barrier fired late, a page that kept its `LeafNode` — is
    /// refused here rather than allowed to put a packet on a
    /// DataChannel this origin's identity has already moved off.
    fn admit(&self) -> Result<(), LeafError> {
        if self.closed {
            return Err(LeafError::Session(
                "the node is closed: it no longer holds this origin's identity".into(),
            ));
        }
        Ok(())
    }

    /// Deliver everything that has arrived: hand each queued
    /// datagram to the node, push everything the node produced to
    /// the transport, and collect its events for dispatch.
    ///
    /// Collect, not dispatch: see [`Inner::outbox`]. The listeners
    /// are called by [`dispatch_events`] once this borrow is gone.
    ///
    /// This is the arrival path, and it runs **without** the
    /// periodic sweep: what an inbound packet owes its sender is an
    /// acknowledgement, and that has to leave now, not on the next
    /// tick (see the inbound sink for the RTO arithmetic). Sweeping
    /// per packet would also make retransmit and deadline work
    /// proportional to inbound traffic rather than to time.
    ///
    /// A closed node delivers nothing. The ticker's own check is not
    /// enough on its own: an operation that resumes after a
    /// stand-down pumps too, and this is the line that stops it.
    fn deliver(&mut self) {
        if self.closed {
            return;
        }
        let now = clock::now();
        while let Some((peer, bytes)) = self.inbox.pop_front() {
            if !self.node.has_session(peer) && is_handshake_packet(&bytes) {
                if let Err(e) = self.node.complete_handshake(peer, &bytes) {
                    console_error(&format!("net-mesh-leaf: handshake: {e}"));
                }
                continue;
            }
            self.node.on_datagram(peer, bytes, now);
        }
        self.flush();
    }

    /// Deliver, then run the node's time-driven work: retransmit
    /// timers, call deadlines, reassembly expiry.
    fn pump(&mut self) {
        if self.closed {
            return;
        }
        self.deliver();
        self.node.tick(clock::now());
        self.flush();
    }

    /// Put everything the node queued on the wire and collect
    /// everything it produced for the application.
    fn flush(&mut self) {
        for out in self.node.take_outbound() {
            if let Err(e) = self.transport.send(out.peer, out.packet) {
                // Admission refusal. Typed, surfaced, and the
                // packet was never enqueued (§2).
                console_error(&format!("net-mesh-leaf: send refused: {e}"));
            }
        }
        let events = self.node.drain_events();
        self.collect(events);
    }

    /// Queue `events` for the listeners.
    fn collect(&mut self, events: Vec<LeafEvent>) {
        self.outbox
            .extend(events.iter().map(crate::node::LeafEvent::to_json));
    }
}

/// Hand every collected event to the listeners, holding no borrow
/// while one runs.
///
/// The whole point of the outbox. A listener is application code and
/// is documented to be able to call back in — `stream.send` from
/// `onMessage` is the shape the TypeScript surface advertises — so
/// the borrow has to be released *between* collecting an event and
/// delivering it, not merely dropped afterwards. Each turn of the
/// loop takes the outbox and the listener list under a short borrow
/// and then lets go, so a callback's own send is admitted, pumps,
/// and leaves its events for the next turn.
fn dispatch_events(inner: &Rc<RefCell<Inner>>) {
    loop {
        let (events, listeners) = {
            // A pump that is already running owns this cell, and its
            // own dispatch will deliver what we would have.
            let Ok(mut guard) = inner.try_borrow_mut() else {
                return;
            };
            if guard.dispatching || guard.outbox.is_empty() {
                return;
            }
            guard.dispatching = true;
            (core::mem::take(&mut guard.outbox), guard.listeners.clone())
        };
        for json in events {
            let value = JsValue::from_str(&json);
            for listener in &listeners {
                let _ = listener.call1(&JsValue::NULL, &value);
            }
        }
        // Cleared after delivery, so a callback's re-entrant dispatch
        // during the loop above was the one this guard shut out.
        if let Ok(mut guard) = inner.try_borrow_mut() {
            guard.dispatching = false;
        } else {
            return;
        }
    }
}

/// Run `work` under the node's borrow, then deliver whatever it
/// produced with no borrow held.
///
/// Every entry point that pumps goes through here, which is what
/// makes the no-borrow-during-a-callback rule a property of the
/// module rather than a discipline each method has to remember.
fn with_node<T>(inner: &Rc<RefCell<Inner>>, work: impl FnOnce(&mut Inner) -> T) -> T {
    let outcome = work(&mut inner.borrow_mut());
    dispatch_events(inner);
    outcome
}

/// A browser node.
#[wasm_bindgen]
pub struct LeafNode {
    inner: Rc<RefCell<Inner>>,
}

#[wasm_bindgen]
impl LeafNode {
    /// Bootstrap against an anchor and return a connected node.
    ///
    /// `opts`: `{ credentialB64, bootstrapUrl?, origin, iceServers? }`.
    /// `bootstrapUrl` overrides the credential's when present;
    /// everything else Layer 0 needs — the pinned anchor key, the
    /// PSK, the listener URL — comes from the credential itself.
    pub async fn connect(opts: JsValue) -> Result<LeafNode, JsError> {
        let credential_str = require_string(&opts, "credentialB64")?;
        let credential = Credential::decode(&credential_str).map_err(js)?;
        credential.validate_at(clock::now_unix_secs()).map_err(js)?;
        let bootstrap_url = optional_string(&opts, "bootstrapUrl")
            .unwrap_or_else(|| credential.bootstrap_url.clone());
        let ice_servers = parse_ice_servers(&opts)?;
        let identity = identity_from(&opts)?;
        let node_id = identity.node_id();

        // Layer 0 step 1 lives inside `attach`: the live anchor info
        // is fetched and COMPARED against the key the credential
        // pins, and a mismatch is refused there — before an offer
        // exists and before any handshake is attempted. That
        // ordering is the whole MITM witness, which is why it is a
        // constructor rather than a step a later edit could move.
        let control = AnchorControlPlane::attach(bootstrap_url, credential.clone(), node_id)
            .await
            .map_err(js)?;
        let anchor = control.anchor_node();
        let anchor_rtc_addr = control.anchor_rtc_addr();

        let seed = u64::from_le_bytes(
            random32().map_err(js)?[..8]
                .try_into()
                .map_err(|_| JsError::new("seed"))?,
        );
        let mut node = crate::node::LeafNode::new(identity, seed);
        node.set_peer_rtc_addr(anchor, anchor_rtc_addr);

        let inner = Rc::new(RefCell::new(Inner {
            node,
            transport: RtcLeafTransport::new(Rc::new(|_, _| {})),
            anchor,
            control,
            dialog: 0,
            invite: credential.invite.clone(),
            inbox: VecDeque::new(),
            outbox: Vec::new(),
            dispatching: false,
            listeners: Vec::new(),
            closed: false,
        }));

        // The inbound sink queues **and delivers**. A `Weak` so the
        // closure cannot keep a closed node alive; a `try_borrow_mut`
        // so a datagram that arrives while the pump already holds
        // the node is left for the pump that is running.
        //
        // **Delivery cannot wait for the tick.** Queueing alone put
        // up to `TICK_MS` between a packet arriving and the
        // acknowledgement it owes going out, and a native sender's
        // initial RTO is `ReliableStream::DEFAULT_RTO` — 50 ms, the
        // same order. Every reply then timed out before its ack
        // could physically arrive: the sender took each spurious
        // timeout as congestion, collapsed its window to `MIN_CWND`,
        // burned `DEFAULT_MAX_RETRIES` on packets the leaf already
        // held, and stalled the stream. Measured round trip was
        // ~62 ms against a 50 ms RTO. Arrival is the only moment at
        // which the leaf can answer in time.
        let weak: Weak<RefCell<Inner>> = Rc::downgrade(&inner);
        let sink: crate::rtc::InboundSink = Rc::new(move |peer, bytes| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            {
                let Ok(mut guard) = inner.try_borrow_mut() else {
                    return;
                };
                guard.inbox.push_back((peer, bytes));
                guard.deliver();
            }
            // Outside the borrow, deliberately: a `stream_data`
            // listener is where an application answers, and the
            // answer is a synchronous `send` back into this same
            // cell.
            dispatch_events(&inner);
        });
        inner.borrow_mut().transport = RtcLeafTransport::new(sink);

        // Layer 0 step 2: the offer, through the boundary. Both
        // handles are cloned out of the `RefCell` first: holding a
        // borrow across an await would deadlock against the pump on
        // the next tick.
        let (transport, control) = {
            let guard = inner.borrow();
            (guard.transport.clone(), guard.control.clone())
        };
        // The cancellation owner, armed **before the first
        // resource-bearing suspension**. Every branch below that
        // *returns* cleans up after itself; this covers the branch
        // that returns nothing because the future stopped existing.
        // See [`ConnectGuard`].
        let attempt = ConnectGuard::new(&inner, &transport, &control);
        let offer = transport
            .create_offer(anchor, &ice_servers)
            .await
            .map_err(js)?;
        let accepted = control.offer(offer).await.map_err(js)?;
        // Recorded **before** the next await, not after
        // `accept_answer`: from the anchor's answer onwards there is
        // an accepted attempt registered for this node id, and a
        // dialog id living only in a local is a dialog nobody can
        // hand back if the page walks away during
        // `setRemoteDescription`.
        inner.borrow_mut().dialog = accepted.dialog;
        attempt.accepted(accepted.dialog);
        transport
            .accept_answer(anchor, &accepted.answer)
            .await
            .map_err(js)?;

        // From here on the anchor has an ACCEPTED ATTEMPT registered
        // for this node id, so every failure below must hand it back:
        // an attempt the page walks away from holds an ICE agent and
        // a signalling reservation on the anchor until its own
        // deadline. A page is told to decide after a typed failure,
        // and deciding to try again must not be charged for the
        // attempt that failed.
        let brought_up = async {
            // Layer 0 step 3: wait for the channel, trickling both
            // ways through the boundary.
            wait_for_channel(&inner, anchor).await?;

            // Layer 1: the NKpsk0 handshake. `accepted.peer_static`
            // is the CREDENTIAL's key — the control plane returns
            // what authenticated the attempt, and `attach` already
            // refused a live key that differed from it. This is the
            // whole MITM property.
            let msg1 = {
                let mut guard = inner.borrow_mut();
                let slot = guard.transport.next_slot();
                let packet = guard
                    .node
                    .begin_handshake(anchor, &credential.psk, &accepted.peer_static, slot)
                    .map_err(js)?;
                guard.transport.send(anchor, packet.clone()).map_err(js)?;
                packet
            };
            debug_assert!(!msg1.is_empty());
            wait_for_session(&inner, anchor).await?;
            Ok::<(), JsError>(())
        }
        .await;
        if let Err(failure) = brought_up {
            abandon_attempt(&inner).await;
            attempt.disarm();
            return Err(failure);
        }

        // Layer 2: enrollment, over the session just installed.
        // §12 admits a browser's session as **provisional** and
        // refuses calls, publishes and subscribes above the
        // transport until the anchor has admitted this leaf, so a
        // connect that skipped this returns a node whose every
        // operation dies on its deadline.
        //
        // The ticker starts first, deliberately: the enrollment
        // request only reaches the wire when something pumps, and
        // the reply only arrives when something drains.
        start_ticker(Rc::downgrade(&inner));
        let node = LeafNode { inner };
        if let Err(failure) = node.enroll().await {
            // Same rule one layer up: the session and the attempt
            // both go back, so the anchor is not left holding either.
            node.close();
            attempt.disarm();
            return Err(failure);
        }
        attempt.disarm();
        Ok(node)
    }

    /// This node's id, as 16 lowercase hex digits.
    pub fn node_id_hex(&self) -> String {
        format!("{:016x}", self.inner.borrow().node.node_id())
    }

    /// The anchor's node id, as 16 lowercase hex digits.
    pub fn anchor_id_hex(&self) -> String {
        format!("{:016x}", self.inner.borrow().anchor)
    }

    /// This node's **origin hash**, as 16 lowercase hex digits.
    ///
    /// Not the node id, and not interchangeable with it: the origin
    /// hash is derived from the entity key and is what rides every
    /// packet header this node seals, what names its nRPC reply
    /// channels (`<service>.replies.<origin>`), and what a receiver
    /// compares an event's `EventMeta.origin_hash` against — a
    /// direct peer whose packet origin and payload origin disagree
    /// has its frame dropped before admission, so anything that
    /// builds an event payload for this node to send MUST name this
    /// value.
    pub fn origin_hash_hex(&self) -> String {
        format!("{:016x}", self.inner.borrow().node.origin_hash())
    }

    /// Every counter, as JSON. u64s are decimal strings.
    pub fn counters_json(&self) -> String {
        self.inner.borrow().node.counters().to_json()
    }

    /// Call `service` on the anchor.
    ///
    /// Resolves to the reply body, or rejects with the typed
    /// failure — `Timeout`, `SessionLost`, `Refused(status)`. Never
    /// a silent retry.
    pub async fn call(
        &self,
        service: String,
        payload: Uint8Array,
        timeout_ms: Option<f64>,
    ) -> Result<Uint8Array, JsError> {
        let receiver = with_node(&self.inner, |guard| {
            guard.admit()?;
            let peer = guard.anchor;
            let receiver = guard.node.call(
                peer,
                &service,
                &payload.to_vec(),
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                timeout_ms.map(|ms| ms.max(0.0) as u64),
            )?;
            guard.pump();
            Ok::<_, LeafError>(receiver)
        })
        .map_err(js)?;
        match receiver.await {
            Ok(Ok(body)) => Ok(Uint8Array::from(&body[..])),
            Ok(Err(e)) => Err(js(LeafError::Rpc(e))),
            // The sender was dropped: the call was cancelled
            // locally, which is a cancellation, not a timeout.
            Err(_) => Err(js(LeafError::Rpc(crate::error::RpcError::Malformed(
                "the call was cancelled locally".into(),
            )))),
        }
    }

    /// Subscribe to `channel` on the anchor.
    pub async fn subscribe(&self, channel: String) -> Result<(), JsError> {
        with_node(&self.inner, |guard| {
            guard.admit()?;
            let peer = guard.anchor;
            guard.node.subscribe(peer, &channel)?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)
    }

    /// Publish `payload` on `channel`.
    pub async fn publish(&self, channel: String, payload: Uint8Array) -> Result<(), JsError> {
        with_node(&self.inner, |guard| {
            guard.admit()?;
            let peer = guard.anchor;
            guard.node.publish(peer, &channel, &payload.to_vec())?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)
    }

    /// Open an application stream.
    ///
    /// `opts`: `{ reliability: "reliable" | "fireAndForget",
    /// reliable?: boolean, label?, streamId?, channelHash? }`.
    /// `streamId` (a decimal or `0x`-hex **string**, because a `u64`
    /// never crosses as a JS number) and `channelHash` (a **number**
    /// in `0..=65535`) are used verbatim when present, so a stream
    /// can match a publish contract a native handler dispatches on.
    ///
    /// Both are read by `stream_options`, which
    /// [`crate::leader_session::MeshSession::open_stream`] also
    /// calls: the direct and the proxied surface cannot read the
    /// same option object two ways.
    pub fn open_stream(&self, opts: JsValue) -> Result<LeafStream, JsError> {
        let options = stream_options(&opts)?;

        let mut guard = self.inner.borrow_mut();
        guard.admit().map_err(js)?;
        let peer = guard.anchor;
        let handle = guard
            .node
            .open_stream(
                peer,
                &options.label,
                options.reliability,
                options.stream_id,
                options.channel_hash,
            )
            .map_err(js)?;
        Ok(LeafStream {
            inner: Rc::clone(&self.inner),
            handle,
        })
    }

    /// The ICE servers a `connect(opts)` with this options object
    /// would configure its `RTCPeerConnection` with, as JSON.
    ///
    /// Not a convenience: it is the only way for a caller — or a
    /// test — to see what the leaf made of the `RTCIceServer[]` it
    /// was handed *before* a connection attempt consumes it. Stage 5
    /// read those objects with `as_string`, so every one of them
    /// dropped and a page's STUN/TURN configuration was silently
    /// absent from the offer. Reads through exactly the parser
    /// [`Self::connect`] uses.
    ///
    /// Shape: `[{"urls":["stun:host:3478"],"username":"u",
    /// "credential":"c"}]`, `username`/`credential` present only
    /// when the entry carried them.
    pub fn effective_ice_servers(opts: JsValue) -> Result<String, JsError> {
        Ok(ice_servers_json(&parse_ice_servers(&opts)?))
    }

    /// What an `open_stream(opts)` with this options object would
    /// actually ask the node for, as JSON — the same read, without
    /// the stream.
    ///
    /// Shape: `{"reliability":"reliable","label":"app",
    /// "streamId":"0000000000000009","channelHash":7}`, with
    /// `streamId` `null` when the caller did not pin one (the node
    /// allocates) and `channelHash` `null` when absent.
    pub fn effective_stream_options(opts: JsValue) -> Result<String, JsError> {
        Ok(stream_options(&opts)?.to_json())
    }

    /// Build, sign and publish this leaf's announcement.
    ///
    /// `capabilities` become tags alongside the mandatory `leaf` and
    /// `transport:rtc`; `reflex_addr` and `rtc_addr` stay absent.
    pub async fn announce(&self, capabilities: Vec<String>) -> Result<(), JsError> {
        with_node(&self.inner, |guard| {
            guard.admit()?;
            let announcement = guard.node.build_announcement(&capabilities)?;
            let peer = guard.anchor;
            // v1's control plane has no publish endpoint; §7's
            // reachability path is the data one — the anchor floods
            // what it receives.
            guard.node.announce_to_peer(peer, &announcement)?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)
    }

    /// Answer a capability query from the announcements this leaf
    /// verified. A JSON array of
    /// `{ node_id, entity_id, capabilities, rtc_addr, noise_pubkey, version }`.
    pub async fn query(&self, capability: String) -> Result<String, JsError> {
        Ok(self.inner.borrow().node.query(&capability))
    }

    /// Sign a `0x0D02` signalling envelope for `peer` and hand it to
    /// the control plane — no session with `peer` needed.
    ///
    /// Through the boundary, not the data path. An envelope
    /// authenticates itself, so the carrier is untrusted and can be
    /// anything.
    ///
    /// **The v1 anchor carrier refuses** (R14). `AnchorControlPlane`
    /// does not carry envelopes: the Stage 4b listener reads one
    /// trickle frame type, `candidate`, and forwards nothing to a
    /// third party, so `signal` on a node connected to a real anchor
    /// returns `LeafError::ControlPlane` naming the peer it could not
    /// reach. That is deliberate — the first cut reported the local
    /// send as a delivery the anchor discarded. The carrier that does
    /// deliver today is the anchorless `MockControlPlane`; a
    /// forwarding anchor route is Stage 6's, and the trait's shape
    /// already admits it, which is why this takes a peer id and an
    /// envelope rather than a session.
    pub async fn signal(
        &self,
        peer_hex: String,
        dialog: f64,
        kind: String,
        payload: Uint8Array,
    ) -> Result<(), JsError> {
        let peer = parse_u64(&peer_hex)?;
        let kind = match kind.as_str() {
            "offer" => SignalKind::Offer,
            "answer" => SignalKind::Answer,
            "candidate" => SignalKind::Candidate,
            "reject" => SignalKind::Reject,
            other => return Err(JsError::new(&format!("unknown signal kind {other:?}"))),
        };
        let (control, envelope) = {
            let guard = self.inner.borrow();
            guard.admit().map_err(js)?;
            let envelope = guard
                .node
                .sign_signal(peer, dialog as u64, kind, payload.to_vec());
            (guard.control.clone(), envelope)
        };
        control.signal(envelope).await.map_err(js)
    }

    /// Register an event listener. Each receives one JSON string per
    /// event.
    pub fn on_event(&self, callback: js_sys::Function) {
        self.inner.borrow_mut().listeners.push(callback);
    }

    /// Run the enrollment exchange against the anchor.
    ///
    /// [`Self::connect`] awaits this already, so a page never needs
    /// it; it is here because "still provisional" and "the anchor
    /// was slow" are the same symptom from the outside — an nRPC
    /// timeout — and a harness needs to be able to drive and
    /// observe the step that distinguishes them.
    ///
    /// A refusal is typed and final: the invite is single-use, so
    /// nothing here retries. Retrying would burn it and turn a
    /// legible refusal into an illegible replay.
    pub async fn enroll(&self) -> Result<(), JsError> {
        let Some(receiver) = with_node(&self.inner, |guard| {
            guard.admit()?;
            if guard.node.is_enrolled() {
                return Ok(None);
            }
            let peer = guard.anchor;
            let invite = guard.invite.clone();
            let receiver =
                guard
                    .node
                    .begin_enrollment(peer, &invite, "net-mesh-leaf", &[], None)?;
            guard.pump();
            Ok::<_, LeafError>(Some(receiver))
        })
        .map_err(js)?
        else {
            return Ok(());
        };
        let reply = match receiver.await {
            Ok(Ok(body)) => body,
            Ok(Err(e)) => return Err(js(LeafError::Rpc(e))),
            Err(_) => {
                return Err(js(LeafError::Rpc(crate::error::RpcError::Malformed(
                    "the enrollment call was cancelled locally".into(),
                ))))
            }
        };
        with_node(&self.inner, |guard| {
            guard.node.finish_enrollment(&reply)?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)
    }

    /// Whether the anchor has admitted this leaf.
    ///
    /// `false` after a successful handshake means the session is
    /// still provisional, which is the discriminator a harness needs
    /// when a call times out: §12 refused it, rather than the
    /// service being slow.
    pub fn is_enrolled(&self) -> bool {
        self.inner.borrow().node.is_enrolled()
    }

    /// Close the node: every session, every channel, every pending
    /// call.
    ///
    /// Pending calls fail `RpcError::SessionLost` — typed, and not
    /// re-issued by anybody.
    pub fn close(&self) {
        with_node(&self.inner, |guard| {
            guard.closed = true;
            let anchor = guard.anchor;
            guard.node.drop_session(anchor, "the node was closed");
            // The attempt ends through the boundary: closing the
            // carrier's socket is the carrier's business, not this
            // module's.
            let control = guard.control.clone();
            let dialog = guard.dialog;
            wasm_bindgen_futures::spawn_local(async move {
                let _ = control.end_attempt(dialog).await;
            });
            guard.transport.close_all();
            let events = guard.node.drain_events();
            guard.collect(events);
        });
    }

    /// Retire the node because leadership moved off this tab, and say
    /// how many pending calls that failed.
    ///
    /// [`Self::close`] is the page saying "I am done with this node";
    /// this is the lifecycle saying "this node no longer holds the
    /// identity". The difference is the disposition of the pending
    /// calls, and it matters to the caller: a close is a
    /// `SessionLost`, and a stand-down is a `LeaderLost` naming the
    /// generation that owned the call, because "which leader did I
    /// lose" is the question a page asks next and this is the only
    /// place that knows the answer.
    ///
    /// `closed` is set **first**, so the pump the failure path runs
    /// through emits nothing, and everything after it is teardown.
    pub fn retire(&self, generation: u64) -> usize {
        with_node(&self.inner, |guard| {
            if guard.closed {
                return 0;
            }
            guard.closed = true;
            let failed = guard.node.fail_calls_on_leader_loss(generation);
            let anchor = guard.anchor;
            guard.node.drop_session(anchor, "leadership was released");
            let control = guard.control.clone();
            let dialog = guard.dialog;
            wasm_bindgen_futures::spawn_local(async move {
                let _ = control.end_attempt(dialog).await;
            });
            guard.transport.close_all();
            let events = guard.node.drain_events();
            guard.collect(events);
            failed
        })
    }
}

/// One application stream.
#[wasm_bindgen]
pub struct LeafStream {
    inner: Rc<RefCell<Inner>>,
    handle: StreamHandle,
}

#[wasm_bindgen]
impl LeafStream {
    /// The stream id, 16 lowercase hex digits.
    pub fn stream_id_hex(&self) -> String {
        format!("{:016x}", self.handle.stream_id)
    }

    /// Whether this stream retransmits.
    pub fn is_reliable(&self) -> bool {
        self.handle.reliability.is_reliable()
    }

    /// Send one payload. The bytes ride verbatim as one event in one
    /// packet — no leaf-added framing.
    ///
    /// Callable from [`Self::on_message`]: the borrow this takes is
    /// released before any listener runs, which is what makes
    /// `stream.onMessage(p => stream.send(p))` — the echo shape the
    /// TypeScript surface advertises — a supported operation rather
    /// than a `RefCell` trap.
    pub fn send(&self, payload: Uint8Array) -> Result<(), JsError> {
        with_node(&self.inner, |guard| {
            guard.admit()?;
            guard.node.stream_send(self.handle, &payload.to_vec())?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)
    }

    /// Listen for this stream's inbound payloads.
    ///
    /// **The callback receives the node's `stream_data` event JSON
    /// string**, not bytes — the same string
    /// [`LeafNode::on_event`] delivers, filtered to this stream's
    /// id:
    ///
    /// ```text
    /// {"type":"stream_data","stream_id":"9","seq":"1","payload":"AQI="}
    /// ```
    ///
    /// `stream_id` and `seq` are decimal `u64` strings and `payload`
    /// is standard padded base64. One contract, one direction: the
    /// leaf emits its canonical event JSON and the consumer decodes
    /// it. Emitting bytes here instead would mean a second encoding
    /// of an event that already exists, and would throw away `seq`
    /// and the rest of the event's provenance at the boundary.
    /// `@net-mesh/browser`'s `LeafStream` does that decode, which is
    /// why its `onMessage` and its async iterator yield
    /// `Uint8Array`; a host wiring this callback itself must parse
    /// the same way.
    ///
    /// The callback runs with **no** borrow of the node held, so it
    /// may call straight back in: [`Self::send`], [`LeafNode::close`]
    /// and the rest are all reachable from here. That was not true
    /// before — the event was emitted from inside `flush`'s `&mut
    /// self` — so the advertised echo trapped instead of sending.
    pub fn on_message(&self, callback: js_sys::Function) {
        let wanted = format!("\"stream_id\":\"{}\"", self.handle.stream_id);
        let filter = Closure::wrap(Box::new(move |json: JsValue| {
            if json
                .as_string()
                .is_some_and(|text| text.contains(&wanted) && text.contains("\"stream_data\""))
            {
                let _ = callback.call1(&JsValue::NULL, &json);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let function: js_sys::Function =
            filter.as_ref().unchecked_ref::<js_sys::Function>().clone();
        filter.forget();
        self.inner.borrow_mut().listeners.push(function);
    }

    /// Stop using the stream. The session stays; a stream is
    /// per-stream state, not a connection.
    ///
    /// Nothing leaves the wire: one DataChannel carries every stream
    /// and there is no per-stream teardown frame. What it does do is
    /// release the node's own per-stream state — the receive cursor
    /// and the stream's classification — through the same fenced
    /// entry point a stale handle is refused by. It used to do
    /// literally nothing, which meant a proxied `StreamClose`
    /// removed the leader's handle and left the node holding the
    /// stream: a page that closed and reopened the same id got the
    /// old cursor.
    ///
    /// A refusal here is reported rather than returned: the handle is
    /// being discarded either way, and `close` is the one operation
    /// for which "it was already gone" is the outcome the caller
    /// wanted.
    pub fn close(&self) {
        with_node(&self.inner, |guard| {
            if let Err(e) = guard.node.close_stream(self.handle) {
                console_error(&format!("net-mesh-leaf: stream close: {e}"));
            }
        });
    }
}

// ───────────────────────────── plumbing ─────────────────────────────

/// A Net handshake packet: `0x4E45` magic, version 1, HANDSHAKE flag.
fn is_handshake_packet(bytes: &[u8]) -> bool {
    bytes.len() >= net_wire::protocol::HEADER_SIZE
        && u16::from_le_bytes([bytes[0], bytes[1]]) == net_wire::protocol::MAGIC
        && net_wire::protocol::PacketFlags::from_bits(bytes[3]).is_handshake()
}

/// Pump the control plane, both directions.
///
/// Out: the candidates the browser gathered. In: the peer's
/// candidates, the announcements a control plane delivered, and the
/// signalling envelopes — which the **leaf** verifies, never the
/// transport. One function, called from the connect wait and from
/// the ticker, so the two cannot drift apart.
async fn service_control_plane(inner: &Rc<RefCell<Inner>>) -> Result<(), LeafError> {
    let (control, transport, dialog, anchor) = {
        let guard = inner.borrow();
        (
            guard.control.clone(),
            guard.transport.clone(),
            guard.dialog,
            guard.anchor,
        )
    };
    for (peer, candidate) in transport.take_local_candidates() {
        debug_assert_eq!(peer, anchor);
        if let Err(e) = control.trickle(dialog, candidate).await {
            console_error(&format!("net-mesh-leaf: trickle: {e}"));
        }
    }

    let mut ended = None;
    for event in control.drain_events() {
        match event {
            ControlEvent::Candidate {
                dialog: for_dialog,
                candidate,
            } => {
                if for_dialog != dialog {
                    continue;
                }
                if let Err(e) = transport.add_remote_candidate(anchor, &candidate).await {
                    console_error(&format!("net-mesh-leaf: remote candidate: {e}"));
                }
            }
            ControlEvent::AttemptEnded {
                dialog: for_dialog,
                reason,
            } => {
                if for_dialog == dialog {
                    ended = Some(reason);
                }
            }
            ControlEvent::Announcement(announcement) => {
                inner.borrow_mut().node.ingest_announcement(&announcement.0);
            }
            ControlEvent::Signal(envelope) => {
                let now = clock::now();
                inner.borrow_mut().node.accept_signal(envelope, now);
            }
        }
    }

    match ended {
        Some(reason) => Err(LeafError::ControlPlane(reason)),
        None => Ok(()),
    }
}

/// Poll until the DataChannel opens, servicing the control plane,
/// or fail with the corrected ICE typing.
async fn wait_for_channel(inner: &Rc<RefCell<Inner>>, peer: NodeId) -> Result<(), JsError> {
    let mut waited = 0;
    while waited < ICE_DEADLINE_MS {
        // Serviced before the first sleep: the listener sends its
        // own candidate as the trickle socket's first frame, and
        // applying it immediately is the head start S0b measured
        // (6.6× gather-complete at the floor).
        //
        // **Openness is decided before the dialog's end is.** The
        // bootstrap dialog exists to carry SDP and candidates; once
        // the channel is up it has done its job, and the anchor
        // retires it as a matter of course. Failing the connect on
        // its close regardless of whether the channel had already
        // opened turned the anchor's own completion into
        // `the anchor closed the bootstrap dialog: 1006` — a socket
        // whose handshake merely lost a race with ICE.
        let ended = service_control_plane(inner).await.err();
        if inner.borrow().transport.is_open(peer) {
            return Ok(());
        }
        if let Some(ended) = ended {
            return Err(js(ended));
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }

    // The deadline passed. The HTTPS bootstrap demonstrably
    // succeeded (we have an answer), so the probe is the one thing
    // that can turn the classification.
    let rtc_addr = inner.borrow().control.anchor_rtc_addr();
    let probe_failed = match &rtc_addr {
        Some(addr) => stun_probe_failed(addr).await,
        None => false,
    };
    Err(js(LeafError::Rtc(classify_ice_failure(
        true,
        probe_failed,
        rtc_addr.as_deref(),
    ))))
}

/// Poll until the handshake installs a session.
///
/// The channel is already open here, so the bootstrap dialog has
/// nothing left to carry: `msg1` and `msg2` ride the DataChannel,
/// not the control plane. Its end is therefore not this wait's
/// failure — but it IS the best diagnosis available if the session
/// never arrives, so it is kept and reported then.
async fn wait_for_session(inner: &Rc<RefCell<Inner>>, peer: NodeId) -> Result<(), JsError> {
    let mut waited = 0;
    let mut dialog_ended: Option<LeafError> = None;
    while waited < ICE_DEADLINE_MS {
        if let Err(e) = service_control_plane(inner).await {
            dialog_ended = Some(e);
        }
        let installed = with_node(inner, |guard| {
            guard.pump();
            guard.node.has_session(peer)
        });
        if installed {
            return Ok(());
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    Err(js(match dialog_ended {
        Some(ended) => LeafError::Session(format!(
            "the anchor did not complete the Noise handshake inside the deadline ({ended})"
        )),
        None => LeafError::Session(
            "the anchor did not complete the Noise handshake inside the deadline".into(),
        ),
    }))
}

/// Hand a failed attempt back to the anchor.
///
/// A `connect()` that rejects must leave NOTHING behind: the anchor
/// registers an accepted attempt the moment it answers the offer,
/// and an attempt nobody retires holds an ICE agent, a dialog row
/// and a signalling reservation until its `ice_deadline` — which on
/// a real anchor is tens of seconds and is charged against the
/// bound the next offer is measured against. Handing it back is the
/// page saying "I am done with this one", and it is the only thing
/// that can say so: the anchor cannot distinguish a browser that
/// gave up from one that is still gathering.
///
/// Awaited rather than spawned, so the caller's rejection is the
/// last thing that happens.
async fn abandon_attempt(inner: &Rc<RefCell<Inner>>) {
    let (control, dialog, transport) = {
        let guard = inner.borrow();
        (guard.control.clone(), guard.dialog, guard.transport.clone())
    };
    inner.borrow_mut().closed = true;
    if dialog != 0 {
        let _ = control.end_attempt(dialog).await;
    }
    transport.close_all();
}

/// The cleanup owner of a `connect` that never returns a node.
///
/// # Why a guard rather than one more error branch
///
/// Every failure `connect` *returns* hands its resources back on the
/// way out. A future can also simply stop existing, and nothing in
/// the function body observes that: no `?`, no `if let Err`, no
/// `match`. §8's promotion is exactly that case — the origin's Web
/// Lock is held across this call, and `close()` on a tab whose
/// promotion is still bootstrapping cancels it by **dropping** this
/// future. By then the attempt owns a real `RTCPeerConnection` and
/// its DataChannel ([`crate::rtc::RtcLeafTransport::create_offer`]
/// installs the link before its first await) and, past the anchor's
/// answer, a dialog on the anchor.
///
/// So the cleanup belongs to a value that dies with the future.
/// Closing the RTC resources is **synchronous**, deliberately: the
/// frame that dropped the future goes on to release the bootstrap's
/// lock, and a successor granted that lock must not find its
/// predecessor's ICE agent still gathering and its channel still
/// open. Handing the accepted attempt back is a network round trip
/// and cannot be awaited from `Drop`, so it is spawned — the anchor
/// learns a moment later, which is the same ordering
/// [`LeafNode::close`] already uses.
///
/// Disarmed on the two paths that have already cleaned up
/// ([`abandon_attempt`] and [`LeafNode::close`]) and on success,
/// where the node — and its transport — belong to the caller.
struct ConnectGuard {
    inner: Rc<RefCell<Inner>>,
    transport: RtcLeafTransport,
    control: AnchorControlPlane,
    /// `0` until the anchor has accepted the attempt; there is
    /// nothing to hand back before that.
    dialog: Cell<u64>,
    armed: Cell<bool>,
}

impl ConnectGuard {
    fn new(
        inner: &Rc<RefCell<Inner>>,
        transport: &RtcLeafTransport,
        control: &AnchorControlPlane,
    ) -> Self {
        Self {
            inner: Rc::clone(inner),
            transport: transport.clone(),
            control: control.clone(),
            dialog: Cell::new(0),
            armed: Cell::new(true),
        }
    }

    /// The anchor accepted the attempt: a cancellation from here on
    /// owes it an `end_attempt`.
    fn accepted(&self, dialog: DialogId) {
        self.dialog.set(dialog);
    }

    /// The node reached its caller, or the failing branch has
    /// already handed everything back.
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for ConnectGuard {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        // The ticker and the inbound sink both hold `Weak`s, so they
        // stop on their own once this frame's strong references go.
        // `closed` is for anything that got a handle in between: a
        // cancelled attempt's node refuses rather than pumps.
        if let Ok(mut guard) = self.inner.try_borrow_mut() {
            guard.closed = true;
        }
        // Before this function returns, and therefore before the
        // lock: this is the whole ordering claim.
        self.transport.close_all();
        let dialog = self.dialog.get();
        if dialog != 0 {
            let control = self.control.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = control.end_attempt(dialog).await;
            });
        }
    }
}

/// The periodic tick: the control plane is serviced, deadlines and
/// reassembly expiry keep moving on an otherwise silent connection.
fn start_ticker(inner: Weak<RefCell<Inner>>) {
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timer_sleep(TICK_MS).await.ok();
            let Some(inner) = inner.upgrade() else {
                return;
            };
            if inner.try_borrow().is_ok_and(|guard| guard.closed) {
                return;
            }
            // A control-plane failure after connect is not fatal to
            // the node: the session outlives the bootstrap dialog,
            // and the anchor closing it is normal.
            let _ = service_control_plane(&inner).await;
            let stopped = {
                let Ok(mut guard) = inner.try_borrow_mut() else {
                    continue;
                };
                if guard.closed {
                    true
                } else {
                    guard.pump();
                    false
                }
            };
            if stopped {
                return;
            }
            // After the borrow, so a listener that sends or closes
            // from here is doing so on an unborrowed node.
            dispatch_events(&inner);
        }
    });
}

/// The identity `connect` runs under.
///
/// Custodial when the host supplies one — the same shape as
/// `MeshNodeConfig::entity_keypair` — generated from the platform
/// CSPRNG otherwise. Both spellings are accepted because the page
/// and the TS wrapper disagree about casing elsewhere in this wave.
///
/// **A key that is present but unusable is a hard failure.** Falling
/// through to `generate()` is what made a dropped option look like a
/// leaf ignoring custody, and "two tabs sharing one identity" is an
/// exit criterion nobody can check if the fallback is silent.
fn identity_from(opts: &JsValue) -> Result<LeafIdentity, JsError> {
    let entity_hex = optional_string(opts, "entitySecretHex")
        .or_else(|| optional_string(opts, "entity_secret_hex"));
    let noise_hex = optional_string(opts, "noiseSecretHex")
        .or_else(|| optional_string(opts, "noise_secret_hex"));
    match (entity_hex, noise_hex) {
        (None, None) => LeafIdentity::generate().map_err(js),
        (None, Some(_)) => Err(JsError::new(
            "noiseSecretHex was supplied without entitySecretHex: the Noise static \
             is not an identity, and generating the entity half beside an injected \
             Noise half would produce a node id the host did not choose",
        )),
        (Some(entity), noise) => {
            let entity = secret32(&entity, "entitySecretHex")?;
            let noise = match noise {
                Some(hex) => secret32(&hex, "noiseSecretHex")?,
                None => random32().map_err(js)?,
            };
            Ok(LeafIdentity::from_secrets(
                EntityKeypair::from_secret(entity),
                noise,
            ))
        }
    }
}

/// A 32-byte secret from hex, or a failure naming the option.
fn secret32(hex: &str, key: &str) -> Result<[u8; 32], JsError> {
    crate::identity::unhex(hex)
        .map_err(|e| JsError::new(&format!("{key} is not hex: {e}")))?
        .try_into()
        .map_err(|_| JsError::new(&format!("{key} must be 32 bytes")))
}

fn random32() -> crate::error::Result<[u8; 32]> {
    let mut out = [0u8; 32];
    getrandom::fill(&mut out)
        .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
    Ok(out)
}

fn js(error: LeafError) -> JsError {
    JsError::new(&error.to_string())
}

fn require_string(opts: &JsValue, key: &str) -> Result<String, JsError> {
    optional_string(opts, key).ok_or_else(|| JsError::new(&format!("{key} is required")))
}

fn optional_string(opts: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_string())
}

fn optional_bool(opts: &JsValue, key: &str) -> Option<bool> {
    js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_bool())
}

/// A string option that refuses a present value of another type.
///
/// [`optional_string`] answers `None` for anything that is not a
/// string, which is the right reading for an absent option and the
/// wrong one for a *supplied* option of the wrong type: a caller
/// who wrote `streamId: 9` — the exact mistake the `u64`-as-string
/// rule exists to prevent — would silently get an allocated id
/// instead of the one their native handler dispatches on.
fn typed_string(opts: &JsValue, key: &str, expected: &str) -> Result<Option<String>, JsError> {
    let value = js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .map_err(|_| JsError::new(&format!("{key} could not be read")))?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    value.as_string().map(Some).ok_or_else(|| {
        let actual = value.js_typeof().as_string().unwrap_or_default();
        JsError::new(&format!("{key} must be {expected}, not a {actual}"))
    })
}

/// A `u16` option, or a loud refusal.
///
/// `channelHash` used to be read with `as_f64()` and cast. Two
/// silent failures came out of that: the **string** the published
/// TypeScript type asked callers for read as `None` and became hash
/// 0 — a different channel — and `70_000` saturated to `65_535`
/// instead of being rejected. A hash the caller did not ask for is
/// worse than an error, so this is the only reader for it.
fn optional_u16(opts: &JsValue, key: &str) -> Result<Option<u16>, JsError> {
    let value = js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .map_err(|_| JsError::new(&format!("{key} could not be read")))?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(number) = value.as_f64() else {
        let actual = value.js_typeof().as_string().unwrap_or_default();
        return Err(JsError::new(&format!(
            "{key} must be a number in 0..=65535, not a {actual}"
        )));
    };
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 || number > 65_535.0 {
        return Err(JsError::new(&format!(
            "{key} must be a whole number in 0..=65535, got {number}"
        )));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(Some(number as u16))
}

/// Everything `open_stream` reads from its options object.
///
/// One struct rather than four reads at each of the two call sites,
/// because the direct surface and the proxied one
/// ([`crate::leader_session::MeshSession::open_stream`]) reading the
/// same object two different ways is exactly the defect this
/// replaces.
pub(crate) struct StreamOptions {
    pub(crate) reliability: Reliability,
    pub(crate) label: String,
    /// The id the caller pinned, or `None` to let the node allocate.
    pub(crate) stream_id: Option<u64>,
    pub(crate) channel_hash: Option<u16>,
}

impl StreamOptions {
    /// The options as JSON, `streamId` spelled the way
    /// [`LeafStream::stream_id_hex`] will report it.
    pub(crate) fn to_json(&self) -> String {
        let reliability = if self.reliability.is_reliable() {
            "reliable"
        } else {
            "fireAndForget"
        };
        let stream_id = self
            .stream_id
            .map_or_else(|| "null".to_string(), |id| format!("\"{id:016x}\""));
        let channel_hash = self
            .channel_hash
            .map_or_else(|| "null".to_string(), |hash| hash.to_string());
        format!(
            "{{\"reliability\":\"{reliability}\",\"label\":{},\"streamId\":{stream_id},\
             \"channelHash\":{channel_hash}}}",
            json_string(&self.label)
        )
    }
}

/// Read `{ reliability?, reliable?, label?, streamId?, channelHash? }`.
pub(crate) fn stream_options(opts: &JsValue) -> Result<StreamOptions, JsError> {
    let reliability = match typed_string(opts, "reliability", "\"reliable\" or \"fireAndForget\"")?
    {
        Some(spelling) => Reliability::parse(&spelling)
            .ok_or_else(|| JsError::new("reliability must be \"reliable\" or \"fireAndForget\""))?,
        None => match optional_bool(opts, "reliable") {
            Some(true) | None => Reliability::Reliable,
            Some(false) => Reliability::FireAndForget,
        },
    };
    Ok(StreamOptions {
        reliability,
        label: typed_string(opts, "label", "a string")?.unwrap_or_else(|| "app".to_string()),
        stream_id: match typed_string(opts, "streamId", "a decimal or 0x-hex string")? {
            Some(raw) => Some(parse_u64(&raw)?),
            None => None,
        },
        channel_hash: optional_u16(opts, "channelHash")?,
    })
}

/// Read `iceServers` as what the web platform calls it: an array of
/// `RTCIceServer`, whose `urls` is a string or an array of them and
/// which carries `username`/`credential` for TURN.
///
/// Stage 5 read this with `as_string` on each element, so every
/// object a page passed — the only shape `RTCIceServer` has —
/// evaluated to nothing and the page's ICE configuration was
/// silently absent from the offer. A bare URL string is refused
/// rather than quietly accepted as a second spelling: one contract.
fn parse_ice_servers(opts: &JsValue) -> Result<Vec<IceServer>, JsError> {
    let value = js_sys::Reflect::get(opts, &JsValue::from_str("iceServers"))
        .map_err(|_| JsError::new("iceServers could not be read"))?;
    if value.is_undefined() || value.is_null() {
        return Ok(Vec::new());
    }
    let array = value
        .dyn_into::<js_sys::Array>()
        .map_err(|_| JsError::new("iceServers must be an array of RTCIceServer objects"))?;
    array
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_ice_server(&entry, index))
        .collect()
}

fn parse_ice_server(entry: &JsValue, index: usize) -> Result<IceServer, JsError> {
    let urls_value = js_sys::Reflect::get(entry, &JsValue::from_str("urls"))
        .map_err(|_| JsError::new(&format!("iceServers[{index}] could not be read")))?;
    let urls = match urls_value.as_string() {
        Some(single) => vec![single],
        None => match urls_value.dyn_into::<js_sys::Array>() {
            Ok(list) => list
                .iter()
                .map(|url| {
                    url.as_string().ok_or_else(|| {
                        JsError::new(&format!(
                            "iceServers[{index}].urls must contain only strings"
                        ))
                    })
                })
                .collect::<Result<Vec<String>, JsError>>()?,
            Err(_) => {
                return Err(JsError::new(&format!(
                    "iceServers[{index}] must be an RTCIceServer object whose `urls` is a \
                     string or an array of strings"
                )))
            }
        },
    };
    if urls.is_empty() {
        return Err(JsError::new(&format!(
            "iceServers[{index}].urls is empty, which configures nothing"
        )));
    }
    Ok(IceServer {
        urls,
        username: optional_string(entry, "username"),
        credential: optional_string(entry, "credential"),
    })
}

/// The parsed ICE servers as JSON, for
/// [`LeafNode::effective_ice_servers`].
fn ice_servers_json(servers: &[IceServer]) -> String {
    let mut out = String::from("[");
    for (index, server) in servers.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"urls\":[");
        for (position, url) in server.urls.iter().enumerate() {
            if position > 0 {
                out.push(',');
            }
            out.push_str(&json_string(url));
        }
        out.push(']');
        if let Some(username) = &server.username {
            out.push_str(",\"username\":");
            out.push_str(&json_string(username));
        }
        if let Some(credential) = &server.credential {
            out.push_str(",\"credential\":");
            out.push_str(&json_string(credential));
        }
        out.push('}');
    }
    out.push(']');
    out
}

/// One JSON string literal, escaped — the same one-liner
/// `node.rs` and `leader_session.rs` use for the same job.
fn json_string(raw: &str) -> String {
    serde_json::Value::String(raw.to_string()).to_string()
}

/// A `u64` from a decimal or `0x`-prefixed hex string. The boundary
/// never takes a JS number for a `u64`.
fn parse_u64(raw: &str) -> Result<u64, JsError> {
    crate::bootstrap::parse_node_id(raw)
        .ok_or_else(|| JsError::new(&format!("{raw:?} is not a u64 (decimal or 0x-hex)")))
}
