//! `AnchorControlPlane` — the one v1 implementation of
//! [`ControlPlane`](crate::control_plane::ControlPlane).
//!
//! Everything Layer 0 needs from a native anchor, expressed over the
//! three routes the Stage 4b listener actually serves:
//!
//! | trait method | carrier |
//! |---|---|
//! | [`ControlPlane::offer`] | `POST /rtc/offer` |
//! | [`ControlPlane::trickle`] | `GET /rtc/trickle` (WebSocket) |
//! | [`ControlPlane::signal`] | the same trickle socket |
//! | [`ControlPlane::end_attempt`] | closing that socket |
//! | [`ControlPlane::drain_events`] | frames arriving on it |
//! | [`ControlPlane::publish_announcement`] | **no route** — typed refusal |
//! | [`ControlPlane::query_capability`] | **no route** — typed refusal |
//!
//! `GET /rtc/anchor` is not a trait method: it is [`Self::attach`],
//! because what it exists for is the **pinned-key refusal** and that
//! has to happen before an offer exists. The credential pins the
//! anchor's Noise static key; the live key is fetched and *compared*;
//! a mismatch is refused before any SDP is created and before any
//! handshake is attempted. That refusal is the entire content of the
//! 4b MITM witness, which is why it sits in the constructor rather
//! than somewhere a later reordering could move it.
//!
//! # What crosses the boundary, and what does not
//!
//! Out of this module the leaf receives node ids, `Sdp`,
//! `IceCandidate`, `DialogId`, `SignalEnvelope`, `SignedAnnouncement`
//! and `[u8; 32]` keys. It never receives a `PeerAddr`, an anchor
//! handle, a `Response`, a `WebSocket`, or a Net packet.
//! `tests/control_plane_boundary.rs` asserts that from the outside.
//!
//! The credential goes the other way: it is a *leaf* input (the page
//! hands it to `connect`), it never crosses the trait, and the only
//! thing this module publishes back from it is
//! [`BootstrapAccepted::peer_static`] — the pinned key the leaf then
//! handshakes against.
//!
//! # `signal`, and the gap it names
//!
//! An envelope is self-authenticating (see [`SignalEnvelope`]), so
//! the carrier can be anything. v1 carries it on the bootstrap
//! dialog the anchor already owns — the trickle socket — as
//! `{"type":"signal","dialog":…,"envelope":"<base64>"}`, and admits
//! inbound envelopes the same way. It does **not** ride the data
//! path: a control plane that put a Net packet on the wire would be
//! a relay wearing a trait.
//!
//! The gap, stated plainly: today's listener forwards trickle frames
//! only when `type == "candidate"`
//! (`sdk/src/rtc_bootstrap.rs::trickle_socket`), so an envelope
//! addressed to a third node reaches the anchor and stops there.
//! Peer-to-peer signalling through a native anchor needs one more
//! listener-side case; the leaf half — signing, carrying, verifying
//! — is complete and is exercised end to end by the anchorless mock,
//! which carries the identical envelopes.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use base64::Engine as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

use crate::bootstrap::{AnchorInfo, Credential, OfferAccepted};
use crate::control_plane::{
    BootstrapAccepted, ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp,
    SignalEnvelope, SignedAnnouncement,
};
use crate::error::{LeafError, Result};
use crate::signal;

/// The shared interior. `Rc` because every async method holds a
/// handle across an `await` and the caller reaches this through a
/// `RefCell` it must not keep borrowed — the same reason
/// [`RtcLeafTransport`](crate::rtc::RtcLeafTransport) is `Clone`.
struct State {
    /// The listener's base URL, `https://` (or `http://localhost`).
    bootstrap_url: String,
    /// Layer 0's input. Never crosses the trait.
    credential: Credential,
    /// The node id this leaf claims in the offer.
    self_node: NodeId,
    /// The anchor's node id, from `GET /rtc/anchor`.
    anchor_node: NodeId,
    /// The anchor's published RTC socket, when it has one.
    anchor_rtc_addr: Option<String>,
    /// The dialog `POST /rtc/offer` allocated, once it has.
    dialog: Cell<Option<DialogId>>,
    /// The trickle socket for that dialog.
    trickle: RefCell<Option<WebSocket>>,
    /// What arrived on it, waiting for `drain_events`.
    events: Rc<RefCell<VecDeque<ControlEvent>>>,
    /// The socket's handlers, kept alive for the socket's lifetime.
    handlers: RefCell<Vec<JsValue>>,
}

/// The anchor-backed control plane.
#[derive(Clone)]
pub struct AnchorControlPlane {
    state: Rc<State>,
}

