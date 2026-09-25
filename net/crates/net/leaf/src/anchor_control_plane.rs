//! `AnchorControlPlane` — the one v1 implementation of
//! [`ControlPlane`].
//!
//! Everything Layer 0 needs from a native anchor, expressed over the
//! three routes the Stage 4b listener actually serves:
//!
//! | trait method | carrier |
//! |---|---|
//! | [`ControlPlane::offer`] | `POST /rtc/offer` |
//! | [`ControlPlane::trickle`] | `GET /rtc/trickle` (WebSocket) |
//! | [`ControlPlane::signal`] | **no route** — typed refusal |
//! | [`ControlPlane::end_attempt`] | closing that socket |
//! | [`ControlPlane::drain_events`] | frames arriving on it |
//! | [`ControlPlane::take_revocation_bundles`] | `org_revocation_bundle` frames (see below) |
//! | [`ControlPlane::publish_announcement`] | **no route** — typed refusal |
//! | [`ControlPlane::query_capability`] | **no route** — typed refusal |
//!
//! # The revocation feed
//!
//! The feed's frame is `{"type":"org_revocation_bundle","bundle":"<base64>"}` —
//! the anchor-protocol vocabulary plus ONE frame type, carrying an
//! organization root's signed revocation bundle **opaque**: this
//! adapter parses the envelope and queues the bytes;
//! [`ControlPlane::take_revocation_bundles`] hands them out in
//! arrival order and the leaf's org module does the verifying and the
//! raise-only merge. "Verified by the leaf, not by the transport",
//! exactly as [`ControlEvent::Announcement`].
//!
//! The frame is recognised on the trickle socket AND on a dedicated
//! org-control socket
//! ([`crate::anchor_control_plane::AnchorControlPlane::open_org_control`]), because the Stage
//! 4b listener's trickle handler originates exactly one frame and
//! cannot be extended from here — a harness that must FEED floors
//! serves the same frame vocabulary on a socket of its own, and the
//! bootstrap URL's `#org-control=<ws-url>` override tag points this
//! adapter at it. Neither socket nor URL crosses the trait.
//!
//! `GET /rtc/anchor` is not a trait method: it is `Self::attach`,
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
//! `IceCandidate`, `DialogId`, `SignalEnvelope`, `SignedAnnouncement`,
//! `[u8; 32]` keys and org-revocation bundle bytes. It never receives
//! a `PeerAddr`, an anchor handle, a `Response`, a `WebSocket`, or a
//! Net packet.
//! `tests/control_plane_boundary.rs` asserts that from the outside.
//!
//! The credential goes the other way: it is a *leaf* input (the page
//! hands it to `connect`), it never crosses the trait, and the only
//! thing this module publishes back from it is
//! [`BootstrapAccepted::peer_static`] — the pinned key the leaf then
//! handshakes against.
//!
//! # `signal`: refused here, on purpose (R14)
//!
//! An envelope is self-authenticating (see [`SignalEnvelope`]), so
//! the carrier can be anything — but this carrier cannot carry it.
//! The Stage 4b listener reads exactly one trickle frame type,
//! `type == "candidate"` (`sdk/src/rtc_bootstrap.rs::trickle_socket`),
//! and forwards nothing to a third party at all. The first cut of
//! this adapter serialised envelopes into a `type: "signal"` frame
//! and reported the local send as delivery; the anchor discarded
//! every one of them.
//!
//! So `signal` returns a typed refusal naming the peer it could not
//! reach. Carrying envelopes end to end needs a listener route that
//! forwards WITH an acknowledgement — generic peer coordination,
//! which is Stage 6's. The trait's shape already admits it, which is
//! why `signal` takes a peer id and an envelope rather than a
//! session; the anchorless `MockControlPlane` carries envelopes for
//! real today, and that is where the leaf's signalling path is
//! witnessed.

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
/// [`RtcLeafTransport`] is `Clone`.
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
    /// The anchor's **separately announced** STUN endpoint, when it
    /// announced one. A different socket from `anchor_rtc_addr`.
    anchor_stun_addr: Option<String>,
    /// The dialog `POST /rtc/offer` allocated, once it has.
    dialog: Cell<Option<DialogId>>,
    /// The trickle socket for that dialog, with everything that dies
    /// with it.
    trickle: RefCell<Option<Trickle>>,
    /// What arrived on it, waiting for `drain_events`.
    events: Rc<RefCell<VecDeque<ControlEvent>>>,
    /// Org-revocation bundles that arrived on the trickle or
    /// org-control socket, waiting for `take_revocation_bundles`.
    /// Verbatim bytes in arrival order: the envelope is parsed, the
    /// bundle never is.
    revocation: Rc<RefCell<VecDeque<Vec<u8>>>>,
    /// The socket's handlers, kept alive for the socket's lifetime.
    handlers: RefCell<Vec<JsValue>>,
}

