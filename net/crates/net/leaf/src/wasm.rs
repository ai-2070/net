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
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::anchor_control_plane::AnchorControlPlane;
use crate::bootstrap::{classify_ice_failure, gloo_timer_sleep, stun_probe_failed, Credential};
use crate::clock;
use crate::control_plane::{
    ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp, SignalKind,
};
use crate::error::LeafError;
use crate::identity::{EntityKeypair, LeafIdentity};
use crate::node::{Inbound, LeafEvent, StreamHandle};
use crate::rtc::{IceServer, RtcLeafTransport};
use crate::stream::Reliability;

/// How long `connect` waits for the DataChannel to open.
///
/// Past it, the attempt fails with the corrected typing: an ICE
/// timeout, upgraded to `UdpBlocked` only if the STUN probe against
/// the anchor's published `rtc_addr` also failed.
pub const ICE_DEADLINE_MS: i32 = 10_000;

/// How long a browser ↔ browser attempt waits for ICE.
///
/// The same budget the bootstrap attempt gets. One number, because
/// "how long before ICE is hopeless" is a property of the network
/// and the browser, not of which peer is at the other end — and two
/// numbers would be two answers to it.
pub const PEER_ICE_DEADLINE_MS: u64 = ICE_DEADLINE_MS as u64;

/// How long a relayed session's handshake may take (§9 step 2).
///
/// Two anchor hops and one Noise round trip, nothing gathered and
/// nothing probed, so this is generous rather than tuned. It is
/// separate from the ICE deadline because it measures a different
/// thing: a peer that never answers here is discoverable but not
/// reachable through the anchor, which is not an ICE outcome and
/// must not be counted as one.
const RELAY_SESSION_DEADLINE_MS: i32 = 5_000;

/// The prefix a `peer_offer` refusal carries when the peer has not
/// been discovered.
///
/// Pinned as a constant because `@net-mesh/browser` types
/// `connectPeer`'s `noAnnouncement` outcome off it, which is the
/// same discipline `errors.ts`'s `ICE_TIMEOUT_DISPLAY` already
/// follows for the taxonomy: the wasm boundary carries a `Display`
/// string, so the string is the contract. The browser ↔ browser
/// witnesses drive the outcome end to end, so a drift here is a red
/// test rather than a silently misclassified failure.
pub const NO_ANNOUNCEMENT_PREFIX: &str = "no verified announcement for";

/// The prefix a peer call carries when the attempt it names is gone
/// — superseded by a newer one, or ended. Typed by `connectPeer` as
/// `superseded`.
pub const NO_LIVE_ATTEMPT_PREFIX: &str = "no live attempt with";

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

/// Which half of §9 this leaf plays in one attempt.
///
/// Provisioned by the call that started the attempt, never inferred:
/// the offerer is the Noise **initiator** (plan §9 step 4, "a Noise
/// handshake runs over it" in the offerer's role), and a leaf that
/// guessed its role from "do I have a handshake in flight" would
/// report a role confusion as a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerRole {
    /// This leaf created the offer, so it drives ICE and runs the
    /// Noise handshake.
    Offerer,
    /// This leaf answered, so it waits for message 1.
    Answerer,
}

/// One terminal ICE disposition. Exactly one moves per counted
/// attempt, at most once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IceTerm {
    /// A direct session was **installed** — not merely a channel
    /// opened.
    Direct,
    /// The attempt's own deadline passed with ICE never connected,
    /// and no UDP-blocked evidence. A relayed session is not a
    /// failed one.
    Relayed,
    /// The attempt ended without installing for a reason other than
    /// its deadline.
    Failed,
    /// The deadline passed **and**
    /// [`crate::error::UdpBlockedEvidence`] was actually
    /// established. Never claimed without it: the `None` arm of that
    /// evidence is an ICE timeout, and a term that claims a
    /// narrower cause than its evidence supports gets read as a
    /// diagnosis.
    UdpBlocked,
}

/// One browser ↔ browser attempt, as the page drives it.
///
/// The page names peers; this holds everything else, which is the
/// security property of the surface. The SDP never crosses the
/// boundary, and neither does a Noise key: the peer's static key
/// comes from its signature-verified announcement and nowhere else.
struct PeerDialog {
    /// The dialog both sides number this exchange with. Minted by
    /// the offerer; the answerer reads it off the signed envelope.
    dialog: DialogId,
    /// This leaf's half.
    role: PeerRole,
    /// Verified envelope payloads that arrived and have not been
    /// applied — the answer, and the peer's trickled candidates.
    /// Filed by [`Inner::collect`], which sees every event the node
    /// produces, so verification has already happened: an envelope
    /// only becomes a [`LeafEvent::Signal`] after
    /// [`crate::node::LeafNode::accept_signal`] checked its
    /// signature against the sender's announcement, its `not_after`
    /// and the replay set.
    inbox: VecDeque<(SignalKind, Vec<u8>)>,
    /// Local candidates gathered for this peer, waiting to be signed
    /// and sent.
    local: VecDeque<IceCandidate>,
    /// When this attempt gives up on ICE.
    deadline: crate::clock::Deadline,
    /// Whether one of the four terminal terms has already counted
    /// for this attempt.
    settled: bool,
}