impl AnchorControlPlane {
    /// Fetch `GET /rtc/anchor`, **refuse a key the credential does
    /// not pin**, and return a control plane bound to that anchor.
    ///
    /// The refusal is here, in the constructor, so that no ordering
    /// of the connect sequence can put a handshake before it.
    pub async fn attach(
        bootstrap_url: String,
        credential: Credential,
        self_node: NodeId,
    ) -> Result<Self> {
        let info = AnchorInfo::from_json(&http_get(&format!("{bootstrap_url}/rtc/anchor")).await?)?;
        info.check_pinned_key(&credential)?;
        Ok(Self {
            state: Rc::new(State {
                bootstrap_url,
                credential,
                self_node,
                anchor_node: info.node_id,
                anchor_rtc_addr: info.rtc_addr,
                dialog: Cell::new(None),
                trickle: RefCell::new(None),
                events: Rc::new(RefCell::new(VecDeque::new())),
                handlers: RefCell::new(Vec::new()),
            }),
        })
    }

    /// The anchor's node id.
    #[inline]
    pub fn anchor_node(&self) -> NodeId {
        self.state.anchor_node
    }

    /// The anchor's published RTC socket.
    ///
    /// Not a trait method and deliberately not one: it is not an
    /// address the leaf dials — a browser cannot dial a socket — it
    /// is the STUN probe's only legitimate target and the `rtc_addr`
    /// the `connected` event reports. The trait carries no addresses
    /// at all, so this stays on the implementation that minted it.
    #[inline]
    pub fn anchor_rtc_addr(&self) -> Option<String> {
        self.state.anchor_rtc_addr.clone()
    }