/// One trickle socket, the dialog it serves, and everything that
/// dies with it (M17/M47).
///
/// The dialog-scoped state lives **with the socket** rather than in
/// `State` — the buffered-frames queue most of all. One shared queue,
/// flushed by whichever `on_open` ran next, had no dialog fence at
/// the flush: a successor's socket trickled its predecessor's stale
/// candidates, or an abandoned socket drained a successor's fresh
/// ones onto the wrong dialog, which is an ICE failure with nothing
/// naming the cause. Keyed like this, a flush can only ever carry
/// this dialog's frames, and dropping the `Trickle` drops the queue
/// with it — the drain-and-discard `end_attempt` owes a dialog.
struct Trickle {
    /// The dialog this socket serves. The socket is addressed by
    /// this, never by whatever dialog the state currently names: a
    /// superseded attempt's socket must still be closeable by the
    /// `end_attempt` that names it, and a stale `end_attempt` must
    /// not close its successor's socket.
    dialog: DialogId,
    socket: WebSocket,
    /// Frames minted before the socket finished opening.
    ///
    /// `WebSocket.send` THROWS while the socket is `CONNECTING`, and
    /// the frame is then gone: the browser does not queue it and the
    /// caller has no way to know a candidate it gathered was never
    /// sent. On loopback the window is small enough to miss; behind a
    /// NAT the frames lost in it are the server-reflexive candidates,
    /// and ICE then fails with nothing naming the cause. Frames wait
    /// here and `onopen` flushes them in order.
    pending: Rc<RefCell<Vec<String>>>,
    /// The socket's handler `Closure`s, kept alive for the socket's
    /// lifetime and detached before the socket is dropped.
    handlers: Vec<JsValue>,
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
    /// The refusal is in [`Self::bind`], below the fetch, so that no
    /// ordering of the connect sequence can put a handshake before
    /// it.
    pub async fn attach(
        bootstrap_url: String,
        credential: Credential,
        self_node: NodeId,
    ) -> Result<Self> {
        // The fetch goes to the listener proper; the `#org-control=`
        // override tag (if any) is not part of its URL.
        let (base, _) = split_org_control(&bootstrap_url);
        let info = AnchorInfo::from_json(&http_get(&format!("{base}/rtc/anchor")).await?)?;
        Self::bind(bootstrap_url, credential, self_node, &info)
    }

