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
//! [`LeafError`](crate::error::LeafError)'s `Display`, and the TS
//! wrapper re-types them. Every `u64` crosses as a decimal
//! **string**: `JSON.parse` rounds integers above 2^53, so a numeric
//! `channel_hash` would silently name the wrong channel.
//!
//! # The control-plane boundary
//!
//! Everything that is not a Net packet crosses
//! [`ControlPlane`](crate::control_plane::ControlPlane), whose one
//! v1 implementation is
//! [`AnchorControlPlane`](crate::anchor_control_plane::AnchorControlPlane):
//! the anchor info and its pinned-key refusal, the offer, the
//! candidate trickle in both directions, the signalling envelopes,
//! and the end of the attempt. This module **drives** that trait and
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

use std::cell::RefCell;
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
use crate::rtc::RtcLeafTransport;
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
    listeners: Vec<js_sys::Function>,
    closed: bool,
}

impl Inner {
    /// Hand every queued datagram to the node, push everything the
    /// node produced to the transport, and fire the listeners.
    fn pump(&mut self) {
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
        self.node.tick(now);
        for out in self.node.take_outbound() {
            if let Err(e) = self.transport.send(out.peer, out.packet) {
                // Admission refusal. Typed, surfaced, and the
                // packet was never enqueued (§2).
                console_error(&format!("net-mesh-leaf: send refused: {e}"));
            }
        }
        let events = self.node.drain_events();
        for event in &events {
            self.emit(event);
        }
    }