    /// Open `GET /rtc/trickle`, presenting the attempt token as the
    /// WebSocket subprotocol the listener requires.
    fn open_trickle(&self, dialog: DialogId, attempt_token: &str) -> Result<()> {
        let state = &self.state;
        let ws_url = format!(
            "{}/rtc/trickle?dialog={dialog}&node_id={:#x}",
            state
                .bootstrap_url
                .replacen("https://", "wss://", 1)
                .replacen("http://", "ws://", 1),
            state.self_node,
        );
        let socket =
            WebSocket::new_with_str(&ws_url, &format!("net-bootstrap-attempt.{attempt_token}"))
                .map_err(|e| refused(&format!("the trickle socket did not open: {e:?}")))?;

        let events = Rc::clone(&state.events);
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(text) = event.data().as_string() else {
                // Binary frames are not part of this protocol.
                return;
            };
            if let Some(event) = parse_trickle_frame(&text, dialog) {
                events.borrow_mut().push_back(event);
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let events = Rc::clone(&state.events);
        let on_close = Closure::wrap(Box::new(move |event: CloseEvent| {
            // The listener closes with a typed code when it refuses
            // the attempt; surfacing it is what turns a silent ICE
            // deadline into the reason the anchor gave.
            events.borrow_mut().push_back(ControlEvent::AttemptEnded {
                dialog,
                reason: format!(
                    "the anchor closed the bootstrap dialog: {} {}",
                    event.code(),
                    event.reason()
                ),
            });
        }) as Box<dyn FnMut(CloseEvent)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        state.handlers.borrow_mut().push(on_message.into_js_value());
        state.handlers.borrow_mut().push(on_close.into_js_value());
        *state.trickle.borrow_mut() = Some(socket);
        Ok(())
    }

    /// Send one JSON frame on the trickle socket.
    fn send_frame(&self, dialog: DialogId, frame: &serde_json::Value) -> Result<()> {
        if self.state.dialog.get() != Some(dialog) {
            return Err(refused(&format!(
                "dialog {dialog} is not this control plane's bootstrap dialog"
            )));
        }
        let socket = self.state.trickle.borrow();
        let socket = socket
            .as_ref()
            .ok_or_else(|| refused("the bootstrap dialog has no trickle socket"))?;
        socket
            .send_with_str(&frame.to_string())
            .map_err(|e| refused(&format!("the trickle socket refused a frame: {e:?}")))
    }
}

impl ControlPlane for AnchorControlPlane {
    async fn offer(&self, offer: Sdp) -> Result<BootstrapAccepted> {
        let state = &self.state;
        let body = serde_json::json!({
            "credential": state.credential.encoded,
            "node_id": format!("{:#x}", state.self_node),
            "sdp": offer.0,
        })
        .to_string();
        let accepted = OfferAccepted::from_json(
            &http_post_json(&format!("{}/rtc/offer", state.bootstrap_url), &body).await?,
        )?;

        // The socket opens before the leaf installs the answer: the
        // anchor's own candidate is the socket's FIRST frame, and it
        // is what lets the browser start connectivity checks while
        // it is still gathering.
        state.dialog.set(Some(accepted.dialog));
        self.open_trickle(accepted.dialog, &accepted.attempt_token)?;

        Ok(BootstrapAccepted {
            dialog: accepted.dialog,
            answer: Sdp(accepted.sdp),
            // The CREDENTIAL's key, never the one the answer or
            // `GET /rtc/anchor` carried. `attach` already refused a
            // live key that differs from it.
            peer_static: state.credential.anchor_noise_pubkey,
            peer_node: state.anchor_node,
        })
    }

    async fn trickle(&self, dialog: DialogId, candidate: IceCandidate) -> Result<()> {
        // `type` is load-bearing: the listener's trickle handler
        // drops every frame whose `type` is not `"candidate"`.
        self.send_frame(
            dialog,
            &serde_json::json!({
                "type": "candidate",
                "dialog": dialog,
                "candidate": candidate.candidate,
                "mid": candidate.mid,
            }),
        )
    }

    async fn end_attempt(&self, dialog: DialogId) -> Result<()> {
        if self.state.dialog.get() != Some(dialog) {
            // Idempotent by contract: an attempt that is already
            // gone is not an error.
            return Ok(());
        }
        self.state.dialog.set(None);
        if let Some(socket) = self.state.trickle.borrow_mut().take() {
            socket.set_onclose(None);
            let _ = socket.close();
        }
        Ok(())
    }

    async fn publish_announcement(&self, _announcement: SignedAnnouncement) -> Result<()> {
        // Not a placeholder: the listener has no publish route, and
        // §7's reachability path is the data one — the leaf sends its
        // signed announcement to the anchor as a `0x1000` fold frame
        // over the session and the anchor floods it. A control plane
        // that pretended to publish would hide that.
        Err(refused(
            "the bootstrap listener has no announcement-publish route; a leaf \
             publishes over its session as a 0x1000 fold frame, which is §7's \
             reachability path",
        ))
    }

    async fn query_capability(&self, capability: &str) -> Result<Vec<SignedAnnouncement>> {
        Err(refused(&format!(
            "the bootstrap listener has no capability-query route, so {capability:?} \
             cannot be answered from the mesh; a leaf answers `query` from the \
             announcements its own dispatcher verified"
        )))
    }

    async fn signal(&self, envelope: SignalEnvelope) -> Result<()> {
        let dialog = self
            .state
            .dialog
            .get()
            .ok_or_else(|| refused("there is no bootstrap dialog to carry an envelope on"))?;
        let bytes = signal::encode(&envelope)?;
        self.send_frame(
            dialog,
            &serde_json::json!({
                "type": "signal",
                "dialog": envelope.dialog,
                "to": format!("{:#x}", envelope.to),
                "envelope": base64::engine::general_purpose::STANDARD.encode(&bytes),
            }),
        )
    }

    fn drain_events(&self) -> Vec<ControlEvent> {
        self.state.events.borrow_mut().drain(..).collect()
    }
}

/// One inbound trickle frame as a control event.
///
/// A frame carrying a `candidate` string is a candidate whether or
/// not it names its `type` — the listener's own first frame does
/// both, and a leaf that required the tag would drop the anchor's
/// head start. Anything else is ignored rather than guessed at.
fn parse_trickle_frame(text: &str, dialog: DialogId) -> Option<ControlEvent> {
    let document: serde_json::Value = serde_json::from_str(text).ok()?;
    if let Some(line) = document.get("candidate").and_then(|v| v.as_str()) {
        return Some(ControlEvent::Candidate {
            dialog: document
                .get("dialog")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(dialog),
            candidate: IceCandidate {
                candidate: line.to_string(),
                mid: document
                    .get("mid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    .to_string(),
            },
        });
    }
    if document.get("type").and_then(|v| v.as_str()) == Some("signal") {
        let encoded = document.get("envelope").and_then(|v| v.as_str())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        // Structure only. The signature, the window and the
        // seen-set are the LEAF's checks, and they stay the leaf's:
        // a control plane that verified envelopes would be a
        // control plane the leaf trusted.
        return Some(ControlEvent::Signal(signal::decode(&bytes).ok()?));
    }
    None
}

async fn http_get(url: &str) -> Result<String> {
    let window = web_sys::window().ok_or_else(|| refused("no window"))?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| refused(&format!("GET {url} failed: {e:?}")))?;
    read_body(response, url).await
}

async fn http_post_json(url: &str, body: &str) -> Result<String> {
    let window = web_sys::window().ok_or_else(|| refused("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(body));
    let headers = web_sys::Headers::new().map_err(|e| refused(&format!("headers: {e:?}")))?;
    headers
        .set("content-type", "application/json")
        .map_err(|e| refused(&format!("content-type: {e:?}")))?;
    init.set_headers(&headers);
    let request = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| refused(&format!("request: {e:?}")))?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| refused(&format!("POST {url} failed: {e:?}")))?;
    read_body(response, url).await
}

async fn read_body(response: JsValue, url: &str) -> Result<String> {
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| refused("fetch did not return a Response"))?;
    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|e| refused(&format!("body: {e:?}")))?,
    )
    .await
    .map_err(|e| refused(&format!("body: {e:?}")))?
    .as_string()
    .unwrap_or_default();
    if !response.ok() {
        // The listener's refusals are typed JSON; surface the body
        // rather than only the status, or a `BootstrapRefusal`
        // becomes "400".
        return Err(refused(&format!(
            "{url} answered {}: {text}",
            response.status()
        )));
    }
    Ok(text)
}

fn refused(what: &str) -> LeafError {
    LeafError::ControlPlane(what.to_string())
}
