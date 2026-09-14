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
//! # `AnchorControlPlane`, and what v1 does not have
//!
//! The Stage 4b listener serves exactly three routes:
//! `POST /rtc/offer`, `GET /rtc/anchor`, `GET /rtc/trickle`. So
//! `offer`, `trickle` and `end_attempt` are real HTTP/WebSocket
//! calls, `publish_announcement` rides the **data path** (a `0x1000`
//! fold frame to the anchor, which is §7's reachability path — the
//! anchor floods it), and `query_capability` returns a typed refusal
//! naming the absent endpoint. That is not a placeholder: a leaf
//! answers [`LeafNode::query`] from the announcements its dispatcher
//! verified, which is the mechanism §7 describes. The trait's shape
//! is what lets a serverless Tier A control plane answer it
//! directly instead.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, WebSocket};

use crate::bootstrap::{
    classify_ice_failure, gloo_timer_sleep, stun_probe_failed, AnchorInfo, Credential,
    OfferAccepted,
};
use crate::clock;
use crate::control_plane::{IceCandidate, NodeId, SignalKind};
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
    anchor_rtc_addr: Option<String>,
    /// The trickle socket for the bootstrap dialog, kept open for
    /// candidate trickle in both directions.
    trickle: Option<WebSocket>,
    dialog: u64,
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

        // Identity: injected if the host supplies one (custodial
        // model, same shape as `MeshNodeConfig::entity_keypair`),
        // generated from the platform CSPRNG otherwise.
        let identity = match optional_string(&opts, "entitySecretHex") {
            Some(hex) => {
                let secret: [u8; 32] = crate::identity::unhex(&hex)
                    .map_err(js)?
                    .try_into()
                    .map_err(|_| JsError::new("entitySecretHex must be 32 bytes"))?;
                let noise: [u8; 32] = match optional_string(&opts, "noiseSecretHex") {
                    Some(hex) => crate::identity::unhex(&hex)
                        .map_err(js)?
                        .try_into()
                        .map_err(|_| JsError::new("noiseSecretHex must be 32 bytes"))?,
                    None => random32().map_err(js)?,
                };
                LeafIdentity::from_secrets(EntityKeypair::from_secret(secret), noise)
            }
            None => LeafIdentity::generate().map_err(js)?,
        };

        // Layer 0 step 1: the live anchor info, for COMPARISON
        // against the credential's pinned key.
        let info = AnchorInfo::from_json(&http_get(&format!("{bootstrap_url}/rtc/anchor")).await?)
            .map_err(js)?;
        info.check_pinned_key(&credential).map_err(js)?;

        let node_id = identity.node_id();
        let seed = u64::from_le_bytes(
            random32().map_err(js)?[..8]
                .try_into()
                .map_err(|_| JsError::new("seed"))?,
        );
        let mut node = crate::node::LeafNode::new(identity, seed);
        node.set_peer_rtc_addr(info.node_id, info.rtc_addr.clone());

        let inner = Rc::new(RefCell::new(Inner {
            node,
            transport: RtcLeafTransport::new(Rc::new(|_, _| {})),
            anchor: info.node_id,
            anchor_rtc_addr: info.rtc_addr.clone(),
            trickle: None,
            dialog: 0,
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

        // Layer 0 step 2: the offer. The transport is cloned out of
        // the `RefCell` first: holding the borrow across the await
        // would deadlock against the pump on the next tick.
        let transport = inner.borrow().transport.clone();
        let offer = transport
            .create_offer(info.node_id, &ice_servers)
            .await
            .map_err(js)?;

        let body = serde_json::json!({
            "credential": credential.encoded,
            "node_id": format!("{node_id:#x}"),
            "sdp": offer.0,
        })
        .to_string();
        let accepted = OfferAccepted::from_json(
            &http_post_json(&format!("{bootstrap_url}/rtc/offer"), &body).await?,
        )
        .map_err(js)?;

        transport
            .accept_answer(
                info.node_id,
                &crate::control_plane::Sdp(accepted.sdp.clone()),
            )
            .await
            .map_err(js)?;
        inner.borrow_mut().dialog = accepted.dialog;

        // Layer 0 step 3: the trickle socket, authorised by the
        // attempt token presented as the WebSocket subprotocol.
        let socket = open_trickle(
            &bootstrap_url,
            node_id,
            accepted.dialog,
            &accepted.attempt_token,
            Rc::downgrade(&inner),
        )?;
        inner.borrow_mut().trickle = Some(socket);

        // Layer 0 step 4: wait for the channel, trickling both ways.
        wait_for_channel(&inner, info.node_id).await?;

        // Layer 1: the NKpsk0 handshake, against the CREDENTIAL's
        // key. This is the whole MITM property.
        let msg1 = {
            let mut guard = inner.borrow_mut();
            let slot = guard.transport.next_slot();
            let packet = guard
                .node
                .begin_handshake(
                    info.node_id,
                    &credential.psk,
                    &credential.anchor_noise_pubkey,
                    slot,
                )
                .map_err(js)?;
            guard
                .transport
                .send(info.node_id, packet.clone())
                .map_err(js)?;
            packet
        };
        debug_assert!(!msg1.is_empty());
        wait_for_session(&inner, info.node_id).await?;

        start_ticker(Rc::downgrade(&inner));
        Ok(LeafNode { inner })
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

    /// Sign and send a `0x0D02` signalling envelope to `peer`
    /// through the control plane — no session with `peer` needed.
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
        let envelope =
            self.inner
                .borrow()
                .node
                .sign_signal(peer, dialog as u64, kind, payload.to_vec());
        let bytes = crate::signal::encode(&envelope).map_err(js)?;
        // v1 carries envelopes on the anchor session, which the
        // anchor forwards blind — it can read nothing that verifies.
        let mut guard = self.inner.borrow_mut();
        let anchor = guard.anchor;
        let handle = guard
            .node
            .open_stream(anchor, "signal", Reliability::Reliable, None, None)
            .map_err(js)?;
        guard.node.stream_send(handle, &bytes).map_err(js)?;
        guard.pump();
        Ok(())
    }

    /// Register an event listener. Each receives one JSON string per
    /// event.
    pub fn on_event(&self, callback: js_sys::Function) {
        self.inner.borrow_mut().listeners.push(callback);
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
        if let Some(socket) = guard.trickle.take() {
            let _ = socket.close();
        }
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

/// Poll until the DataChannel opens, trickling candidates outward,
/// or fail with the corrected ICE typing.
async fn wait_for_channel(inner: &Rc<RefCell<Inner>>, peer: NodeId) -> Result<(), JsError> {
    let mut waited = 0;
    while waited < ICE_DEADLINE_MS {
        {
            let guard = inner.borrow();
            for (peer, candidate) in guard.transport.take_local_candidates() {
                debug_assert_eq!(peer, guard.anchor);
                send_local_candidate(&guard, &candidate);
            }
            if guard.transport.is_open(peer) {
                return Ok(());
            }
        }
        gloo_timer_sleep(TICK_MS).await.ok();
        waited += TICK_MS;
    }

    // The deadline passed. The HTTPS bootstrap demonstrably
    // succeeded (we have an answer), so the probe is the one thing
    // that can turn the classification.
    let rtc_addr = inner.borrow().anchor_rtc_addr.clone();
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

fn send_local_candidate(inner: &Inner, candidate: &IceCandidate) {
    if let Some(socket) = &inner.trickle {
        let message = serde_json::json!({
            "candidate": candidate.candidate,
            "mid": candidate.mid,
        })
        .to_string();
        let _ = socket.send_with_str(&message);
    }
}

/// Open `GET /rtc/trickle`, presenting the attempt token as the
/// WebSocket subprotocol the listener requires.
fn open_trickle(
    bootstrap_url: &str,
    node_id: NodeId,
    dialog: u64,
    attempt_token: &str,
    inner: Weak<RefCell<Inner>>,
) -> Result<WebSocket, JsError> {
    let ws_url = format!(
        "{}/rtc/trickle?dialog={dialog}&node_id={node_id:#x}",
        bootstrap_url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    let socket =
        WebSocket::new_with_str(&ws_url, &format!("net-bootstrap-attempt.{attempt_token}"))
            .map_err(|e| JsError::new(&format!("the trickle socket did not open: {e:?}")))?;

    let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
        let Some(text) = event.data().as_string() else {
            return;
        };
        let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        let Some(line) = document.get("candidate").and_then(|v| v.as_str()) else {
            return;
        };
        let mid = document
            .get("mid")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .to_string();
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let (transport_peer, candidate) = {
            let guard = inner.borrow();
            (
                guard.anchor,
                IceCandidate {
                    mid,
                    candidate: line.to_string(),
                },
            )
        };
        wasm_bindgen_futures::spawn_local(async move {
            // Clone the transport out and drop the borrow before
            // awaiting: the pump borrows the same cell every tick.
            let transport = inner.borrow().transport.clone();
            if let Err(e) = transport
                .add_remote_candidate(transport_peer, &candidate)
                .await
            {
                console_error(&format!("net-mesh-leaf: remote candidate: {e}"));
            }
        });
    }) as Box<dyn FnMut(MessageEvent)>);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
    Ok(socket)
}

/// The periodic tick: deadlines and reassembly expiry keep moving on
/// an otherwise silent connection.
fn start_ticker(inner: Weak<RefCell<Inner>>) {
    wasm_bindgen_futures::spawn_local(async move {
        loop {
            gloo_timer_sleep(TICK_MS).await.ok();
            let Some(inner) = inner.upgrade() else {
                return;
            };
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

async fn http_get(url: &str) -> Result<String, JsError> {
    let window = web_sys::window().ok_or_else(|| JsError::new("no window"))?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| JsError::new(&format!("GET {url} failed: {e:?}")))?;
    read_body(response, url).await
}

async fn http_post_json(url: &str, body: &str) -> Result<String, JsError> {
    let window = web_sys::window().ok_or_else(|| JsError::new("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(body));
    let headers = web_sys::Headers::new().map_err(|e| JsError::new(&format!("headers: {e:?}")))?;
    headers
        .set("content-type", "application/json")
        .map_err(|e| JsError::new(&format!("content-type: {e:?}")))?;
    init.set_headers(&headers);
    let request = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| JsError::new(&format!("request: {e:?}")))?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| JsError::new(&format!("POST {url} failed: {e:?}")))?;
    read_body(response, url).await
}

async fn read_body(response: JsValue, url: &str) -> Result<String, JsError> {
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| JsError::new("fetch did not return a Response"))?;
    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|e| JsError::new(&format!("body: {e:?}")))?,
    )
    .await
    .map_err(|e| JsError::new(&format!("body: {e:?}")))?
    .as_string()
    .unwrap_or_default();
    if !response.ok() {
        // The listener's refusals are typed JSON; surface the body
        // rather than only the status, or a `BootstrapRefusal`
        // becomes "400".
        return Err(JsError::new(&format!(
            "{url} answered {}: {text}",
            response.status()
        )));
    }
    Ok(text)
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