    fn emit(&self, event: &LeafEvent) {
        let json = JsValue::from_str(&event.to_json());
        for listener in &self.listeners {
            let _ = listener.call1(&JsValue::NULL, &json);
        }
    }
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
        let ice_servers = string_array(&opts, "iceServers");
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
            listeners: Vec::new(),
            closed: false,
        }));

        // The inbound sink queues; the pump drains. A `Weak` so the
        // closure cannot keep a closed node alive.
        let weak: Weak<RefCell<Inner>> = Rc::downgrade(&inner);
        let sink: crate::rtc::InboundSink = Rc::new(move |peer, bytes| {
            if let Some(inner) = weak.upgrade() {
                if let Ok(mut inner) = inner.try_borrow_mut() {
                    inner.inbox.push_back((peer, bytes));
                }
            }
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
        let offer = transport
            .create_offer(anchor, &ice_servers)
            .await
            .map_err(js)?;
        let accepted = control.offer(offer).await.map_err(js)?;
        transport
            .accept_answer(anchor, &accepted.answer)
            .await
            .map_err(js)?;
        inner.borrow_mut().dialog = accepted.dialog;

        // Layer 0 step 3: wait for the channel, trickling both ways
        // through the boundary.
        wait_for_channel(&inner, anchor).await?;

        // Layer 1: the NKpsk0 handshake. `accepted.peer_static` is
        // the CREDENTIAL's key — the control plane returns what
        // authenticated the attempt, and `attach` already refused a
        // live key that differed from it. This is the whole MITM
        // property.
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
        node.enroll().await?;
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
        let receiver = {
            let mut guard = self.inner.borrow_mut();
            let peer = guard.anchor;
            let receiver = guard
                .node
                .call(
                    peer,
                    &service,
                    &payload.to_vec(),
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    timeout_ms.map(|ms| ms.max(0.0) as u64),
                )
                .map_err(js)?;
            guard.pump();
            receiver
        };
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
        let mut guard = self.inner.borrow_mut();
        let peer = guard.anchor;
        guard.node.subscribe(peer, &channel).map_err(js)?;
        guard.pump();
        Ok(())
    }

    /// Publish `payload` on `channel`.
    pub async fn publish(&self, channel: String, payload: Uint8Array) -> Result<(), JsError> {
        let mut guard = self.inner.borrow_mut();
        let peer = guard.anchor;
        guard
            .node
            .publish(peer, &channel, &payload.to_vec())
            .map_err(js)?;
        guard.pump();
        Ok(())
    }

    /// Open an application stream.
    ///
    /// `opts`: `{ reliability: "reliable" | "fireAndForget",
    /// reliable?: boolean, label?, streamId?, channelHash? }`.
    /// `streamId` / `channelHash` are used verbatim when present, so
    /// a stream can match a publish contract a native handler
    /// dispatches on.
    pub fn open_stream(&self, opts: JsValue) -> Result<LeafStream, JsError> {
        let reliability = match optional_string(&opts, "reliability") {
            Some(spelling) => Reliability::parse(&spelling).ok_or_else(|| {
                JsError::new("reliability must be \"reliable\" or \"fireAndForget\"")
            })?,
            None => match optional_bool(&opts, "reliable") {
                Some(true) | None => Reliability::Reliable,
                Some(false) => Reliability::FireAndForget,
            },
        };
        let label = optional_string(&opts, "label").unwrap_or_else(|| "app".to_string());
        let stream_id = match optional_string(&opts, "streamId") {
            Some(raw) => Some(parse_u64(&raw)?),
            None => None,
        };
        let channel_hash = optional_f64(&opts, "channelHash").map(|v| v as u16);

        let mut guard = self.inner.borrow_mut();
        let peer = guard.anchor;
        let handle = guard
            .node
            .open_stream(peer, &label, reliability, stream_id, channel_hash)
            .map_err(js)?;
        Ok(LeafStream {
            inner: Rc::clone(&self.inner),
            handle,
        })
    }

    /// Build, sign and publish this leaf's announcement.
    ///
    /// `capabilities` become tags alongside the mandatory `leaf` and
    /// `transport:rtc`; `reflex_addr` and `rtc_addr` stay absent.
    pub async fn announce(&self, capabilities: Vec<String>) -> Result<(), JsError> {
        let mut guard = self.inner.borrow_mut();
        let announcement = guard.node.build_announcement(&capabilities).map_err(js)?;
        let peer = guard.anchor;
        // v1's control plane has no publish endpoint; §7's
        // reachability path is the data one — the anchor floods what
        // it receives.
        guard
            .node
            .announce_to_peer(peer, &announcement)
            .map_err(js)?;
        guard.pump();
        Ok(())
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
    /// anything: the anchor's bootstrap dialog here, a room object
    /// in the serverless follow-on, the in-memory mock in the
    /// anchorless test.
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
        let receiver = {
            let mut guard = self.inner.borrow_mut();
            if guard.node.is_enrolled() {
                return Ok(());
            }
            let peer = guard.anchor;
            let invite = guard.invite.clone();
            let receiver = guard
                .node
                .begin_enrollment(peer, &invite, "net-mesh-leaf", &[], None)
                .map_err(js)?;
            guard.pump();
            receiver
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
        let mut guard = self.inner.borrow_mut();
        guard.node.finish_enrollment(&reply).map_err(js)?;
        guard.pump();
        Ok(())
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
    /// Pending calls fail [`RpcError::SessionLost`] — typed, and not
    /// re-issued by anybody.
    pub fn close(&self) {
        let mut guard = self.inner.borrow_mut();
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
        for event in &events {
            guard.emit(event);
        }
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
    pub fn send(&self, payload: Uint8Array) -> Result<(), JsError> {
        let mut guard = self.inner.borrow_mut();
        guard
            .node
            .stream_send(self.handle, &payload.to_vec())
            .map_err(js)?;
        guard.pump();
        Ok(())
    }

    /// Stream data arrives as `stream_data` events on the node's
    /// `on_event`; this registers a listener filtered to this
    /// stream's id.
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
    pub fn close(&self) {
        // Nothing to tear down transport-side: one DataChannel
        // carries every stream, and the wire session owns the
        // per-stream state. Kept so the TS surface is symmetric.
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
        service_control_plane(inner).await.map_err(js)?;
        if inner.borrow().transport.is_open(peer) {
            return Ok(());
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
async fn wait_for_session(inner: &Rc<RefCell<Inner>>, peer: NodeId) -> Result<(), JsError> {
    let mut waited = 0;
    while waited < ICE_DEADLINE_MS {
        service_control_plane(inner).await.map_err(js)?;
        {
            let mut guard = inner.borrow_mut();
            guard.pump();
            if guard.node.has_session(peer) {
                return Ok(());
            }
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }
    Err(js(LeafError::Session(
        "the anchor did not complete the Noise handshake inside the deadline".into(),
    )))
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
            let Ok(mut guard) = inner.try_borrow_mut() else {
                continue;
            };
            if guard.closed {
                return;
            }
            guard.pump();
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

fn optional_f64(opts: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.as_f64())
}

fn string_array(opts: &JsValue, key: &str) -> Vec<String> {
    js_sys::Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Array>().ok())
        .map(|array| array.iter().filter_map(|v| v.as_string()).collect())
        .unwrap_or_default()
}

/// A `u64` from a decimal or `0x`-prefixed hex string. The boundary
/// never takes a JS number for a `u64`.
fn parse_u64(raw: &str) -> Result<u64, JsError> {
    crate::bootstrap::parse_node_id(raw)
        .ok_or_else(|| JsError::new(&format!("{raw:?} is not a u64 (decimal or 0x-hex)")))
}