/// One filed re-attempt trigger: where it came from, and which peer
/// it names.
///
/// `None` is an `online` event — the network came back, which
/// belongs to every pair this leaf owns a repair for rather than to
/// one. `Some(peer)` is that peer's ICE giving up.
type RetryTrigger = (crate::retry::TriggerSource, Option<NodeId>);

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
    /// The trust domain's NKpsk0 pre-shared key, from the same
    /// credential the anchor handshake used.
    ///
    /// Held because a browser ↔ browser handshake runs the **same**
    /// NKpsk0 with the same domain key (plan §9: it is the §5
    /// sequence, not a second protocol). The page never supplies it
    /// and never sees it.
    psk: [u8; 32],
    /// The ICE configuration `connect` was handed.
    ///
    /// Held so a peer attempt is configured **identically** to the
    /// bootstrap one: [`crate::rtc::new_connection`]'s own comment
    /// says why two copies of this are two chances to differ, and a
    /// browser ↔ browser attempt where only one side has a STUN
    /// server is a connectivity bug whose only symptom is a
    /// deadline.
    ice_servers: Vec<IceServer>,
    /// The latest **verified** offer each peer sent that this leaf
    /// has not answered yet: `peer` → `(dialog, SDP bytes)`.
    ///
    /// The one envelope that arrives with no attempt to file it on,
    /// because it is the invitation to start one. Kept here rather
    /// than handed to the page so that `peer_accept_offer` answers
    /// the SDP that was *signed*: a page that passed the offer back
    /// in could pass a different one. One entry per peer — a peer
    /// that re-offers supersedes its own previous offer, which is
    /// also what a re-offer means on the wire.
    offers: HashMap<NodeId, (DialogId, Vec<u8>)>,
    /// Live browser ↔ browser attempts, keyed by peer (§9).
    peers: HashMap<NodeId, PeerDialog>,
    /// Local candidates gathered for the **bootstrap** dialog,
    /// waiting for [`ControlPlane::trickle`].
    ///
    /// The transport gathers for every peer into one queue and
    /// [`RtcLeafTransport::take_local_candidates`] drains it, so the
    /// bootstrap trickle and a peer's signed `Candidate` envelopes
    /// would race for each other's candidates if either drained it
    /// directly. [`Inner::harvest_candidates`] is the single drain
    /// and sorts them into here and into each dialog's own queue.
    anchor_candidates: VecDeque<IceCandidate>,
    /// Whether the bootstrap attempt has already counted one of the
    /// four terminal ICE terms.
    bootstrap_settled: bool,
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
    // ───────── the network-change re-attempt owner (slice 3) ─────────
    /// The one owner of the network-change re-attempt.
    ///
    /// Both trigger sources reach it and nothing else decides: see
    /// [`crate::retry::RetryPolicy`].
    retry: crate::retry::RetryPolicy,
    /// Whether the window listeners are installed.
    ///
    /// Opt-in, and the page's call — see
    /// [`LeafNode::arm_network_retry`]. Idempotent: a second arming
    /// is a no-op, not a second listener, because two listeners for
    /// one `online` event are two triggers for one network change
    /// and the whole row is about that not becoming two attempts.
    retry_armed: bool,
    /// The `online`/`offline` listeners, kept alive for the node's
    /// lifetime. Dropping a closure would detach the callback.
    retry_listeners: Vec<Closure<dyn FnMut(JsValue)>>,
    /// Triggers filed and not yet decided.
    ///
    /// **The single funnel.** The `online` listener runs inside a JS
    /// callback and the ICE watcher inside another
    /// ([`crate::rtc::RtcLeafTransport::take_ice_failures`]), so
    /// neither may re-enter the `RefCell` the pump may hold. They
    /// file here; the ticker drains and [`Inner::retry`] decides.
    /// A `None` peer is an `online` event, which belongs to every
    /// peer this leaf owns a re-attempt for rather than to one.
    retry_triggers: Rc<RefCell<VecDeque<RetryTrigger>>>,
    /// Peers this leaf took a DIRECT session with **as the
    /// offerer**, and therefore owns the re-attempt for.
    ///
    /// The answerer must not re-offer: §9 step 4 gives the offerer
    /// the initiating role, and two leaves re-offering each other
    /// on one network change is two attempts per pair plus a
    /// supersession race. So the side that offered owns the repair
    /// and the other side answers it, exactly as it answered the
    /// first one.
    direct_offerer: std::collections::HashSet<NodeId>,
    /// The last re-attempt's disposition, for `retryReport()`.
    retry_last: Option<&'static str>,
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
        while let Some((transport_peer, bytes)) = self.inbox.pop_front() {
            match self.node.classify_datagram(transport_peer, bytes) {
                Inbound::Refused => {}
                Inbound::Session { from, packet, .. } => {
                    self.node.on_datagram(from, packet, now);
                }
                Inbound::Handshake {
                    from,
                    packet,
                    relayed,
                } => self.on_handshake(from, &packet, relayed),
            }
        }
        self.flush();
    }

    /// Run one inbound Noise handshake packet.
    ///
    /// Three cases, and the discriminator is never "do I have a
    /// session", because §9 step 4's whole point is a handshake that
    /// arrives *while* one exists:
    ///
    /// 1. **Message 2 for a handshake this leaf started.** The
    ///    anchor's bootstrap answer and a peer's answer to
    ///    `peer_handshake` both land here, unchanged from Stage 5.
    /// 2. **Message 1 from a peer whose attempt this leaf owns.**
    ///    Either the relayed session being established (§9 step 2)
    ///    or the direct session replacing it (step 4). Answered with
    ///    this leaf's own Noise static key; the initiator's key is
    ///    never needed, which is why NKpsk0's responder half can run
    ///    on nothing but the domain PSK.
    /// 3. **Anything else** is refused and logged, which is Stage
    ///    5's disposition for an unexpected handshake and stays it.
    ///
    /// **The bounds on case 2 are the whole of its safety.** A
    /// *relayed* message 1 may only ESTABLISH — it is refused when a
    /// session already exists — so nothing a relay replays can
    /// displace a live session, let alone downgrade a direct one to
    /// a relayed one. A *direct* message 1 may replace, because it
    /// arrived on a DataChannel this leaf negotiated with that peer
    /// inside an attempt it is driving; that is §9 step 4 and it
    /// goes through `LeafNode::install_session`'s
    /// unchanged replacement fence.
    fn on_handshake(&mut self, from: NodeId, packet: &[u8], relayed: bool) {
        if self.node.is_handshaking(from) {
            if let Err(e) = self.node.complete_handshake(from, packet) {
                console_error(&format!("net-mesh-leaf: handshake: {e}"));
            } else if from != self.anchor && self.transport.is_open(from) {
                // An OPEN channel to this peer is what makes the
                // session that just installed the DIRECT one:
                // `peer_handshake` refuses to begin unless the
                // channel is open, so nothing else can reach here
                // with one. Deliberately not `!relayed`: which path
                // message 2 happened to arrive on is the answerer's
                // addressing detail, and reading it here made a
                // direct session look relayed to the side that
                // initiated it.
                self.direct_installed(from);
            }
            return;
        }
        let admitted = from != self.anchor
            && if relayed {
                // Establish only, and only for a peer whose signed
                // announcement this leaf verified — which
                // `classify_datagram` already required to resolve
                // `from` at all.
                !self.node.has_session(from)
            } else {
                // Replace, but only inside an attempt this leaf is
                // driving.
                self.peers.contains_key(&from)
            };
        if !admitted {
            console_error(&format!(
                "net-mesh-leaf: unexpected handshake from {from:#x} (relayed: {relayed})"
            ));
            return;
        }
        let slot = self.transport.next_slot();
        let msg2 = match self.node.accept_handshake(from, &self.psk, packet, slot) {
            Ok(msg2) => msg2,
            Err(e) => {
                console_error(&format!("net-mesh-leaf: responder handshake: {e}"));
                if !relayed {
                    self.settle(from, IceTerm::Failed);
                }
                return;
            }
        };
        if relayed {
            // The answer goes back the way message 1 came. Set
            // before the send, because addressing is what makes the
            // send reach a peer with no channel of its own.
            self.node.set_peer_relay(from, self.anchor);
        } else {
            // **Message 2 goes back on the channel message 1 arrived
            // on, and the relay is dropped BEFORE the send.** Both
            // halves of that were a real bug: this side installs the
            // direct session here, and a relay entry still in place
            // sent its answer back through the anchor — so the
            // initiator completed its handshake on a RELAYED packet,
            // never saw a direct arrival, and sat waiting for its
            // own relay entry to clear until the deadline. The pair
            // is direct from this moment; its addressing has to say
            // so before anything else leaves.
            self.direct_installed(from);
        }
        let out = self.node.route_outbound(from, msg2);
        if let Err(e) = self.transport.send(out.peer, out.packet) {
            console_error(&format!("net-mesh-leaf: responder message 2: {e}"));
            if !relayed {
                self.settle(from, IceTerm::Failed);
            }
        }
    }

    /// A direct session with `peer` is installed: stop relaying for
    /// it, and count the attempt.
    ///
    /// §9 step 4 from the addressing side. The session replacement
    /// already happened inside
    /// `LeafNode::install_session` — the Stage 3/4a
    /// fence, untouched — and this is the part that has to follow
    /// it: a relay entry left behind would keep wrapping packets for
    /// a pair that now has its own channel.
    fn direct_installed(&mut self, peer: NodeId) {
        self.node.clear_peer_relay(peer);
        // The re-attempt owner's eligibility set, recorded here
        // because this is the one place a direct session is known to
        // be installed AND the role that installed it is still
        // readable. The offerer owns the repair (see
        // [`Inner::direct_offerer`]); the answerer records nothing
        // and re-offers nothing.
        if self.peers.get(&peer).map(|d| d.role) == Some(PeerRole::Offerer) {
            self.direct_offerer.insert(peer);
        }
        self.settle(peer, IceTerm::Direct);
    }

    /// Is `peer`'s direct path — one this leaf offered — not
    /// carrying traffic?
    ///
    /// The only reading that makes a re-attempt meaningful, and the
    /// `interrupted` argument [`crate::retry::RetryPolicy::note`]
    /// takes. Two facts, both required: this leaf took the pair
    /// direct as the offerer, and the direct path is not usable
    /// right now.
    ///
    /// **Three conditions, not two.** The first version read only
    /// the session — a session exists and no relay entry stands for
    /// it, i.e. `peer_candidate`'s `direct` inverted — and that is
    /// wrong in the exact case a repair is for: a DataChannel that
    /// closed leaves the session installed and unrelayed, so the
    /// pair reads DIRECT while no byte can leave. That is the
    /// "silent half-working fallback" §10 excludes, and a trigger
    /// that consulted it would have decided there was nothing to
    /// repair. So the transport is part of the reading: `is_open`
    /// is what says the direct path can carry anything.
    ///
    /// A pair whose channel is open, whose session is installed and
    /// which holds no relay entry is healthy and is not
    /// re-attempted, however many network changes arrive.
    fn direct_interrupted(&self, peer: NodeId) -> bool {
        if !self.direct_offerer.contains(&peer) {
            return false;
        }
        !(self.node.has_session(peer)
            && self.node.peer_relay(peer).is_none()
            && self.transport.is_open(peer))
    }

    /// Drain the transport's ICE `disconnected` → `failed`
    /// observations into the trigger queue.
    ///
    /// One of the two sources; the other is the `online` listener.
    /// Both file into `retry_triggers` and neither decides anything,
    /// which is what makes the owner single.
    fn harvest_ice_failures(&mut self) {
        for peer in self.transport.take_ice_failures() {
            self.retry_triggers
                .borrow_mut()
                .push_back((crate::retry::TriggerSource::IceFailed, Some(peer)));
        }
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

    /// Queue `events` for the listeners, and file what the §9 drive
    /// loop needs out of them.
    ///
    /// Every event the node produces passes through here, which is
    /// why this is where a verified signalling envelope is filed: an
    /// envelope only becomes a [`LeafEvent::Signal`] after
    /// `accept_signal` checked its signature against the sender's
    /// announcement, its `not_after`, and the replay set. The page
    /// still receives the event — that is how it learns an offer
    /// arrived — but the SDP it carries is applied by this module,
    /// never by the page.
    fn collect(&mut self, events: Vec<LeafEvent>) {
        for event in &events {
            if let LeafEvent::Signal(envelope) = event {
                self.file_signal(envelope);
            }
        }
        self.outbox
            .extend(events.iter().map(crate::node::LeafEvent::to_json));
    }

    /// File one verified envelope where the §9 drive loop will find
    /// it.
    ///
    /// An `Offer` is held per peer, because it is the one envelope
    /// that can arrive with no attempt to file it on — it is the
    /// invitation to start one, and the page decides whether to.
    /// Everything else belongs to a live attempt and to the dialog
    /// that attempt was numbered with: an envelope for a dialog this
    /// leaf is not driving is not silently adopted.
    fn file_signal(&mut self, envelope: &crate::control_plane::SignalEnvelope) {
        if envelope.kind == SignalKind::Offer {
            self.offers
                .insert(envelope.from, (envelope.dialog, envelope.payload.clone()));
            return;
        }
        let Some(dialog) = self.peers.get_mut(&envelope.from) else {
            return;
        };
        if envelope.dialog != dialog.dialog {
            return;
        }
        dialog
            .inbox
            .push_back((envelope.kind, envelope.payload.clone()));
    }

    /// Take the verified offer waiting from `peer`, if any.
    ///
    /// Taken, not read: one answer per offer. A page that calls
    /// `peer_accept_offer` twice for one offer is refused the second
    /// time rather than answering the same SDP on a second
    /// connection.
    fn take_pending_offer(&mut self, peer: NodeId) -> Option<(DialogId, Vec<u8>)> {
        self.offers.remove(&peer)
    }

    /// Retire the live attempt with `peer`, if there is one, because
    /// a new one is replacing it.
    ///
    /// The retired attempt counts `ice_failed`: it ended without
    /// installing and not at its own deadline, which is exactly that
    /// term. Counted rather than dropped, because an attempt that
    /// vanished from the ledger is what makes
    /// `direct + relayed + failed + udp_blocked == attempted` drift.
    fn supersede(&mut self, peer: NodeId) {
        if self.peers.contains_key(&peer) {
            self.settle(peer, IceTerm::Failed);
            self.peers.remove(&peer);
        }
    }

    /// Drain the transport's gathered candidates into the queue each
    /// one belongs to.
    ///
    /// The single drain point. See [`Inner::anchor_candidates`].
    fn harvest_candidates(&mut self) {
        for (peer, candidate) in self.transport.take_local_candidates() {
            if let Some(dialog) = self.peers.get_mut(&peer) {
                dialog.local.push_back(candidate);
            } else if peer == self.anchor {
                self.anchor_candidates.push_back(candidate);
            }
            // A candidate for a peer whose attempt is over belongs
            // to nobody, and trickling it to the anchor's dialog
            // would be the bootstrap attempt receiving another
            // pair's addresses.
        }
    }

    /// Count one terminal ICE term for an attempt, at most once.
    ///
    /// `peer == self.anchor` is the bootstrap attempt, which has no
    /// [`PeerDialog`] and is gated on its own flag. An attempt with
    /// neither counts nothing: a term without an attempt would break
    /// the partition
    /// `direct + relayed + failed + udp_blocked == attempted` that
    /// the conformance matrix asserts on every row.
    fn settle(&mut self, peer: NodeId, term: IceTerm) {
        if peer == self.anchor {
            if self.bootstrap_settled {
                return;
            }
            self.bootstrap_settled = true;
        } else {
            match self.peers.get_mut(&peer) {
                Some(dialog) if !dialog.settled => dialog.settled = true,
                _ => return,
            }
        }
        let counters = self.node.counters();
        match term {
            IceTerm::Direct => counters.ice_direct(),
            IceTerm::Relayed => counters.ice_relayed(),
            IceTerm::Failed => counters.ice_failed(),
            IceTerm::UdpBlocked => counters.udp_blocked(),
        }
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
            psk: credential.psk,
            ice_servers: ice_servers.clone(),
            offers: HashMap::new(),
            peers: HashMap::new(),
            anchor_candidates: VecDeque::new(),
            bootstrap_settled: false,
            inbox: VecDeque::new(),
            outbox: Vec::new(),
            dispatching: false,
            listeners: Vec::new(),
            closed: false,
            // One window for the episode and for the re-attempt it
            // starts: the same number a peer attempt's own ICE
            // deadline is, because "until this attempt is over" is
            // what an episode lasts.
            retry: crate::retry::RetryPolicy::new(PEER_ICE_DEADLINE_MS),
            retry_armed: false,
            retry_listeners: Vec::new(),
            retry_triggers: Rc::new(RefCell::new(VecDeque::new())),
            direct_offerer: std::collections::HashSet::new(),
            retry_last: None,
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
        {
            let mut guard = inner.borrow_mut();
            guard.dialog = accepted.dialog;
            // §10 `ice_attempted`, at the point the dialog becomes a
            // thing this leaf owns. Not at the API entry: an offer
            // that never got created is not an attempt, and the
            // denominator of `ice_direct / ice_attempted` has to
            // mean "direct-path attempts" for the ratio to be
            // readable.
            guard.node.counters().ice_attempted();
        }
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
            // The waits settle their own precise term (a deadline
            // with evidence, a deadline without it, a dialog the
            // anchor ended); this is the backstop for every other
            // way the block above can fail, and `settle` counts at
            // most once per attempt so it cannot double up.
            inner.borrow_mut().settle(anchor, IceTerm::Failed);
            abandon_attempt(&inner).await;
            attempt.disarm();
            return Err(failure);
        }
        inner.borrow_mut().settle(anchor, IceTerm::Direct);

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

    /// `RtcStats` on the leaf — plan §10's telemetry surface, in the
    /// field names the NATIVE `RtcStats` uses.
    ///
    /// Page-facing as `node.rtcStats()` through
    /// `@net-mesh/browser`. `u64`s are decimal strings, the rule
    /// [`Self::counters_json`] follows.
    ///
    /// Two halves, one reading: the transport's own ledger
    /// (admission, writes, the buffered-amount high water) and the
    /// ICE attempt ledger the conformance matrix asserts on. Plus
    /// `not_applicable`, which names the 24 native fields a leaf has
    /// no meaning for **and says why** — see
    /// [`crate::counters::LeafCounters::rtc_stats_json`] for the
    /// reason that is a list of reasons rather than a wall of
    /// zeros.
    pub fn rtc_stats_json(&self) -> String {
        let guard = self.inner.borrow();
        let link = guard.transport.link_snapshot();
        guard.node.counters().rtc_stats_json(&link)
    }

    /// Arm the network-change re-attempt trigger.
    ///
    /// Installs the window's `online` listener; the ICE
    /// `disconnected` → `failed` watcher is already on every peer
    /// connection this transport owns. From here, either
    /// observation files a trigger and
    /// [`crate::retry::RetryPolicy`] decides — once per network
    /// change, per peer.
    ///
    /// # Why a page has to ask
    ///
    /// Re-dialling is policy, and policy is the application's. A
    /// library that re-offered on its own would take that decision
    /// away from a page that has a reason not to: one that is
    /// deliberately holding a pair on the routed path, one tearing
    /// down, one whose user is on a metered link. §9 step 6 makes
    /// the routed session a supported disposition rather than a
    /// degraded one, so "the pair is relayed" is not a fault this
    /// leaf may assume it should fix.
    ///
    /// **Idempotent.** A second call adds no second listener,
    /// because two listeners for one `online` event are two triggers
    /// for one network change — and while the owner would coalesce
    /// them, a page that armed twice should not be relying on that.
    ///
    /// Only pairs this leaf took direct **as the offerer** are
    /// re-attempted: §9 step 4 gives the offerer the initiating
    /// role, and both sides re-offering one network change is two
    /// attempts per pair plus a supersession race.
    pub fn arm_network_retry(&self) -> Result<(), JsError> {
        let mut guard = self.inner.borrow_mut();
        guard.admit().map_err(js)?;
        if guard.retry_armed {
            return Ok(());
        }
        let window = web_sys::window()
            .ok_or_else(|| JsError::new("no window: the network-change trigger needs one"))?;
        let triggers = Rc::clone(&guard.retry_triggers);
        // Filed, never acted on here. This runs inside a JS event
        // callback and the owner lives behind the `RefCell` the pump
        // may hold; the ticker drains.
        let online = Closure::wrap(Box::new(move |_event: JsValue| {
            triggers
                .borrow_mut()
                .push_back((crate::retry::TriggerSource::Online, None));
        }) as Box<dyn FnMut(JsValue)>);
        window
            .add_event_listener_with_callback("online", online.as_ref().unchecked_ref())
            .map_err(|e| JsError::new(&format!("online listener: {e:?}")))?;
        guard.retry_listeners.push(online);
        guard.retry_armed = true;
        Ok(())
    }

    /// The re-attempt owner's ledger, as JSON.
    ///
    /// `{"armed":<bool>,"online":"<n>","iceFailed":"<n>",
    /// "triggers":"<n>","started":"<n>","coalesced":"<n>",
    /// "notEligible":"<n>","openEpisodes":<n>,"owned":<n>,
    /// "last":"<disposition>"|null}`.
    ///
    /// `started` is the number the witness asserts: one network
    /// change, one re-attempt, however many observations of it
    /// arrived. `triggers` is how many arrived, and `coalesced` is
    /// the difference the owner absorbed — reported rather than
    /// hidden, because "the trigger never fired" and "it fired and
    /// was absorbed" are different facts and only one of them is a
    /// defect.
    ///
    /// `owned` is how many pairs this leaf took direct as the
    /// offerer, i.e. the set a network change is evaluated against.
    pub fn retry_report(&self) -> String {
        let guard = self.inner.borrow();
        let ledger = guard.retry.ledger();
        let now = clock::now();
        let last = guard
            .retry_last
            .map_or_else(|| "null".to_string(), json_string);
        format!(
            "{{\"armed\":{},\"online\":\"{}\",\"iceFailed\":\"{}\",\"triggers\":\"{}\",\
             \"started\":\"{}\",\"coalesced\":\"{}\",\"notEligible\":\"{}\",\
             \"openEpisodes\":{},\"owned\":{},\"last\":{last}}}",
            guard.retry_armed,
            ledger.online,
            ledger.ice_failed,
            ledger.triggers(),
            ledger.started,
            ledger.coalesced,
            ledger.not_eligible,
            guard.retry.open_episodes(now),
            guard.direct_offerer.len(),
        )
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
    /// reliable?: boolean, label?, streamId?, channelHash?, peer? }`.
    /// `streamId` (a decimal or `0x`-hex **string**, because a `u64`
    /// never crosses as a JS number) and `channelHash` (a **number**
    /// in `0..=65535`) are used verbatim when present, so a stream
    /// can match a publish contract a native handler dispatches on.
    ///
    /// `peer` is **16 hex digits** — the spelling `node_id_hex()`
    /// hands out and the four §9 peer methods take — and names the
    /// node the stream addresses; absent, it is the anchor, which is
    /// what every caller before this option got and still gets. It
    /// is the page-facing half of §9: `connectPeer` installs a
    /// direct leaf ↔ leaf session and this is what puts application
    /// bytes on it. Nothing below it changes —
    /// `crate::node::LeafNode::open_stream` has always taken a peer
    /// and `route_outbound` has always decided routed vs direct, so
    /// a stream to a peer whose session is still ROUTED rides the
    /// anchor and needs no second call to start riding the
    /// DataChannel.
    ///
    /// **But the HANDLE does not survive the upgrade.** §9 step 4
    /// installs a *replacement* session, so the peer's incarnation
    /// changes, and a `StreamHandle` is fenced to the incarnation it
    /// was opened on
    /// (`LeafNode::check_handle` — the alias the
    /// native R12 fix closed): a handle opened while the pair was
    /// routed refuses with "stale stream handle … reopen the
    /// stream" once the direct session lands. The addressing is
    /// unchanged — same peer, same `streamId` if one was pinned —
    /// so reopening is one call and the far end sees the same
    /// stream; what is not true is that the old handle keeps
    /// working. A page driving a peer across the upgrade should
    /// reopen on that refusal.
    ///
    /// A peer with no session at all refuses typed from `send`,
    /// unchanged.
    ///
    /// Both are read by `stream_options`, which
    /// [`crate::leader_session::MeshSession::open_stream`] also
    /// calls: the direct and the proxied surface cannot read the
    /// same option object two ways. That surface **refuses** `peer`
    /// rather than dropping it — see
    /// `StreamOptions::require_anchor_addressed`.
    pub fn open_stream(&self, opts: JsValue) -> Result<LeafStream, JsError> {
        let options = stream_options(&opts)?;

        let mut guard = self.inner.borrow_mut();
        guard.admit().map_err(js)?;
        let peer = options.peer.unwrap_or(guard.anchor);
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
    /// "streamId":"0000000000000009","channelHash":7,
    /// "peer":"00366d403ce19dac"}`, with `streamId` `null` when the
    /// caller did not pin one (the node allocates), `channelHash`
    /// `null` when absent, and `peer` `null` when the stream
    /// addresses the anchor.
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
        // **One spelling, and it is the one this boundary hands
        // out.** This read `parse_u64`, which is DECIMAL for a bare
        // string — so `node_id_hex()`'s own output was refused here
        // while the four §9 methods accept it, and a page that got
        // an id from one surface could not use it on the other. The
        // leader-proxied twin
        // ([`crate::leader_session::MeshSession::signal`]) read bare
        // hex of any length, so `"9"` and `"deadbeef"` named nodes
        // there that nothing else would accept. All three now parse
        // the same 16 hex digits (PeerLoop's audit, 2026-09-16).
        let peer = parse_peer_id(&peer_hex)?;
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

    // ───────────── browser ↔ browser, from a page (§9) ─────────────
    //
    // Four methods, and between them the page never touches an SDP
    // blob, a candidate line or a key. It names a peer; this module
    // owns everything else. That is the security property, not a
    // convenience: a surface that accepted a Noise key from the page
    // would accept ANY key, so the peer's static key comes from its
    // signature-verified announcement and from nowhere else, and the
    // domain PSK comes from the credential the page already
    // presented to `connect`.
    //
    // The carrier is plan §9 step 3: the signed `0x0D02` envelope
    // rides the A↔B session and the anchor forwards it **blind**.
    // `ControlPlane::signal` is not used and `AnchorControlPlane`
    // still refuses it (R14): a control plane carries what a leaf
    // needs *instead of* a session, and here there is one.

    /// Offer a direct connection to `peer` — §9 steps 2 and 3.
    ///
    /// Resolves to the dialog id, 16 hex digits, which is what the
    /// page correlates every later call on this attempt against.
    ///
    /// Three things happen, in this order:
    ///
    /// 1. **Discovery is required.** With no verified announcement
    ///    for `peer` this refuses before anything is allocated:
    ///    there is no key to handshake against and no authority to
    ///    verify the peer's envelopes with, and guessing either is
    ///    the failure mode this whole surface exists to prevent.
    /// 2. **The relayed session comes up** if there is not one
    ///    already (§9 step 2): NKpsk0 over the anchor's blind
    ///    forwarding, keys from the announcement. Failing here
    ///    counts nothing — no offer was created, so there is no
    ///    attempt to charge.
    /// 3. **The offer goes out** as a signed `Offer` envelope inside
    ///    that session, and `ice_attempted` moves.
    ///
    /// The deadline the attempt runs under is
    /// [`PEER_ICE_DEADLINE_MS`] from now. The re-attempt owner calls
    /// `offer_peer` directly with the episode's deadline
    /// instead, which is how a repair and the window that absorbs
    /// its duplicate triggers end up being one absolute instant
    /// rather than two that nearly agree.
    pub async fn peer_offer(&self, peer_hex: String) -> Result<String, JsError> {
        let peer = parse_peer_id(&peer_hex)?;
        let dialog = self
            .offer_peer(peer, crate::clock::Deadline::in_ms(PEER_ICE_DEADLINE_MS))
            .await?;
        Ok(format!("{dialog:016x}"))
    }

    /// [`Self::peer_offer`]'s whole body, with the attempt's
    /// absolute deadline as a parameter.
    ///
    /// Not page-facing: a page names a peer and gets the one
    /// deadline the leaf publishes. The parameter exists so the
    /// network-change re-attempt owner can drive **this** function —
    /// the production offer path, with its supersession, its
    /// counting and its ordering — under the deadline its episode
    /// already committed to, rather than a second offer path that
    /// agreed with this one on the day it was written.
    async fn offer_peer(
        &self,
        peer: NodeId,
        deadline: crate::clock::Deadline,
    ) -> Result<DialogId, JsError> {
        let (anchor, noise, psk, ice_servers) = {
            let guard = self.inner.borrow();
            guard.admit().map_err(js)?;
            if peer == guard.anchor || peer == guard.node.node_id() {
                return Err(JsError::new(
                    "peer_offer needs a peer: the anchor is `connect`'s and a leaf is not its own peer",
                ));
            }
            let noise = peer_noise_key(&guard, peer).map_err(js)?;
            (guard.anchor, noise, guard.psk, guard.ice_servers.clone())
        };

        // §9 step 2.
        self.ensure_relayed_session(peer, anchor, &psk, &noise)
            .await?;

        // One dialog id per attempt, from the platform CSPRNG: both
        // sides number the exchange with it, the answerer reads it
        // off the signed envelope, and a guessable one would let a
        // third party's envelope land on somebody's live attempt.
        let dialog = u64::from_le_bytes(
            random32().map_err(js)?[..8]
                .try_into()
                .map_err(|_| JsError::new("dialog"))?,
        );
        // **The dialog is registered BEFORE the offer is created**,
        // and the ordering is load-bearing: `create_offer` sets the
        // local description, which is what makes the browser start
        // gathering, and a candidate that arrives before this peer
        // has a dialog has nowhere to be filed
        // ([`Inner::harvest_candidates`] drops it, correctly — a
        // candidate for an attempt that does not exist belongs to
        // nobody). The first host candidates appear within a
        // millisecond or two of `setLocalDescription`, and the
        // periodic tick harvests every 50 ms, so registering
        // afterwards threw away exactly the candidates ICE needs
        // most and left both sides gathering until their deadlines.
        with_node(&self.inner, |guard| {
            // A second offer to the same peer retires the first: one
            // live attempt per peer, and the one it replaces is
            // counted rather than forgotten.
            guard.supersede(peer);
            guard.peers.insert(
                peer,
                PeerDialog {
                    dialog,
                    role: PeerRole::Offerer,
                    inbox: VecDeque::new(),
                    local: VecDeque::new(),
                    deadline,
                    settled: false,
                },
            );
            guard.node.counters().ice_attempted();
        });

        let transport = self.inner.borrow().transport.clone();
        let offer = match transport.create_offer(peer, &ice_servers).await {
            Ok(offer) => offer,
            Err(e) => {
                // The attempt is registered and counted, so it needs
                // a terminal term: the channel never opened and this
                // is not its deadline.
                with_node(&self.inner, |guard| {
                    guard.settle(peer, IceTerm::Failed);
                });
                return Err(js(e));
            }
        };

        let sent = with_node(&self.inner, |guard| {
            let envelope =
                guard
                    .node
                    .sign_signal(peer, dialog, SignalKind::Offer, offer.0.into_bytes());
            guard.node.send_signal_frame(peer, &envelope)?;
            guard.pump();
            Ok::<_, LeafError>(())
        });
        if let Err(e) = sent {
            with_node(&self.inner, |guard| {
                guard.settle(peer, IceTerm::Failed);
            });
            return Err(js(e));
        }
        Ok(dialog)
    }

    /// Answer the offer `peer` sent — the other half of §9 step 3.
    ///
    /// Resolves to the dialog id the offerer minted, which the page
    /// uses exactly as `peer_offer`'s caller does.
    ///
    /// The offer applied here is the one that arrived as a
    /// **verified** envelope: signature checked against the sender's
    /// signed announcement, inside its `not_after`, and unreplayed.
    /// The page passes no SDP, which is why an offer it never
    /// received cannot be answered and an offer it did receive
    /// cannot be altered.
    ///
    /// A second offer from the same peer supersedes the first: the
    /// old dialog is counted `ice_failed` and its id stops being the
    /// live one, which is what the page's next call sees.
    pub async fn peer_accept_offer(&self, peer_hex: String) -> Result<String, JsError> {
        let peer = parse_peer_id(&peer_hex)?;
        let (offer, dialog, ice_servers) = with_node(&self.inner, |guard| {
            guard.admit()?;
            // The announcement is what verified the envelope, and it
            // is what this leaf will need again for every candidate
            // the peer trickles.
            let _ = peer_noise_key(guard, peer)?;
            let (dialog, offer) = guard.take_pending_offer(peer).ok_or_else(|| {
                LeafError::Session(format!(
                    "no verified offer from {peer:#x} is waiting: an offer is answered from \
                     the envelope that arrived, never from the caller"
                ))
            })?;
            let offer = String::from_utf8(offer).map_err(|_| {
                LeafError::ControlPlane(format!("the offer {peer:#x} signed is not UTF-8 SDP"))
            })?;
            Ok::<_, LeafError>((offer, dialog, guard.ice_servers.clone()))
        })
        .map_err(js)?;

        // `accept_offer` below REPLACES this peer's DataChannel, so
        // the answer — which has to reach the peer for that channel
        // ever to open — must go through the relay. Set before the
        // replacement, not after: the session is the same session,
        // only its addressing changes (§9 step 4, read backwards).
        let anchor = with_node(&self.inner, |guard| {
            let anchor = guard.anchor;
            guard.node.set_peer_relay(peer, anchor);
            anchor
        });
        debug_assert_ne!(anchor, peer);

        // Registered before `accept_offer`, for the reason
        // [`Self::peer_offer`] spells out: `accept_offer` sets both
        // descriptions and the browser starts gathering, and a
        // candidate that arrives before this peer has a dialog is
        // filed nowhere.
        with_node(&self.inner, |guard| {
            guard.supersede(peer);
            guard.peers.insert(
                peer,
                PeerDialog {
                    dialog,
                    role: PeerRole::Answerer,
                    inbox: VecDeque::new(),
                    local: VecDeque::new(),
                    deadline: crate::clock::Deadline::in_ms(PEER_ICE_DEADLINE_MS),
                    settled: false,
                },
            );
            guard.node.counters().ice_attempted();
        });

        let transport = self.inner.borrow().transport.clone();
        let answer = match transport
            .accept_offer(peer, &Sdp(offer), &ice_servers)
            .await
        {
            Ok(answer) => answer,
            Err(e) => {
                with_node(&self.inner, |guard| {
                    guard.settle(peer, IceTerm::Failed);
                });
                return Err(js(e));
            }
        };

        let sent = with_node(&self.inner, |guard| {
            let envelope =
                guard
                    .node
                    .sign_signal(peer, dialog, SignalKind::Answer, answer.0.into_bytes());
            guard.node.send_signal_frame(peer, &envelope)?;
            guard.pump();
            Ok::<_, LeafError>(())
        });
        if let Err(e) = sent {
            with_node(&self.inner, |guard| {
                guard.settle(peer, IceTerm::Failed);
            });
            return Err(js(e));
        }
        Ok(format!("{dialog:016x}"))
    }

    /// Service `peer`'s attempt once, and say where it stands.
    ///
    /// This is the trickle pump the page's drive loop calls. It does
    /// four things, all of them for one attempt:
    ///
    /// - applies the peer's `Answer` if one has arrived (an offerer
    ///   only: an answerer already has the offer's description);
    /// - sends every candidate this browser has gathered for `peer`
    ///   as a signed `Candidate` envelope;
    /// - applies every candidate the peer trickled;
    /// - evaluates the attempt's ICE deadline, and settles
    ///   `udp_blocked` or `ice_relayed` when it has passed.
    ///
    /// Resolves to JSON: `{"dialog":"<16 hex>","state":"gathering"
    /// |"open"|"iceTimeout"|"udpBlocked","sent":<n>,"applied":<n>,
    /// "answered":<bool>,"remainingMs":"<decimal>"}`. `dialog` is
    /// the attempt that is CURRENTLY live for this peer, so a page
    /// whose dialog id no longer matches has been superseded — the
    /// one disposition this call cannot report for itself, because
    /// the attempt it would report about is gone.
    ///
    /// A candidate the browser rejects (a stale pair, a duplicate)
    /// is not an error and is not counted in `applied`; the channel
    /// opening is what matters.
    pub async fn peer_candidate(&self, peer_hex: String) -> Result<String, JsError> {
        let peer = parse_peer_id(&peer_hex)?;
        let r = self.service_peer(peer).await?;
        Ok(format!(
            "{{\"dialog\":\"{:016x}\",\"state\":\"{}\",\"sent\":{},\
             \"applied\":{},\"answered\":{},\"direct\":{},\
             \"remainingMs\":\"{}\"}}",
            r.dialog, r.state, r.sent, r.applied, r.answered, r.direct, r.remaining_ms
        ))
    }

    /// [`Self::peer_candidate`]'s whole body, before the JSON.
    ///
    /// Split out for the reason `offer_peer` is: the
    /// re-attempt owner drives the attempt through the SAME service
    /// step a page's drive loop calls, and a private reading is a
    /// struct rather than a string it would have to re-parse.
    async fn service_peer(&self, peer: NodeId) -> Result<AttemptReading, JsError> {
        let (dialog, role, outgoing, incoming, transport) = with_node(&self.inner, |guard| {
            guard.admit()?;
            guard.harvest_candidates();
            let Some(dialog) = guard.peers.get_mut(&peer) else {
                return Err(LeafError::Session(format!(
                    "{NO_LIVE_ATTEMPT_PREFIX} {peer:#x}"
                )));
            };
            let id = dialog.dialog;
            let role = dialog.role;
            let outgoing: Vec<IceCandidate> = dialog.local.drain(..).collect();
            let incoming: Vec<(SignalKind, Vec<u8>)> = dialog.inbox.drain(..).collect();
            Ok::<_, LeafError>((id, role, outgoing, incoming, guard.transport.clone()))
        })
        .map_err(js)?;

        let mut sent = 0usize;
        for candidate in &outgoing {
            with_node(&self.inner, |guard| {
                let envelope = guard.node.sign_signal(
                    peer,
                    dialog,
                    SignalKind::Candidate,
                    candidate_payload(candidate).into_bytes(),
                );
                guard.node.send_signal_frame(peer, &envelope)?;
                guard.pump();
                Ok::<_, LeafError>(())
            })
            .map_err(js)?;
            sent += 1;
        }

        let mut applied = 0usize;
        let mut answered = false;
        for (kind, payload) in incoming {
            match kind {
                SignalKind::Answer if role == PeerRole::Offerer => {
                    let sdp = String::from_utf8(payload)
                        .map_err(|_| JsError::new("an answer payload is not UTF-8 SDP"))?;
                    transport.accept_answer(peer, &Sdp(sdp)).await.map_err(js)?;
                    answered = true;
                }
                SignalKind::Candidate => {
                    let Some(candidate) = parse_candidate(&payload) else {
                        continue;
                    };
                    if transport
                        .add_remote_candidate(peer, &candidate)
                        .await
                        .is_ok()
                    {
                        applied += 1;
                    }
                }
                SignalKind::Reject => {
                    let reason = String::from_utf8_lossy(&payload).to_string();
                    with_node(&self.inner, |guard| {
                        guard.settle(peer, IceTerm::Failed);
                        guard.peers.remove(&peer);
                    });
                    return Err(js(LeafError::ControlPlane(format!(
                        "{peer:#x} rejected the attempt: {reason}"
                    ))));
                }
                // An offer on a dialog this leaf already owns, or an
                // answer to an attempt this leaf did not offer.
                // Neither is actionable and neither is invented into
                // one.
                SignalKind::Offer | SignalKind::Answer => {}
            }
        }

        let open = transport.is_open(peer);
        // The deadline is evaluated AFTER openness, the same
        // ordering `wait_for_channel` uses and for the same reason:
        // a channel that came up is not a timeout, whatever the
        // clock says about the tick it came up on.
        let state = if open {
            "open"
        } else if self.peer_deadline_passed(peer) {
            self.settle_peer_deadline(peer).await
        } else {
            "gathering"
        };
        let remaining = self.peer_remaining_ms(peer);
        // The direct session, which is a different fact from an open
        // channel: the answerer's half of §9 step 4 installs from an
        // inbound packet, so this is how its drive loop learns the
        // pair is off the relay without calling a handshake it does
        // not own.
        let direct = {
            let guard = self.inner.borrow();
            guard.node.has_session(peer) && guard.node.peer_relay(peer).is_none()
        };
        Ok(AttemptReading {
            dialog,
            state,
            sent,
            applied,
            answered,
            direct,
            remaining_ms: remaining,
        })
    }

    /// Run the Noise handshake with `peer` over the direct
    /// DataChannel — §9 step 4.
    ///
    /// **Takes a peer id and nothing else.** A page that could
    /// supply a Noise key could supply *any* key, so the responder
    /// static this handshake authenticates comes from `peer`'s
    /// signature-verified announcement and the PSK from the
    /// credential `connect` was given. There is no parameter through
    /// which either can be substituted, which is the point.
    ///
    /// The offerer's role, per §9 step 4: the leaf that created the
    /// offer is the NKpsk0 initiator. The answerer never calls this
    /// — its half runs from the inbound packet, in
    /// `Inner::on_handshake`.
    ///
    /// On success the direct session is installed on this side and
    /// the relay entry for `peer` is gone, so the pair's traffic
    /// leaves on its own channel. The session replacement itself is
    /// the node's — `LeafNode::install_session`, the
    /// Stage 3/4a fence, unchanged and unreached from here.
    pub async fn peer_handshake(&self, peer_hex: String) -> Result<(), JsError> {
        let peer = parse_peer_id(&peer_hex)?;
        self.run_handshake(peer).await
    }

    /// [`Self::peer_handshake`]'s whole body. Split out so the
    /// re-attempt owner runs the production step and not a copy of
    /// it.
    async fn run_handshake(&self, peer: NodeId) -> Result<(), JsError> {
        let msg1 = with_node(&self.inner, |guard| {
            guard.admit()?;
            let dialog = guard
                .peers
                .get(&peer)
                .ok_or_else(|| LeafError::Session(format!("{NO_LIVE_ATTEMPT_PREFIX} {peer:#x}")))?;
            if dialog.role != PeerRole::Offerer {
                return Err(LeafError::Session(format!(
                    "this leaf answered {peer:#x}'s offer, so the handshake is the offerer's \
                     (§9 step 4); the responder half runs from the inbound packet"
                )));
            }
            if !guard.transport.is_open(peer) {
                return Err(LeafError::Rtc(crate::error::RtcError::ChannelClosed(
                    format!("the DataChannel to {peer:#x} is not open"),
                )));
            }
            let noise = peer_noise_key(guard, peer)?;
            let psk = guard.psk;
            let slot = guard.transport.next_slot();
            let packet = guard.node.begin_handshake(peer, &psk, &noise, slot)?;
            // Direct, deliberately: the channel is open, so message
            // 1 does not go through the relay even though the
            // session it replaces still has a relay entry. Sending
            // it relayed would install the direct session over the
            // anchor, which is the thing this step exists to stop
            // doing.
            guard.transport.send(peer, packet.clone())?;
            Ok::<_, LeafError>(packet)
        })
        .map_err(js)?;
        debug_assert!(!msg1.is_empty());

        let installed = self.wait_for_direct_session(peer).await;
        if !installed {
            with_node(&self.inner, |guard| {
                guard.settle(peer, IceTerm::Failed);
            });
            return Err(js(LeafError::Session(format!(
                "{peer:#x} did not complete the Noise handshake over the direct channel \
                 inside the deadline"
            ))));
        }
        Ok(())
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

/// One service of a live attempt, as [`LeafNode::service_peer`]
/// reads it.
///
/// `state` is about ICE and `direct` is about the SESSION. Two
/// different facts, and a reader that conflates them would call a
/// pair direct because a channel opened.
struct AttemptReading {
    dialog: DialogId,
    state: &'static str,
    sent: usize,
    applied: usize,
    answered: bool,
    direct: bool,
    remaining_ms: u64,
}

/// The network-change re-attempt owner's execution half.
impl LeafNode {
    /// Run ONE re-attempt for `peer`, bounded by `deadline`.
    ///
    /// **Every step here is the production step**, not a copy of
    /// one: `offer_peer` is `peer_offer`'s body (so the
    /// re-attempt supersedes the stale dialog, counts
    /// `ice_attempted` once, and puts the routed session back before
    /// it replaces the channel), [`Self::service_peer`] is
    /// `peer_candidate`'s, and [`Self::run_handshake`] is
    /// `peer_handshake`'s. What this function adds is the loop
    /// between them and nothing else.
    ///
    /// **Retire before install**, Stage 4a's rule: the retirement is
    /// `offer_peer`'s own `supersede`, which counts the dialog it
    /// replaces rather than dropping it, and it happens before the
    /// new dialog is registered — so the §10 partition holds across
    /// a repair.
    ///
    /// **One absolute deadline**: `deadline` is the episode's, it is
    /// what the new dialog is given, and it is therefore what the
    /// channel wait, the settle and the Noise wait all read. There
    /// is no second clock here.
    async fn re_attempt(&self, peer: NodeId, deadline: crate::clock::Deadline) {
        let outcome = self.re_attempt_once(peer, deadline).await;
        with_node(&self.inner, |guard| {
            guard.retry_last = Some(outcome);
        });
    }

    /// The re-attempt, reporting where it ended.
    ///
    /// The dispositions are the typed outcome union's names, because
    /// a repair ends the same four ways an attempt does and a fifth
    /// vocabulary would be a second taxonomy for one fact.
    async fn re_attempt_once(
        &self,
        peer: NodeId,
        deadline: crate::clock::Deadline,
    ) -> &'static str {
        if self.offer_peer(peer, deadline).await.is_err() {
            // The routed session would not come up, the peer is no
            // longer discoverable, or the offer could not be signed
            // and sent. `offer_peer` counted a term for whatever it
            // allocated.
            return "offerRefused";
        }
        loop {
            let reading = match self.service_peer(peer).await {
                Ok(reading) => reading,
                // The peer rejected, or the attempt is gone —
                // superseded by a page that started its own while
                // this ran, which is the page's to own and not
                // this owner's to fight over.
                Err(_) => return "superseded",
            };
            match reading.state {
                "open" => break,
                "iceTimeout" => return "iceTimeout",
                "udpBlocked" => return "udpBlocked",
                _ => {}
            }
            if reading.remaining_ms == 0 {
                // The episode's own deadline, read through the
                // dialog it created. Not a second bound.
                return "iceTimeout";
            }
            gloo_timer_sleep(TICK_MS).await.ok();
        }
        if self.run_handshake(peer).await.is_err() {
            return "handshakeFailed";
        }
        "direct"
    }
}

/// The §9 drive loop's internals.
///
/// A plain `impl`, deliberately: nothing here is page-facing, and
/// four methods is the whole surface the brief specifies. Putting
/// them in the `#[wasm_bindgen]` block would leave the boundary one
/// careless `pub` away from exporting a step that takes SDP.
impl LeafNode {
    /// Bring up the relayed session with `peer` if there is not one
    /// — plan §9 step 2.
    ///
    /// NKpsk0 over the anchor's blind forwarding: message 1 goes out
    /// wrapped in a routing envelope, the anchor forwards it without
    /// decrypting it (it cannot — the inner packet is not sealed to
    /// the anchor), the peer answers the same way, and both sides
    /// install a session that the anchor carries but cannot read.
    ///
    /// The responder static comes from `peer`'s signed announcement
    /// and the PSK from the credential. Nothing here is negotiable
    /// by the page.
    async fn ensure_relayed_session(
        &self,
        peer: NodeId,
        anchor: NodeId,
        psk: &[u8; 32],
        noise: &[u8; 32],
    ) -> Result<(), JsError> {
        if self.inner.borrow().node.has_session(peer) {
            // A session already exists — a pair that was already
            // direct, or already relayed. Either way the attempt
            // about to start REPLACES this peer's DataChannel
            // ([`RtcLeafTransport::create_offer`] and `accept_offer`
            // both do), so from here until the new channel is open
            // the only way to reach the peer is the relay. Putting it
            // back is not a downgrade of the session: the session is
            // untouched and keyed on identity, and this is purely its
            // addressing (§9 step 4, read backwards). Skipping it is
            // how a re-offer ends up signalling an offer down the
            // very channel it just destroyed.
            with_node(&self.inner, |guard| {
                guard.node.set_peer_relay(peer, anchor);
            });
            return Ok(());
        }
        with_node(&self.inner, |guard| {
            // Addressing first: `route_outbound` is what turns the
            // handshake packet into something a peer with no channel
            // can be reached at.
            guard.node.set_peer_relay(peer, anchor);
            let slot = guard.transport.next_slot();
            let msg1 = guard.node.begin_handshake(peer, psk, noise, slot)?;
            let out = guard.node.route_outbound(peer, msg1);
            guard.transport.send(out.peer, out.packet)?;
            guard.pump();
            Ok::<_, LeafError>(())
        })
        .map_err(js)?;

        let mut waited = 0;
        while waited < RELAY_SESSION_DEADLINE_MS {
            gloo_timer_sleep(TICK_MS).await.ok();
            waited += TICK_MS;
            if with_node(&self.inner, |guard| {
                guard.pump();
                guard.node.has_session(peer)
            }) {
                return Ok(());
            }
        }
        // Nothing was allocated on the peer's behalf that outlives
        // this: no dialog, no ICE agent, no attempt. The relay entry
        // goes, so a later attempt starts clean.
        with_node(&self.inner, |guard| {
            guard.node.clear_peer_relay(peer);
        });
        Err(js(LeafError::Session(format!(
            "{peer:#x} did not complete the relayed Noise handshake inside \
             {RELAY_SESSION_DEADLINE_MS} ms: it is discoverable but not reachable through \
             the anchor"
        ))))
    }

    /// Has `peer`'s attempt passed its ICE deadline?
    fn peer_deadline_passed(&self, peer: NodeId) -> bool {
        let now = clock::now();
        self.inner
            .borrow()
            .peers
            .get(&peer)
            .is_some_and(|dialog| dialog.deadline.expired_at(now))
    }

    /// Milliseconds left on `peer`'s attempt; `0` when there is no
    /// attempt or it has expired.
    fn peer_remaining_ms(&self, peer: NodeId) -> u64 {
        let now = clock::now();
        self.inner
            .borrow()
            .peers
            .get(&peer)
            .map_or(0, |dialog| dialog.deadline.remaining_ms_at(now))
    }

    /// Settle a peer attempt whose deadline has passed, and say
    /// which term it settled as.
    ///
    /// The STUN probe runs here and only here: `udp_blocked` may be
    /// claimed **only** where
    /// [`crate::error::UdpBlockedEvidence`] is actually established,
    /// and the `None` arm of that evidence is an ICE timeout, which
    /// counts `ice_relayed`. The probe's target is the address the
    /// anchor published — the only address this leaf is entitled to
    /// aim one at — and the observation it establishes is about this
    /// browser's UDP, not about the peer, which is why a peer
    /// attempt reads it from the same place the bootstrap one does.
    async fn settle_peer_deadline(&self, peer: NodeId) -> &'static str {
        let rtc_addr = self.inner.borrow().control.anchor_rtc_addr();
        let probe_failed = match &rtc_addr {
            Some(addr) => stun_probe_failed(addr).await,
            None => false,
        };
        let failure = classify_ice_failure(true, probe_failed, rtc_addr.as_deref());
        let blocked = matches!(failure, crate::error::RtcError::UdpBlocked { .. });
        with_node(&self.inner, |guard| {
            guard.settle(
                peer,
                if blocked {
                    IceTerm::UdpBlocked
                } else {
                    IceTerm::Relayed
                },
            );
        });
        if blocked {
            "udpBlocked"
        } else {
            "iceTimeout"
        }
    }

    /// Poll until the direct session with `peer` is installed, or
    /// the attempt's deadline passes.
    ///
    /// Bounded by the attempt's own deadline rather than a fresh
    /// one: the page has been waiting since `peer_offer`, and a
    /// second full deadline here would let one `connectPeer` outlive
    /// two.
    async fn wait_for_direct_session(&self, peer: NodeId) -> bool {
        loop {
            let installed = with_node(&self.inner, |guard| {
                guard.pump();
                guard.node.has_session(peer) && guard.node.peer_relay(peer).is_none()
            });
            if installed {
                return true;
            }
            if self.peer_remaining_ms(peer) == 0 {
                return false;
            }
            gloo_timer_sleep(TICK_MS).await.ok();
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

/// The peer's Noise static public key, from its **signed
/// announcement** and from nowhere else.
///
/// §5 Layer 1: key discovery precedes signalling. This is the one
/// datum first contact genuinely requires, and the announcement is
/// the only thing that can supply it honestly — the carrier holds no
/// keys and could not be believed if it offered one, and a page that
/// could pass a key could pass any key. `NoAnnouncement` is
/// therefore a typed, page-visible outcome rather than an internal
/// error: "I have not discovered that peer yet" is a state a drive
/// loop acts on.
fn peer_noise_key(inner: &Inner, peer: NodeId) -> Result<[u8; 32], LeafError> {
    let announcement = inner.node.announcement_for(peer).ok_or_else(|| {
        LeafError::Session(format!(
            "{NO_ANNOUNCEMENT_PREFIX} {peer:#x}: discover the peer by capability query \
             first — a peer's Noise key comes from its signed announcement, never from a caller"
        ))
    })?;
    announcement.noise_pubkey.ok_or_else(|| {
        LeafError::Session(format!(
            "{peer:#x}'s announcement carries no noise_pubkey, so it cannot be handshaked with"
        ))
    })
}

/// One candidate as the envelope payload both halves agree on.
///
/// The same two fields the anchorless witness carries, because it is
/// the same exchange: the candidate line verbatim and the media id
/// it belongs to.
fn candidate_payload(candidate: &IceCandidate) -> String {
    format!(
        "{{\"candidate\":{},\"mid\":{}}}",
        json_string(&candidate.candidate),
        json_string(&candidate.mid)
    )
}

/// Read a candidate payload back, or `None` if it is not one.
///
/// A malformed payload is skipped rather than failing the attempt: it
/// arrived inside a *verified* envelope, so the sender is who it says
/// it is, and one unusable candidate line is a liveness detail — the
/// channel opening is the assertion.
fn parse_candidate(payload: &[u8]) -> Option<IceCandidate> {
    let document: serde_json::Value = serde_json::from_slice(payload).ok()?;
    Some(IceCandidate {
        candidate: document["candidate"].as_str()?.to_string(),
        mid: document["mid"].as_str().unwrap_or("0").to_string(),
    })
}

/// Pump the control plane, both directions.
///
/// Out: the candidates the browser gathered **for the bootstrap
/// dialog**. In: the peer's candidates, the announcements a control
/// plane delivered, and the signalling envelopes — which the
/// **leaf** verifies, never the transport. One function, called from
/// the connect wait and from the ticker, so the two cannot drift
/// apart.
///
/// A browser ↔ browser attempt's candidates do **not** go out here:
/// they are signed `Candidate` envelopes on the A↔B session (§9 step
/// 3), sent by `peer_candidate`. Both used to drain the transport
/// directly, which made them race for each other's candidates;
/// [`Inner::harvest_candidates`] is now the single drain and sorts
/// them, so the bootstrap dialog can never be trickled another
/// pair's addresses.
async fn service_control_plane(inner: &Rc<RefCell<Inner>>) -> Result<(), LeafError> {
    let (control, transport, dialog, anchor, outgoing) = {
        let mut guard = inner.borrow_mut();
        guard.harvest_candidates();
        let outgoing: Vec<IceCandidate> = guard.anchor_candidates.drain(..).collect();
        (
            guard.control.clone(),
            guard.transport.clone(),
            guard.dialog,
            guard.anchor,
            outgoing,
        )
    };
    for candidate in outgoing {
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
            // And the re-attempt owner, for the same reason: it
            // spawns, and what it spawns takes the node.
            drive_retries(&inner);
        }
    });
}

/// Drain the trigger queue, decide once per peer, and start at most
/// one re-attempt each.
///
/// **This is the single owner.** Both sources file into
/// `retry_triggers` — the window's `online` listener and the
/// transport's ICE `disconnected` → `failed` watcher — and neither
/// acts; this drains, asks [`crate::retry::RetryPolicy`], and
/// spawns. Two callbacks that each started a re-attempt would be
/// two owners of one retry, and both firing for one network change
/// is the defect this row exists to catch.
///
/// Called from the ticker rather than from the callbacks so the
/// decision is never taken while the node is borrowed. The cost is
/// up to one tick of latency on a repair that is already waiting on
/// ICE.
fn drive_retries(inner: &Rc<RefCell<Inner>>) {
    let starts = {
        let Ok(mut guard) = inner.try_borrow_mut() else {
            return;
        };
        if guard.closed {
            return;
        }
        guard.harvest_ice_failures();
        let triggers: Vec<RetryTrigger> = guard.retry_triggers.borrow_mut().drain(..).collect();
        let now = clock::now();
        let mut starts: Vec<(NodeId, crate::clock::Deadline)> = Vec::new();
        for (source, peer) in triggers {
            // An `online` event belongs to every peer this leaf owns
            // a re-attempt for, not to one: the network came back,
            // and which pairs that repairs is a question about the
            // pairs. A peer-scoped ICE failure belongs to its peer.
            let peers: Vec<NodeId> = match peer {
                Some(peer) => vec![peer],
                None => guard.direct_offerer.iter().copied().collect(),
            };
            for peer in peers {
                let interrupted = guard.direct_interrupted(peer);
                if let crate::retry::RetryDecision::Start(deadline) =
                    guard.retry.note(peer, source, now, interrupted)
                {
                    starts.push((peer, deadline));
                }
            }
        }
        starts
    };
    for (peer, deadline) in starts {
        let node = LeafNode {
            inner: Rc::clone(inner),
        };
        wasm_bindgen_futures::spawn_local(async move {
            node.re_attempt(peer, deadline).await;
        });
    }
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
    /// The peer this stream addresses, or `None` for the anchor.
    ///
    /// The node's own `open_stream` has always been peer-addressed
    /// (`crate::node::LeafNode::open_stream` takes a `NodeId` and
    /// `route_outbound` decides routed vs direct); this boundary
    /// pinned it to the anchor, so a page could establish a direct
    /// leaf ↔ leaf session under §9 and then had nothing that could
    /// send a byte over it. The only leaf → peer traffic a page
    /// could cause was signalling, which the anchor's per-pair
    /// application-data counter excludes — so §10's first two parts
    /// were unreachable from a page as well.
    ///
    /// 16 hex digits, read through [`parse_peer_id`]: the same
    /// spelling `node_id_hex()` hands out and the four peer methods
    /// take. A decimal id is refused rather than accepted as a
    /// second spelling.
    pub(crate) peer: Option<u64>,
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
        // `peer` is spelled the way the boundary HANDS OUT node ids —
        // 16 lowercase hex digits — and is `null` for the anchor.
        // Reported rather than left implicit because this reader is
        // the only way a page can confirm the leaf read its peer id
        // at all, which is exactly what `effective_ice_servers` was
        // added for after Stage 5 dropped every `RTCIceServer`
        // silently.
        let peer = self
            .peer
            .map_or_else(|| "null".to_string(), |peer| format!("\"{peer:016x}\""));
        format!(
            "{{\"reliability\":\"{reliability}\",\"label\":{},\"streamId\":{stream_id},\
             \"channelHash\":{channel_hash},\"peer\":{peer}}}",
            json_string(&self.label)
        )
    }

    /// Refuse a `peer` option on a surface that cannot honour it.
    ///
    /// [`stream_options`] is shared with the leader-proxied surface
    /// ([`crate::leader_session::MeshSession::open_stream`]) —
    /// deliberately, so the direct and the proxied path cannot read
    /// one options object two ways. But a follower's stream is
    /// opened by the LEADER tab's node, and `LeaderRequest::
    /// StreamOpen` carries no peer, so a `peer` that crossed there
    /// would be dropped and the stream would quietly address the
    /// leader's anchor instead. "The positions stopped arriving" is
    /// indistinguishable from a slow peer, so the proxied surface
    /// refuses the option by name instead of honouring it
    /// approximately.
    pub(crate) fn require_anchor_addressed(&self, surface: &str) -> Result<(), JsError> {
        match self.peer {
            None => Ok(()),
            Some(peer) => Err(JsError::new(&format!(
                "{surface} cannot address a peer: the stream would be opened by the leader \
                 tab's node, which this request carries no peer to. Call `openStream({{ peer: \
                 \"{peer:016x}\" }})` on the leader's own node."
            ))),
        }
    }
}

/// Read `{ reliability?, reliable?, label?, streamId?, channelHash?,
/// peer? }`.
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
        peer: match typed_string(opts, "peer", "16 hex digits")? {
            Some(raw) => Some(parse_peer_id(&raw)?),
            None => None,
        },
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

/// A peer id in the spelling every surface here HANDS OUT: 16
/// lowercase hex digits, no prefix.
///
/// [`parse_u64`] reads a bare string as DECIMAL, which is right for
/// `streamId` and wrong for a node id — and the mismatch was not
/// theoretical: `node_id_hex()` emits `00366d403ce19dac`, a page
/// passed exactly that back, and the decimal reading refused it. So
/// the four peer methods parse the spelling their own callers were
/// given: 16 hex digits, or a `0x`-prefixed hex string, and nothing
/// else. A decimal node id is refused rather than accepted as a
/// second spelling — one contract, and `0009` meaning two different
/// nodes depending on which method read it is exactly the class of
/// defect the `u64`-as-string rule exists to prevent.
pub(crate) fn parse_peer_id(raw: &str) -> Result<u64, JsError> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if hex.len() != 16 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsError::new(&format!(
            "{raw:?} is not a peer id: a node id crosses this boundary as 16 hex digits, \
             the spelling `node_id_hex()` produces. A query descriptor's `nodeId` is an \
             exact DECIMAL string and has to be converted, not passed through"
        )));
    }
    u64::from_str_radix(hex, 16).map_err(|_| JsError::new(&format!("{raw:?} is not a peer id")))
}

/// A `u64` from a decimal or `0x`-prefixed hex string. The boundary
/// never takes a JS number for a `u64`.
fn parse_u64(raw: &str) -> Result<u64, JsError> {
    crate::bootstrap::parse_node_id(raw)
        .ok_or_else(|| JsError::new(&format!("{raw:?} is not a u64 (decimal or 0x-hex)")))
}