    /// Bind to an anchor whose info is already in hand, refusing a
    /// key the credential does not pin.
    ///
    /// Split from [`Self::attach`] because the two halves answer to
    /// different things. The fetch is the listener's; the comparison
    /// and everything it gates — including this adapter's typed
    /// signalling refusal (R14) — is the credential's, and belongs
    /// somewhere a caller holding an anchor document can reach
    /// without a listener to fetch it from. `attach` is that caller
    /// with the fetch; the browser witness for the refusal is that
    /// caller with a document it minted itself, which is the only
    /// way this adapter's own refusal can be observed rather than
    /// a mock's.
    pub fn bind(
        bootstrap_url: String,
        credential: Credential,
        self_node: NodeId,
        info: &AnchorInfo,
    ) -> Result<Self> {
        info.check_pinned_key(&credential)?;
        // The override tag is consumed HERE so no URL this adapter
        // mints (`/rtc/anchor`, `/rtc/offer`, `/rtc/trickle`) can
        // inherit it. Failing that, the page-published feed pointer
        // (`globalThis.__netOrgControl`) — the `window.__netChangeArmed`
        // precedent, a page hook the leaf reads.
        let (bootstrap_url, org_control) = split_org_control(&bootstrap_url);
        let org_control = org_control.or_else(|| {
            let global = js_sys::global();
            let value = js_sys::Reflect::get(&global, &JsValue::from_str("__netOrgControl"))
                .ok()?
                .as_string()
                .filter(|url| !url.is_empty());
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[org-feed] bind read hook={value:?}"
            )));
            value
        });
        let control = Self {
            state: Rc::new(State {
                bootstrap_url,
                credential,
                self_node,
                anchor_node: info.node_id,
                anchor_rtc_addr: info.rtc_addr.clone(),
                anchor_stun_addr: info.stun_addr.clone(),
                dialog: Cell::new(None),
                trickle: RefCell::new(None),
                events: Rc::new(RefCell::new(VecDeque::new())),
                revocation: Rc::new(RefCell::new(VecDeque::new())),
                handlers: RefCell::new(Vec::new()),
            }),
        };
        // A named feed socket that will not open is a FEED THIS
        // CONNECTION WILL NEVER GET, and every revocation witness
        // behind it would silently measure a leaf with floor 0. So
        // this fails loud instead of degrading.
        if let Some(url) = org_control {
            control.open_org_control(url)?;
        }
        Ok(control)
    }

    /// Open the dedicated org-control socket at `url`, carrying the
    /// anchor protocol's `org_revocation_bundle` frame vocabulary.
    ///
    /// Why a second socket exists at all: `GET /rtc/trickle`'s server
    /// (`sdk/src/rtc_bootstrap.rs`) originates exactly one frame — the
    /// anchor's first candidate — so a harness that must FEED
    /// revocation facts needs a socket its own anchor side can send
    /// on. Same protocol, one frame type, and
    /// [`ControlPlane::take_revocation_bundles`] cannot tell which
    /// socket a bundle arrived on — which is the point: the feed's
    /// transport is the carrier's business; the bytes and their order
    /// are the contract.
    ///
    /// [`Self::bind`] opens this automatically when the bootstrap URL
    /// carries the `#org-control=<ws-url>` override tag; this method
    /// is the additive seam for a caller that learns the URL some
    /// other way. Leaks nothing: the URL is an opaque string in,
    /// typed success or refusal out.
    pub fn open_org_control(&self, url: String) -> Result<()> {
        let socket = WebSocket::new(&url)
            .map_err(|e| refused(&format!("the org-control socket did not open: {e:?}")))?;
        let dialog = self.state.dialog.get().unwrap_or(0);
        let events = Rc::clone(&self.state.events);
        let revocation = Rc::clone(&self.state.revocation);
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(text) = event.data().as_string() else {
                // Binary frames are not part of this protocol.
                return;
            };
            route_frame(&text, dialog, &events, &revocation);
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        self.state
            .handlers
            .borrow_mut()
            .push(on_message.into_js_value());
        Ok(())
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
    /// is the **diagnostic** STUN probe's only legitimate target and
    /// the `rtc_addr` the `connected` event reports. The trait
    /// carries no addresses at all, so this stays on the
    /// implementation that minted it.
    ///
    /// It is also this connection's ICE peer, which is why
    /// [`crate::bootstrap::check_ice_servers_against_peer`] compares
    /// a caller's `iceServers` against it.
    #[inline]
    pub fn anchor_rtc_addr(&self) -> Option<String> {
        self.state.anchor_rtc_addr.clone()
    }

    /// The anchor's separately announced STUN endpoint, when it
    /// announced one.
    ///
    /// The sibling of [`Self::anchor_rtc_addr`] and deliberately a
    /// second value rather than a derivation of it: this is the
    /// endpoint a connection with this anchor gathers against, and
    /// `rtc_addr` is the peer it gathers *for*. `None` means the
    /// anchor announced nothing, and the leaf then configures no
    /// ICE servers at all.
    #[inline]
    pub fn anchor_stun_addr(&self) -> Option<String> {
        self.state.anchor_stun_addr.clone()
    }

    /// Take the held trickle socket — the one `names`, or whatever is
    /// held when `names` is `None` (which is what a superseding
    /// offer retires).
    fn take_trickle(state: &State, names: Option<DialogId>) -> Option<Trickle> {
        let mut slot = state.trickle.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|held| names.is_none_or(|dialog| dialog == held.dialog))
        {
            slot.take()
        } else {
            None
        }
    }

    /// Retire a trickle socket for good: detach the handlers so a
    /// queued event cannot reach a dropped `Closure`, then close the
    /// socket — closing it is what hands the anchor's attempt back,
    /// which is what `ControlPlane::end_attempt` means on this
    /// carrier. The handler `Closure`s and any buffered frames drop
    /// with the `Trickle`.
    fn retire(trickle: Trickle) {
        trickle.socket.set_onopen(None);
        trickle.socket.set_onmessage(None);
        trickle.socket.set_onclose(None);
        let _ = trickle.socket.close();
        // The handler `Closure`s drop only after they are detached
        // from the socket; `pending`'s buffered frames drop with the
        // `Trickle`.
        drop(trickle.handlers);
    }

    /// Open `GET /rtc/trickle`, presenting the attempt token as the
    /// WebSocket subprotocol the listener requires.
    fn open_trickle(&self, dialog: DialogId, attempt_token: &str) -> Result<()> {
        let state = &self.state;
        // A new dialog supersedes whatever came before, and closing
        // the old socket is exactly what `end_attempt` does — it is
        // what hands the anchor's superseded attempt back. Left open
        // (the M47 shape), a replaced dialog's socket keeps pushing
        // `ControlEvent`s for a dialog this leaf no longer tracks
        // while the anchor holds the old attempt to its own deadline.
        if let Some(stale) = Self::take_trickle(state, None) {
            Self::retire(stale);
        }
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
        let revocation = Rc::clone(&state.revocation);
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(text) = event.data().as_string() else {
                // Binary frames are not part of this protocol.
                return;
            };
            route_frame(&text, dialog, &events, &revocation);
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

        // The flush. Everything minted while the socket was
        // CONNECTING goes out here, in order, before anything else
        // this leaf sends — a candidate delivered late is still a
        // candidate, but a candidate dropped is an ICE failure with
        // no cause in any log. The queue is this `Trickle`'s own, so
        // the flush is dialog-fenced by construction: no other
        // dialog's frames can be sitting in it.
        let pending: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let pending_on_open = Rc::clone(&pending);
        let sock = socket.clone();
        let on_open = Closure::wrap(Box::new(move |_event: web_sys::Event| {
            for frame in pending_on_open.borrow_mut().drain(..) {
                // Nothing to do about a failure here that the close
                // handler does not already report: the socket is open,
                // so a refusal is the listener's, not a race.
                let _ = sock.send_with_str(&frame);
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        *state.trickle.borrow_mut() = Some(Trickle {
            dialog,
            socket,
            pending,
            handlers: vec![
                on_open.into_js_value(),
                on_message.into_js_value(),
                on_close.into_js_value(),
            ],
        });
        Ok(())
    }

    /// Send one JSON frame on the trickle socket.
    fn send_frame(&self, dialog: DialogId, frame: &serde_json::Value) -> Result<()> {
        if self.state.dialog.get() != Some(dialog) {
            return Err(refused(&format!(
                "dialog {dialog} is not this control plane's bootstrap dialog"
            )));
        }
        let trickle = self.state.trickle.borrow();
        let trickle = trickle
            .as_ref()
            .filter(|held| held.dialog == dialog)
            .ok_or_else(|| refused("the bootstrap dialog has no trickle socket"))?;
        let text = frame.to_string();
        // CONNECTING is not a failure, it is a race with the socket's
        // own handshake. Buffer into THIS dialog's queue — so the
        // flush that later sends it cannot carry another dialog's
        // frames — and its own `onopen` flushes in order.
        if trickle.socket.ready_state() == WebSocket::CONNECTING {
            trickle.pending.borrow_mut().push(text);
            return Ok(());
        }
        trickle
            .socket
            .send_with_str(&text)
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
        // it is still gathering. The dialog slot moves only once its
        // socket exists, so `state.dialog` never names an attempt
        // with no socket behind it.
        self.open_trickle(accepted.dialog, &accepted.attempt_token)?;
        state.dialog.set(Some(accepted.dialog));

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
        // Idempotent by contract: an attempt that is already gone is
        // not an error. But "already gone" and "replaced" are not the
        // same thing (M47): the socket is addressed by the dialog it
        // was opened for, so THIS call closes the socket it names
        // even when the state has moved on to a successor — and the
        // successor's socket is never in reach of a stale call.
        if self.state.dialog.get() == Some(dialog) {
            self.state.dialog.set(None);
        }
        // Taken out of the state before the await below, so no
        // `RefCell` borrow is held across it.
        let Some(trickle) = Self::take_trickle(&self.state, Some(dialog)) else {
            return Ok(());
        };
        // The attempt is over: frames buffered for it must not be
        // flushed by an `onopen` that fires during the wait below.
        trickle.pending.borrow_mut().clear();
        // Closing CONNECTING aborts the upgrade: the anchor never gets
        // a socket whose close can retire the accepted offer. Firefox
        // can delay upgrades after earlier aborted connections, so even
        // a seconds-long failed connect can still be in this state.
        // Keep the handback alive for a bounded establishment window;
        // RTC resources have already been closed by the caller.
        for _ in 0..150 {
            if trickle.socket.ready_state() != WebSocket::CONNECTING {
                break;
            }
            if crate::bootstrap::gloo_timer_sleep(100).await.is_err() {
                break;
            }
        }
        let established = trickle.socket.ready_state() == WebSocket::OPEN;
        Self::retire(trickle);
        if !established {
            return Err(refused("the trickle socket did not open before attempt handback; the anchor must expire the dialog"));
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

    /// **Refused, typed** (R14).
    ///
    /// This used to serialise the envelope into a `type: "signal"`
    /// frame on the trickle socket and report the local send as
    /// success. The Stage 4b listener reads exactly one frame type —
    /// `type: "candidate"` — and drops everything else without a
    /// word (`sdk/src/rtc_bootstrap.rs`), so every such envelope was
    /// discarded by the anchor while the caller was told it had been
    /// delivered. A control plane that reports delivery it cannot
    /// perform is worse than one that cannot perform it.
    ///
    /// The honest v1 answer is a typed refusal. Carrying envelopes
    /// end to end needs a listener route that forwards them to
    /// another peer WITH an acknowledgement, which is generic peer
    /// coordination and belongs to Stage 6; the trait's shape
    /// already admits it, which was the point of taking a peer id
    /// and an envelope rather than a session.
    ///
    /// The anchorless `MockControlPlane` carries envelopes for real
    /// and is unaffected: the leaf's signalling path is exercised
    /// there, against a carrier that actually delivers.
    async fn signal(&self, envelope: SignalEnvelope) -> Result<()> {
        Err(LeafError::ControlPlane(format!(
            "this anchor control plane cannot carry a signalling envelope to {:#x}: the \
             Stage 4b bootstrap listener serves POST /rtc/offer, GET /rtc/anchor and the \
             trickle socket's `candidate` frames, and forwards nothing to a third party. \
             Peer-to-peer signalling over an anchor is Stage 6 work; refusing here is \
             deliberate, because the previous behaviour reported a local send as a \
             delivery the anchor discarded",
            envelope.to
        )))
    }

    fn drain_events(&self) -> Vec<ControlEvent> {
        self.state.events.borrow_mut().drain(..).collect()
    }

    fn take_revocation_bundles(&mut self) -> Vec<Vec<u8>> {
        self.state.revocation.borrow_mut().drain(..).collect()
    }
}

/// Route one inbound anchor-protocol frame: the revocation feed's
/// bundle first, then the trickle vocabulary.
///
/// One router for both sockets, so a bundle is a bundle no matter
/// which socket carried it and the two paths cannot drift into
/// disagreeing about what a frame means.
fn route_frame(
    text: &str,
    dialog: DialogId,
    events: &RefCell<VecDeque<ControlEvent>>,
    revocation: &RefCell<VecDeque<Vec<u8>>>,
) {
    if let Some(bundle) = parse_revocation_frame(text) {
        revocation.borrow_mut().push_back(bundle);
        return;
    }
    if let Some(event) = parse_trickle_frame(text, dialog) {
        events.borrow_mut().push_back(event);
    }
}

/// The revocation feed's one control frame.
///
/// `{"type":"org_revocation_bundle","bundle":"<standard base64>"}` —
/// the trickle vocabulary plus one frame type, carrying an org root's
/// signed revocation bundle. The bytes stay opaque: this parses the
/// envelope, never the bundle, and the signature check stays with the
/// leaf's org module (`ControlPlane`'s "verified by the leaf, not by
/// the transport" rule). Anything else returns `None`, so the trickle
/// parser still decides what any other frame means.
fn parse_revocation_frame(text: &str) -> Option<Vec<u8>> {
    let document: serde_json::Value = serde_json::from_str(text).ok()?;
    if document.get("type").and_then(|v| v.as_str()) != Some("org_revocation_bundle") {
        return None;
    }
    let encoded = document.get("bundle").and_then(|v| v.as_str())?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

/// Split the `#org-control=<ws-url>` override tag off a bootstrap
/// URL.
///
/// The tag is how a harness tells [`Self::AnchorControlPlane`] where
/// its anchor-side revocation feed lives without a second connect
/// parameter: the Stage 4b listener's trickle handler originates
/// exactly one frame, so the runner serves the same frame vocabulary
/// on a socket of its own and the bootstrap URL carries the pointer.
/// Everything else about the URL is unchanged; a URL with no tag is
/// `(whole, None)`.
fn split_org_control(bootstrap_url: &str) -> (String, Option<String>) {
    match bootstrap_url.split_once("#org-control=") {
        Some((base, url)) if !url.is_empty() => (base.to_string(), Some(url.to_string())),
        _ => (bootstrap_url.to_string(), None),
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
