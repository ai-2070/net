//! Stage 5's browser witnesses, in the same runner and the same
//! ledger as Stage 4b's.
//!
//! # What is under test here, and what was under test in 4b
//!
//! Stage 4b's page owned the transport and nothing above it: it spoke
//! the bootstrap protocol itself, built every packet in wasm from
//! payload bytes the runner had encoded natively, and was explicitly
//! **not** an nRPC client. These witnesses are the other half. The
//! page calls `connect()` and then uses the `@net-mesh/browser`
//! surface — `call`, `openStream`, `announce`, `query` — and every
//! observable is still read **on the anchor**: a real nRPC handler
//! running, `find_best_node` resolving, `peer_session_id` not moving.
//! A witness here failing is the leaf crate or the wrapper failing,
//! never the harness guessing.
//!
//! # The Stage 5 bundle is a HARD dependency
//!
//! These witnesses need `net-mesh-leaf` built to wasm and
//! `@net-mesh/browser` built to a browser-loadable ESM bundle. When
//! either is absent every witness below is recorded **FAIL — absent**
//! with the exact paths and build commands. It is never skipped: a
//! browser job that silently emitted 12 witnesses instead of 19 is
//! indistinguishable from one whose Stage 5 half regressed, and the
//! CI floor exists precisely to make that impossible.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilityRequirement};
use net::adapter::net::cortex::{
    EventMeta, RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use net::adapter::net::{MeshNode, Reliability, StreamConfig, StreamError, MAX_EVENT_SIZE};

use crate::browser::{Driver, Engine, LaunchSpec};
use crate::udp_block::UdpProfile;
use crate::{hex, rpc_request_frame, unhex, wait_for, Ledger, StepResult};

/// The capability tag the browser announces and the anchor resolves.
const STAGE5_TAG: &str = "stage5.browser";
/// The native service the leaf's nRPC client calls.
const ECHO_SERVICE: &str = "app.stage5.echo";
/// A channel both tabs declare at `openSession`, so a promoted
/// follower has something to restore.
const RESTORE_CHANNEL: &str = "stage5.restore";
/// The native service the fire-and-forget stream's events dispatch to.
const SINK_SERVICE: &str = "app.stage5.sink";
/// The native service the leader-close leg parks a call inside, so a
/// call can be genuinely IN FLIGHT — entered on the anchor, unanswered
/// — at the moment the leader stands down.
const PARK_SERVICE: &str = "app.stage5.park";
/// A capability tag announced at RUNTIME, through the leader, by the
/// tab that is NOT the leader. A promoted follower must re-announce
/// current intent, so the anchor must still resolve this tag after the
/// handoff.
const RESTORE_TAG: &str = "stage5.restore.tag";

/// How many sequential round trips the reliable witness makes, and
/// how many it then puts IN FLIGHT at once.
///
/// The sequential leg establishes exact replies and the native
/// handler's arrival order. It is deliberately not described as
/// retransmission or reorder correctness: awaiting each reply before
/// sending the next never puts two requests on the wire together, so
/// a fire-and-forget stream on a loss-free link satisfies it too.
/// What the concurrent leg adds is the property sequencing hides —
/// N replies outstanding at once, each of which must come back to
/// its OWN caller exactly once. Recovery from real loss is a
/// separate, separately named witness.
const RELIABLE_ROUND_TRIPS: usize = 64;
const RELIABLE_IN_FLIGHT: usize = 16;
/// Events the fire-and-forget witness sends, and the drop period.
const FAF_EVENTS: usize = 40;
const FAF_DROP_EVERY: u64 = 4;

// --- the REAL stream-ABI exercises (R11 / E6) ----------------------
//
// Stream ids the page PINS at `openStream` so the anchor can open
// the same id from its side and push bytes down it. Bit 49 is the
// leaf's own stream discriminator and bit 48 (a channel
// publication) is deliberately clear, so an id the leaf did not
// register would still classify as a stream rather than a channel.
const ABI_DIRECT_STREAM_ID: u64 = 0x0002_0000_0000_5A01;
const ABI_WINDOW_STREAM_ID: u64 = 0x0002_0000_0000_5A02;
const ABI_PROXY_STREAM_ID: u64 = 0x0002_0000_0000_5A03;

/// The large-message witness's OWN stream, and why it has one.
///
/// Its native → leaf legs used to ride `ABI_DIRECT_STREAM_ID` and
/// count payloads by INDEX — "everything after the twelve that
/// stream already carried". That coupling made one leg's verdict a
/// function of another leg's arrival count, and it misreported both
/// directions: a short earlier list shifted the index, so the
/// ceiling payload landed inside the skipped prefix and the leg
/// reported "REFUSED typed, but something else also arrived" for a
/// stream on which NOTHING had arrived; and a payload recovered
/// late by a retransmit would have shifted it the other way. A
/// dedicated stream makes both structurally impossible: this
/// witness asserts on payloads that can only be its own, from
/// sequence zero, and needs no index arithmetic to say what
/// arrived.
const ABI_LARGE_STREAM_ID: u64 = 0x0002_0000_0000_5A04;

/// Payloads the direct stream witness pushes native → leaf, and
/// their size.
const ABI_DIRECT_EVENTS: usize = 12;
const ABI_DIRECT_SIZE: usize = 512;

/// The credit window the anchor opens its sustained-traffic stream
/// with, and the traffic it then puts through it.
///
/// `ABI_WINDOW_EVENTS * ABI_WINDOW_SIZE` is eight times the window,
/// so the sender's initial credit is exhausted seven times over and
/// the transfer can only complete if the RECEIVER — the browser leaf
/// — kept emitting `StreamWindow` grants and they kept arriving.
const ABI_WINDOW_BYTES: u32 = 16_384;
const ABI_WINDOW_EVENTS: usize = 64;
const ABI_WINDOW_SIZE: usize = 2_048;

/// The reliable loss/reorder exercise: how many nRPC REQUEST events
/// ride one RELIABLE stream, and the drop / reorder periods the
/// page's transport hooks are armed with.
///
/// Both hooks are armed at once and both must fire: the witness
/// requires `dropped > 0` and `reordered > 0` before it will accept
/// the recovery it observes as recovery from anything.
const ABI_RELIABLE_EVENTS: usize = 40;
const ABI_RELIABLE_DROP_EVERY: u64 = 5;
const ABI_RELIABLE_REORDER_EVERY: u64 = 3;

/// The three sizes the large-message witness uses, and why each is
/// the size it is.
///
/// `MAX_EVENT_SIZE` is 8 104 B (`MAX_PAYLOAD_SIZE` minus the event
/// frame's 4-byte length prefix) and no NATIVE node reassembles leaf
/// fragments (§9.2), so the largest body that can round-trip leaf →
/// native → leaf through `call()` is one that fits, with its nRPC
/// framing, in a single event. `ABI_CEILING_SIZE` sits just under
/// that: the reply is `echo:` + the body, so it must fit too.
const ABI_CEILING_SIZE: usize = 7_800;
/// Past 64 832 B, where §9.2 says the leaf attempts nothing and
/// returns a typed `LeafError::Wire` naming streams. The witness
/// asserts the refusal is typed and that nothing truncated reaches
/// the far side — which is the contract, where "96 KiB works" is
/// not.
const ABI_OVER_LIMIT_SIZE: usize = 96 * 1024;
/// The leaf's fragmentation ceiling, DERIVED rather than restated:
/// `net_leaf::frame::MAX_FRAGMENTED_PAYLOAD` is `MAX_FRAGMENT_PAYLOAD`
/// (= `MAX_EVENT_SIZE`) times `MAX_FRAGMENTS`. The runner does not
/// depend on the leaf crate, so the eight is the one number written
/// here; if either factor moves, this moves with it and leg 2's
/// classifier below follows.
const ABI_LEAF_MAX_FRAGMENTS: usize = 8;
const ABI_LEAF_FRAG_CEILING: usize = MAX_EVENT_SIZE * ABI_LEAF_MAX_FRAGMENTS;
/// Native → leaf on a stream, over `MAX_EVENT_SIZE`. Nothing on the
/// native side fragments a stream event, so this is the size at which
/// the sender must REFUSE, typed, naming the limit — the leg that
/// used to return `Ok` and deliver nothing.
const ABI_STREAM_LARGE_SIZE: usize = 32 * 1024;

/// Payloads the leader-PROXIED stream witness pushes native → leaf.
const ABI_PROXY_EVENTS: usize = 8;
const ABI_PROXY_SIZE: usize = 700;

/// Every Stage 5 witness name, in ledger order. The CI job pins these
/// exactly; the list is here so a rename is one edit and a drop is
/// impossible to do quietly.
pub const WITNESSES: [&str; 14] = [
    "stage5_leaf_handshake_over_the_real_listener",
    "stage5_reliable_round_trip",
    "stage5_nrpc_call_to_a_native_service",
    "stage5_fire_and_forget_tolerates_injected_loss",
    "stage5_find_best_node_returns_the_browser_node",
    "stage5_two_tabs_share_one_identity_without_eviction",
    "stage5_reconnect_displaces_a_busy_incumbent",
    "stage5_udp_blocked_surfaces_a_typed_failure",
    "stage5_direct_event_callback_may_reenter_the_node",
    "stage5_direct_stream_carries_native_bytes_to_callback_and_iterator",
    "stage5_native_stream_sustains_traffic_beyond_the_credit_window",
    "stage5_reliable_stream_recovers_injected_loss_and_reorder",
    "stage5_large_messages_cross_the_public_api_in_both_directions",
    "stage5_leader_proxied_stream_carries_native_bytes_both_ways",
];

// ===================================================================
// The step protocol the Stage 5 page executes
// ===================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step5 {
    /// `connect(opts)` through `@net-mesh/browser`.
    Connect {
        id: u64,
        session: String,
        credential: String,
        bootstrap_url: String,
        origin: String,
        /// The anchor's published `rtc_addr`, which is what the
        /// leaf's STUN probe targets.
        anchor_rtc_addr: String,
        stun: Option<String>,
        /// Custodial identity injection (`MeshNodeConfig::entity_keypair`'s
        /// shape). Both tabs get the SAME pair, which is what makes
        /// "one identity in two tabs" a fact rather than a wish.
        entity_secret_hex: Option<String>,
        noise_secret_hex: Option<String>,
        /// Open through §8's `openSession` (Web Lock election,
        /// leader/follower roles, generation fencing) instead of the
        /// plain `connect`. The returned session carries the same
        /// `call`/`subscribe`/`publish`/`announce`/`query`/
        /// `openStream`/`close` surface, so every other step works
        /// unchanged against it.
        use_session: bool,
        /// Declared up front so the new leader can restore them.
        capabilities: Vec<String>,
        subscriptions: Vec<String>,
        /// Pins the Web Lock name, so two tabs contend deliberately
        /// rather than by accident of origin.
        lock_scope: Option<String>,
        /// A `connect` that must reject with a typed failure.
        expect_failure: bool,
    },
    /// `nodeIdHex()` and, if the leaf has a leadership surface, the
    /// tab's role.
    Info {
        id: u64,
        session: String,
    },
    /// One `call(service, payload, timeoutMs)`.
    Call {
        id: u64,
        session: String,
        service: String,
        payload: String,
        timeout_ms: u64,
    },
    /// Issue `call(...)` and return WITHOUT awaiting it, parking the
    /// promise under `handle`. The point is a call that is genuinely
    /// outstanding while the runner does something else to the tab —
    /// a leader stand-down, for instance.
    CallBegin {
        id: u64,
        session: String,
        service: String,
        payload: String,
        timeout_ms: u64,
        handle: String,
    },
    /// Await a call parked by [`Step5::CallBegin`].
    ///
    /// A promise that has not settled within `timeout_ms` comes back
    /// as `info = "never settled"` rather than hanging the step, so a
    /// call that was silently abandoned is a reportable outcome and
    /// not a harness timeout.
    CallAwait {
        id: u64,
        handle: String,
        timeout_ms: u64,
    },
    /// `payloads.len()` calls on one session, so a multi-call
    /// assertion costs one HTTP round trip instead of one per call.
    ///
    /// `concurrent` decides which property is under test. `false`
    /// awaits each reply before issuing the next — one request on the
    /// wire at a time, which is what makes arrival ORDER assertable.
    /// `true` issues every call before awaiting any of them, so N
    /// requests are outstanding together and each reply has to find
    /// its own caller; `replies[i]` is still the reply to
    /// `payloads[i]`, by index, never by arrival.
    CallMany {
        id: u64,
        session: String,
        service: String,
        payloads: Vec<String>,
        timeout_ms: u64,
        concurrent: bool,
    },
    /// `openStream(opts)` + one `send` per payload, with the page's
    /// DataChannel drop hook armed at `drop_every`.
    StreamSend {
        id: u64,
        session: String,
        reliable: bool,
        /// The publish contract's ids, so the anchor dispatches
        /// these events to a real handler.
        stream_id: String,
        channel_hash: u16,
        payloads: Vec<String>,
        drop_every: u64,
    },
    /// `openStream(opts)` through the built package, held open
    /// across steps under `handle`, with BOTH of its consumers —
    /// the `onMessage` callback and the `for await` async iterator —
    /// attached before a byte can arrive.
    ///
    /// Works unchanged on a direct `connect()` node and on a
    /// leader-proxied `MeshSession`; which one it was comes back in
    /// the result, read off the object the package returned.
    StreamOpen {
        id: u64,
        session: String,
        handle: String,
        reliable: bool,
        label: Option<String>,
        /// Pinned so the ANCHOR can open the same id from its side
        /// and send on it. Decimal, because a u64 is not a JS number.
        stream_id: Option<String>,
        channel_hash: Option<u16>,
    },
    /// Send on a stream [`Step5::StreamOpen`] already opened.
    ///
    /// Either verbatim `frames` (the natively encoded nRPC requests a
    /// real handler dispatches) or `count` payloads of `size` bytes
    /// from the deterministic generator both sides implement — which
    /// is what keeps a multi-megabyte exercise off the step channel.
    /// `drop_every` and `reorder_every` arm the page's transport
    /// hooks for the duration of the step.
    StreamWrite {
        id: u64,
        handle: String,
        frames: Vec<String>,
        seed: u64,
        size: usize,
        count: usize,
        drop_every: u64,
        reorder_every: u64,
    },
    /// What the open stream's callback and iterator have received.
    /// Waits for `expect` payloads on both and reports either way.
    StreamInbox {
        id: u64,
        handle: String,
        expect: usize,
        timeout_ms: u64,
    },
    /// One `call()` whose request and reply are both large enough to
    /// fragment. Bodies are generated from `(seed, size)` on both
    /// sides and compared by length + FNV-1a/32.
    CallSized {
        id: u64,
        session: String,
        service: String,
        seed: u64,
        size: usize,
        timeout_ms: u64,
    },
    Announce {
        id: u64,
        session: String,
        capabilities: Vec<String>,
    },
    Query {
        id: u64,
        session: String,
        capability: String,
    },
    /// `probeStunBinding(addr)` — the falsifier for the
    /// `UdpBlocked` typing, exported by `@net-mesh/browser`. The
    /// outcome (`reflexive` | `stunError` | `unanswered`) comes
    /// back in `info`.
    StunProbe {
        id: u64,
        addr: String,
    },
    /// Register an event listener on a **direct** node that calls
    /// straight back into it — a synchronous counter read and a
    /// synchronous `openStream` — and record what each attempt did.
    ///
    /// The one schedule no unit test can reach: `wasm::Inner` exists
    /// only behind a real `connect()`, so the borrow discipline
    /// around its listeners is observable here and nowhere else.
    ArmReentry {
        id: u64,
        session: String,
    },
    /// What the armed listener observed. Readable after the session
    /// is closed, because closing is what fires the event.
    ReentryReport {
        id: u64,
        session: String,
    },
    // ── Stage 6, §9: browser ↔ browser from a page ──
    //
    // Two families, deliberately both: `PeerConnect`/`PeerAccept`
    // drive `@net-mesh/browser`'s page-facing loop, and
    // `PeerOffer`/`PeerAcceptOffer`/`PeerCandidate`/`PeerHandshake`
    // drive the four `#[wasm_bindgen]` methods one at a time. The
    // fine-grained family is what lets a witness put a failure
    // BETWEEN two steps — which is the only way some of the typed
    // failures are reachable at all.
    /// `node.connectPeer(peer)` — the offerer's whole loop.
    PeerConnect {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// `node.acceptPeer(peer)` — the answerer's whole loop.
    PeerAccept {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// `node.peerAttempt(peer)` — one service of a live attempt,
    /// without driving it to a conclusion.
    PeerAttempt {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// The raw `peer_offer`.
    PeerOffer {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// The raw `peer_accept_offer`.
    PeerAcceptOffer {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// The raw `peer_candidate`.
    PeerCandidate {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// The raw `peer_handshake`.
    PeerHandshake {
        id: u64,
        session: String,
        peer_hex: String,
    },
    /// `node.handshakePeer(peer, dialog)` — `connectPeer`'s last
    /// step, with `connectPeer`'s typing, on an attempt the runner
    /// drove through the raw methods itself.
    ///
    /// The only way a witness can reach the `handshakeFailed`
    /// disposition without racing: `connectPeer` decides to hand
    /// shake the moment its own poll reads `open`, so a peer that
    /// has to be gone by then cannot be taken away in time. Driving
    /// the four methods to an open channel, closing the peer, and
    /// then asking for the handshake puts the failure where the
    /// runner chooses rather than where the scheduler lands.
    PeerHandshakeTyped {
        id: u64,
        session: String,
        peer_hex: String,
        dialog: String,
    },
    /// The declared parameter count of each of the four methods.
    ///
    /// An assertion, not a step: `peer_handshake` taking a peer id
    /// and NOTHING else is the security property of the slice, and
    /// an arity read off the live boundary is the only way to observe
    /// from outside that there is no parameter a key could arrive
    /// through.
    PeerArity {
        id: u64,
        session: String,
    },
    /// Every leaf counter, with no attempt required.
    ///
    /// Separate from `PeerAttempt` because that one refuses a peer it
    /// has no live attempt with — correctly — and a ledger read must
    /// not depend on an attempt being live at the moment it is taken.
    PeerCounters {
        id: u64,
        session: String,
    },
    Close {
        id: u64,
        session: String,
    },
    Done {
        id: u64,
    },
}

impl Step5 {
    pub fn id_mut(&mut self) -> &mut u64 {
        match self {
            Self::Connect { id, .. }
            | Self::Info { id, .. }
            | Self::Call { id, .. }
            | Self::CallBegin { id, .. }
            | Self::CallAwait { id, .. }
            | Self::CallMany { id, .. }
            | Self::StreamSend { id, .. }
            | Self::StreamOpen { id, .. }
            | Self::StreamWrite { id, .. }
            | Self::StreamInbox { id, .. }
            | Self::CallSized { id, .. }
            | Self::Announce { id, .. }
            | Self::Query { id, .. }
            | Self::StunProbe { id, .. }
            | Self::ArmReentry { id, .. }
            | Self::ReentryReport { id, .. }
            | Self::PeerConnect { id, .. }
            | Self::PeerAccept { id, .. }
            | Self::PeerAttempt { id, .. }
            | Self::PeerOffer { id, .. }
            | Self::PeerAcceptOffer { id, .. }
            | Self::PeerCandidate { id, .. }
            | Self::PeerHandshake { id, .. }
            | Self::PeerHandshakeTyped { id, .. }
            | Self::PeerArity { id, .. }
            | Self::PeerCounters { id, .. }
            | Self::Close { id, .. }
            | Self::Done { id } => id,
        }
    }

    pub fn id(&self) -> u64 {
        match self {
            Self::Connect { id, .. }
            | Self::Info { id, .. }
            | Self::Call { id, .. }
            | Self::CallBegin { id, .. }
            | Self::CallAwait { id, .. }
            | Self::CallMany { id, .. }
            | Self::StreamSend { id, .. }
            | Self::StreamOpen { id, .. }
            | Self::StreamWrite { id, .. }
            | Self::StreamInbox { id, .. }
            | Self::CallSized { id, .. }
            | Self::Announce { id, .. }
            | Self::Query { id, .. }
            | Self::StunProbe { id, .. }
            | Self::ArmReentry { id, .. }
            | Self::ReentryReport { id, .. }
            | Self::PeerConnect { id, .. }
            | Self::PeerAccept { id, .. }
            | Self::PeerAttempt { id, .. }
            | Self::PeerOffer { id, .. }
            | Self::PeerAcceptOffer { id, .. }
            | Self::PeerCandidate { id, .. }
            | Self::PeerHandshake { id, .. }
            | Self::PeerHandshakeTyped { id, .. }
            | Self::PeerArity { id, .. }
            | Self::PeerCounters { id, .. }
            | Self::Close { id, .. }
            | Self::Done { id } => *id,
        }
    }
}

pub type Step5Item = (Step5, oneshot::Sender<StepResult>);
pub type Step5Sender = mpsc::Sender<Step5Item>;
pub type Step5Queue = Arc<tokio::sync::Mutex<mpsc::Receiver<Step5Item>>>;

/// The two tabs the Stage 5 witnesses drive. `a` carries every
/// single-tab witness; `b` exists for the shared-identity one.
pub const TABS: [&str; 2] = ["a", "b"];

/// Step ids live above this so they can never collide with the 4b
/// script's ids — both scripts post results to one `/harness/result`
/// keyed by id.
const STEP5_ID_BASE: u64 = 1_000_000;

/// The anchor's view of this peer, appended to every witness whose
/// failure would otherwise read only as "the call's deadline
/// elapsed".
///
/// §12 refuses a PROVISIONAL peer's calls, announcements and
/// subscriptions, and that refusal reaches the leaf as an elapsed
/// deadline — so a leaf whose `connect()` never ran the enrollment
/// exchange looks, from the page, exactly like a slow anchor. The
/// discriminator is on the anchor, so the ledger carries it.
fn peer_state(anchor: &MeshNode, node_id: u64) -> String {
    let session = anchor.peer_session_id(node_id);
    let provisional = anchor.peer_is_provisional(node_id);
    let reading = if session.is_none() {
        "NO session: the anchor has already reclaimed or evicted this peer, so nothing \
         above the transport could have worked"
    } else if provisional {
        "PROVISIONAL: §12 refuses this peer's calls, announcements and subscriptions, and \
         the refusal arrives at the leaf as an elapsed deadline — so an enrollment \
         exchange that did not run is indistinguishable from a slow anchor unless you \
         read this field"
    } else {
        "ENROLLED: §12 is NOT refusing this peer, so a failure above this line is a real \
         defect in the path under test and not an admission gate"
    };
    format!("session={session:?} provisional={provisional} — {reading}")
}

/// The ANCHOR's own reliable-stream ledger for `stream_id`, as the
/// sender side of a payload that did not arrive.
///
/// The three questions a missing payload raises, and none of them is
/// answerable from the page: does the sender still OWN the packet
/// (a retransmit is still coming), did it ABANDON a sequence (its
/// retry budget ran out and the peer has been told to reset), and
/// what has the peer ACKNOWLEDGED (a frontier short of `tx_seq` is
/// an unacknowledged tail; one that covers everything with a payload
/// still missing means the loss is downstream of the acknowledgement
/// — at the receiver, not on the wire).
fn sender_ledger(anchor: &MeshNode, node_id: u64, stream_id: u64) -> String {
    let Some(session) = anchor.peer_session_for_test(node_id) else {
        return "no session: the anchor has no peer to have sent on".into();
    };
    let Some(state) = session.try_stream(stream_id) else {
        return format!("no stream state for {stream_id:#x} on the anchor's session");
    };
    let (pending, frontier, abandoned, nack) = state.with_reliability(|r| {
        (
            r.has_pending(),
            r.ack_frontier(),
            r.abandoned_seq(),
            r.build_nack(),
        )
    });
    let stats = anchor.stream_stats(node_id, stream_id);
    let tx_seq = stats.map_or(0, |s| s.tx_seq);
    let grants = stats.map_or(0, |s| s.credit_grants_received);
    let reading = match (abandoned, frontier) {
        (Some(seq), f) => format!(
            "GAVE UP on sequence {seq} (acknowledgement frontier {f:?}): the retry budget \
             ran out, the peer was sent a reset, and its receive half is retired — every \
             later payload on this stream is dropped there"
        ),
        (None, Some(f)) if f >= tx_seq => "fully acknowledged: every sequence this stream \
                                           issued was reported received, so a payload the \
                                           consumer never saw was lost at the receiver, \
                                           after the acknowledgement"
            .into(),
        (None, f) => format!(
            "still owned (pending={pending}, frontier {f:?} of {tx_seq} issued): a \
             retransmit is still due, so a payload missing here is in flight rather than \
             lost"
        ),
    };
    format!(
        "tx_seq={tx_seq} pending={pending} ack_frontier={frontier:?} abandoned={abandoned:?} \
         outstanding_gap={nack:?} credit_grants_received={grants} — {reading}"
    )
}

pub struct Script5 {
    tabs: HashMap<String, Step5Sender>,
    next_id: u64,
}

impl Script5 {
    /// A script over `tabs`, numbering its steps from `id_base`.
    ///
    /// The base matters: every script in this runner posts its
    /// results to one `/harness/result` keyed by step id, so two
    /// scripts sharing a range would resolve each other's steps.
    pub fn new(tabs: HashMap<String, Step5Sender>, id_base: u64) -> Self {
        Self {
            tabs,
            next_id: id_base,
        }
    }

    pub async fn run(&mut self, tab: &str, step: Step5) -> StepResult {
        self.spawn(tab, step).await
    }

    /// Put a step **in flight now** and hand back a future that
    /// resolves to its result.
    ///
    /// What makes two tabs drivable at once. A browser ↔ browser
    /// attempt needs it: the answerer's loop blocks until an offer
    /// arrives, so an offerer that could only be started after the
    /// answerer's step had RETURNED would spend its whole ICE
    /// deadline before the answerer was asked anything.
    ///
    /// **The dispatch is spawned onto the runtime, not merely
    /// described.** A Rust future does nothing until something polls
    /// it, so returning the `async move` block alone left the step
    /// unsent until the caller awaited it — which for the Stage 6
    /// page-facing witness meant awaiting it *after* the offerer's
    /// `connectPeer` had returned. The answerer's `acceptPeer` then
    /// entered 10 042 ms after the offerer, one full
    /// `PEER_ICE_DEADLINE_MS` late, and both halves reported
    /// `iceTimeout` while the identical sequence driven one call at
    /// a time by the runner was green. A method named `spawn` that
    /// only builds a future is a lie the type system does not catch,
    /// so it spawns.
    pub fn spawn(
        &mut self,
        tab: &str,
        mut step: Step5,
    ) -> impl Future<Output = StepResult> + 'static {
        let id = self.next_id;
        self.next_id += 1;
        *step.id_mut() = id;
        let tab = tab.to_string();
        let tx = self.tabs.get(&tab).cloned();
        let dispatch = tokio::spawn(async move {
            let Some(tx) = tx else {
                return fail(format!("no page tab named {tab}"));
            };
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx.send((step, reply_tx)).await.is_err() {
                return fail("the page server is gone");
            }
            match tokio::time::timeout(Duration::from_secs(180), reply_rx).await {
                Ok(Ok(r)) => r,
                Ok(Err(_)) => fail("the step was dropped"),
                Err(_) => fail("the page did not answer this step in 180 s"),
            }
        });
        async move {
            dispatch
                .await
                .unwrap_or_else(|e| fail(format!("the step dispatch task failed: {e}")))
        }
    }
}

fn fail(msg: impl Into<String>) -> StepResult {
    StepResult {
        ok: false,
        error: Some(msg.into()),
        ..Default::default()
    }
}

// ===================================================================
// Native services the witnesses read
// ===================================================================

/// An echo provider whose ordered call log is the observable: the
/// reliable witness passes only if the bodies reached the handler in
/// the order the browser sent them.
struct Echo(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

#[async_trait::async_trait]
impl RpcHandler for Echo {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let body = ctx.payload.body.to_vec();
        self.0
            .lock()
            .expect("the echo log is never poisoned")
            .push(body.clone());
        let mut out = Vec::with_capacity(body.len() + 5);
        out.extend_from_slice(b"echo:");
        out.extend_from_slice(&body);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(out),
        })
    }
}

/// A sink for the fire-and-forget events. Records every body that
/// reached the handler, in arrival order, and counts.
struct Sink {
    seen: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcHandler for Sink {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("the sink log is never poisoned")
            .push(ctx.payload.body.to_vec());
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from_static(b"sunk"),
        })
    }
}

/// A service that ENTERS and then does not answer until the runner
/// releases it.
///
/// This is what makes "a call was pending across the leader's
/// stand-down" a fact rather than a hope: the body is recorded the
/// moment the anchor's handler runs, so the runner can prove the
/// request really reached the native side, and the reply is withheld
/// so the call is still outstanding in the tab when the leader goes
/// away. `entered` keeps one entry PER INVOCATION, so a duplicated
/// dispatch (the same logical call executed twice) is visible as a
/// length of 2.
struct Park {
    entered: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    release: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl RpcHandler for Park {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let body = ctx.payload.body.to_vec();
        self.entered
            .lock()
            .expect("the park log is never poisoned")
            .push(body.clone());
        // Parked. The permit is added by the runner once it has
        // finished with the interruption it arranged.
        let _permit = self.release.acquire().await;
        let mut out = Vec::with_capacity(body.len() + 7);
        out.extend_from_slice(b"parked:");
        out.extend_from_slice(&body);
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(out),
        })
    }
}

// ===================================================================
// The bundle
// ===================================================================

/// Where the Stage 5 artefacts are expected, and how to build them.
pub struct Bundle {
    /// `@net-mesh/browser`'s `dist`, served at `/browser/`.
    pub dist: PathBuf,
    /// The leaf's `wasm-bindgen` output, for the size report and as
    /// a fallback when the wrapper's copy step has not run.
    pub wasm_pkg: PathBuf,
}

impl Bundle {
    /// `net/crates/net/{browser-ts/dist, leaf/pkg}` relative to the
    /// harness directory (`net/crates/net/tests/rtc_browser`).
    pub fn locate(harness_root: &Path) -> Self {
        let net = harness_root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| harness_root.to_path_buf());
        Self {
            dist: net.join("browser-ts/dist"),
            wasm_pkg: net.join("leaf/pkg"),
        }
    }

    /// `Ok(())`, or the exact reason the Stage 5 half cannot run.
    pub fn check(&self) -> Result<(), String> {
        let entry = self.dist.join("index.js");
        let glue = self.dist.join("net_leaf.js");
        let wasm = self.dist.join("net_leaf_bg.wasm");
        let mut missing: Vec<String> = Vec::new();
        for p in [&entry, &glue, &wasm] {
            if !p.exists() {
                missing.push(p.display().to_string());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the Stage 5 bundle is ABSENT — missing {}. Build it with: \
             (1) cd net/crates/net/leaf && cargo build --release --target \
             wasm32-unknown-unknown && wasm-bindgen --target web --out-dir pkg \
             target/wasm32-unknown-unknown/release/net_leaf.wasm; \
             (2) cd net/crates/net/browser-ts && npm install && npm run build \
             (its build copies the leaf's pkg/ next to dist/index.js). \
             This is a FAILURE, not a skip: the CI floor of {} witnesses is what \
             keeps a regressed Stage 5 half from looking like a green Stage 4b one.",
            missing.join(", "),
            12 + WITNESSES.len()
        ))
    }

    /// Raw sizes of the artefacts, for the report. Gzipped sizes are
    /// the leaf/wrapper slices' own exit criterion and are measured
    /// where the bundle is built; this runner records what it serves.
    pub fn sizes(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for name in [
            "index.js",
            "index.bundle.js",
            "net_leaf.js",
            "net_leaf_bg.wasm",
        ] {
            if let Ok(md) = std::fs::metadata(self.dist.join(name)) {
                out.push((format!("dist/{name}"), md.len()));
            }
        }
        // The leaf's own wasm-bindgen output, which is what the
        // wrapper's build copies into `dist`. Recorded separately so
        // a stale copy in `dist` is visible as a size mismatch.
        for name in ["net_leaf.js", "net_leaf_bg.wasm"] {
            if let Ok(md) = std::fs::metadata(self.wasm_pkg.join(name)) {
                out.push((format!("leaf/pkg/{name}"), md.len()));
            }
        }
        out
    }
}

// ===================================================================
// The witnesses
// ===================================================================

pub struct Cx<'a> {
    pub driver: &'a Driver,
    pub engine: Engine,
    pub anchor: &'a Arc<MeshNode>,
    pub anchor_rtc_addr: SocketAddr,
    pub credential: String,
    pub bootstrap_url: String,
    pub origin: String,
    /// `http://localhost:PORT` — the page server's origin.
    pub page_origin: String,
    pub stun: Option<String>,
    pub bundle: Bundle,
    pub tabs: HashMap<String, Step5Sender>,
    /// How the run's browser was launched, so the UDP profile can
    /// relaunch the SAME browser with exactly one field changed.
    pub launch: LaunchSpec,
}

/// Relaunch the run's browser with its non-proxied UDP on or off, and
/// reopen the Stage 5 tab on it.
///
/// The UDP-blocked profile on a host whose firewall cannot filter
/// same-host traffic is an ENGINE setting, and an engine setting is
/// fixed at launch — so establishing it means a relaunch, and undoing
/// it means another. Every step-queue consumer is re-created with the
/// page, and the sessions from earlier witnesses are gone with it,
/// which is why this witness runs last.
async fn relaunch(cx: &Cx<'_>, udp_off: bool, url: &str, what: &str) -> Result<String, String> {
    let _ = cx.driver.close_page("leaf5-a").await;
    cx.driver.shutdown_browser().await?;
    let mut spec = cx.launch.clone();
    spec.webrtc_udp_off = udp_off;
    let launched = cx.driver.launch(&spec).await?;
    cx.driver.open_page("leaf5-a", url).await?;
    Ok(format!(
        "{} {} was relaunched {what} — UDP: {}",
        cx.engine.as_str(),
        launched.version,
        launched.udp
    ))
}

/// Run every Stage 5 witness. Returns `Err` only for a harness fault
/// (a browser that would not open a tab); a witness that failed is
/// recorded and the run continues, exactly like the 4b half.
#[expect(clippy::too_many_lines, reason = "one linear witness script")]
pub async fn run(cx: Cx<'_>, ledger: &mut Ledger) -> Result<(), String> {
    if let Err(why) = cx.bundle.check() {
        for name in WITNESSES {
            ledger.record(name, false, why.clone());
        }
        return Ok(());
    }
    for (name, bytes) in cx.bundle.sizes() {
        println!("[stage5] bundle {name}: {bytes} bytes raw");
    }

    // --- the native services the witnesses read --------------------
    let echo_log: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _echo = cx
        .anchor
        .serve_rpc(ECHO_SERVICE, Arc::new(Echo(Arc::clone(&echo_log))))
        .map_err(|e| format!("serve {ECHO_SERVICE}: {e}"))?;
    let sink_log: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_count = Arc::new(AtomicUsize::new(0));
    let _sink = cx
        .anchor
        .serve_rpc(
            SINK_SERVICE,
            Arc::new(Sink {
                seen: Arc::clone(&sink_log),
                count: Arc::clone(&sink_count),
            }),
        )
        .map_err(|e| format!("serve {SINK_SERVICE}: {e}"))?;
    let park_log: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Zero permits: the first (and only) call parks until the runner
    // adds one.
    let park_release = Arc::new(tokio::sync::Semaphore::new(0));
    let _park = cx
        .anchor
        .serve_rpc(
            PARK_SERVICE,
            Arc::new(Park {
                entered: Arc::clone(&park_log),
                release: Arc::clone(&park_release),
            }),
        )
        .map_err(|e| format!("serve {PARK_SERVICE}: {e}"))?;

    let mut script = Script5::new(cx.tabs.clone(), STEP5_ID_BASE);

    // Tab `a` carries every single-tab witness.
    let url_a = format!("{}/leaf5.html?tab=a", cx.page_origin);
    cx.driver
        .open_page("leaf5-a", &url_a)
        .await
        .map_err(|e| format!("opening the Stage 5 tab: {e}"))?;

    // What this engine actually offers the leaf, read in the page
    // rather than assumed from the engine name. S0b's finding — that
    // `RTCPeerConnection` is undefined in a worker — is the reason
    // the leaf runs on the main thread, so the same fact is worth
    // recording per engine rather than inferred.
    match cx
        .driver
        .eval(
            "leaf5-a",
            "({ ua: navigator.userAgent, \
               pc: typeof RTCPeerConnection, \
               dc: typeof RTCDataChannel, \
               locks: typeof navigator.locks, \
               idb: typeof indexedDB, \
               crypto: typeof (crypto && crypto.subtle) })",
        )
        .await
    {
        Ok(v) => println!("[stage5] engine surface: {v}"),
        Err(e) => println!("[stage5] engine surface unreadable: {e}"),
    }

    // ONE custodial identity for the whole Stage 5 run, injected the
    // way `MeshNodeConfig::entity_keypair` is. Without it every tab
    // mints its own keypair and "two tabs share one identity" could
    // only ever report "they didn't"; with it, both tabs really do
    // present the same node id and the witness measures the thing it
    // is named after.
    let entity_secret = hex(&random_32());
    let noise_secret = hex(&random_32());
    // The Web Lock the two tabs contend for, pinned so the
    // contention is deliberate.
    let lock_scope = format!("net-mesh/stage5/{}", &entity_secret[..16]);
    let connect_as = |session: &str, expect_failure: bool, use_session: bool| Step5::Connect {
        id: 0,
        session: session.to_string(),
        credential: cx.credential.clone(),
        bootstrap_url: cx.bootstrap_url.clone(),
        origin: cx.origin.clone(),
        anchor_rtc_addr: cx.anchor_rtc_addr.to_string(),
        stun: cx.stun.clone(),
        entity_secret_hex: Some(entity_secret.clone()),
        noise_secret_hex: Some(noise_secret.clone()),
        use_session,
        capabilities: vec![STAGE5_TAG.to_string()],
        subscriptions: vec![RESTORE_CHANNEL.to_string()],
        lock_scope: Some(lock_scope.clone()),
        expect_failure,
    };
    let connect_step =
        |session: &str, expect_failure: bool| connect_as(session, expect_failure, false);

    // ================================================================
    // 1 — handshake through the REAL listener, driven by the leaf
    //
    // WHAT THIS WITNESS DOES NOT PROVE. A session forming does not
    // prove the trickle path works: the anchor learns the browser
    // peer-reflexively from the incoming STUN check, so a leaf whose
    // trickled candidates are ALL discarded by the listener still
    // reaches a DataChannel on a same-host or same-LAN run. (That
    // was a live defect — the leaf's candidates carried no
    // `"type":"candidate"` field — found by reading the leaf, not by
    // this witness.) The candidate path is what
    // `mdns_on_pair_formed` measures, and it measures it on the 4b
    // page; this witness is about the leaf driving the whole
    // sequence, not about which candidate won.
    // ================================================================
    let connected = script.run("a", connect_step("main", false)).await;
    let node_hex = connected.node_id.clone().unwrap_or_default();
    let node_id = u64::from_str_radix(node_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    // The leaf's ORIGIN HASH, which is NOT its node id. Every event
    // payload this harness encodes natively and hands the leaf to
    // send must carry it in the `EventMeta`: the anchor drops a
    // direct peer's frame whose packet-header origin and payload
    // origin disagree ("a direct peer must not run the fold under a
    // forged payload origin"), and the header's origin is the leaf's
    // own. Encoding the node id there instead cost a whole witness —
    // 40 events sent, 40 dropped before admission, and the only
    // artifact was a handler that never ran.
    let origin_hex = connected.origin_hash.clone().unwrap_or_default();
    let origin_hash = u64::from_str_radix(origin_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    {
        let session = if node_id == 0 {
            None
        } else {
            cx.anchor.peer_session_id(node_id)
        };
        let provisional = node_id != 0 && cx.anchor.peer_is_provisional(node_id);
        ledger.record(
            WITNESSES[0],
            connected.ok && node_id != 0 && session.is_some(),
            format!(
                "connect() through @net-mesh/browser {} in {:.0} ms; the leaf reported node \
                 {node_hex}; the ANCHOR has a session for that node id: {session:?} \
                 (provisional={provisional}); no bootstrap protocol, no packet building and \
                 no handshake ran in the page — all of it is net-mesh-leaf{}",
                if connected.ok { "resolved" } else { "FAILED" },
                connected.elapsed_ms.unwrap_or(f64::NAN),
                connected
                    .error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
            ),
        );
    }

    // Nothing below can mean anything without a session.
    if !connected.ok || node_id == 0 {
        let why = format!(
            "no leaf session: {}",
            connected
                .error
                .unwrap_or_else(|| "connect() did not report a node id".into())
        );
        for name in &WITNESSES[1..] {
            ledger.record(name, false, why.clone());
        }
        let _ = cx.driver.close_page("leaf5-a").await;
        return Ok(());
    }

    // ================================================================
    // 2 — reliable round trips: 64 sequential, then 16 in flight
    //
    // `call()` is the leaf's nRPC client over a RELIABLE stream.
    //
    // The sequential leg proves the round trip on both sides at
    // once: the browser gets the echo body back for every call, and
    // the anchor's handler saw the bodies in the order they were
    // sent. It does NOT prove retransmission or reorder recovery —
    // awaiting each reply before sending the next never puts two
    // requests on the wire together, and a fire-and-forget stream on
    // a loss-free link satisfies exactly the same observations. That
    // property has its own witness, with real injected loss.
    //
    // The concurrent leg adds what sequencing hides. Sixteen calls
    // are issued before any is awaited, so sixteen pending entries
    // exist at once and every reply has to find its OWN caller.
    // `replies[i]` is the reply to `payloads[i]` by index, so a
    // reply handed to the wrong pending call is a mismatch here
    // rather than something arrival order papers over. The anchor's
    // handler must have run exactly sixteen times over exactly the
    // sixteen distinct bodies — once each, so a duplicated
    // completion is caught too.
    // ================================================================
    {
        let echoed = |sent: &String| {
            let mut v = b"echo:".to_vec();
            v.extend_from_slice(&unhex(sent));
            hex(&v)
        };

        echo_log.lock().expect("echo log").clear();
        let payloads: Vec<String> = (0..RELIABLE_ROUND_TRIPS)
            .map(|i| hex(format!("s5-reliable-{i:04}").as_bytes()))
            .collect();
        let r = script
            .run(
                "a",
                Step5::CallMany {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    payloads: payloads.clone(),
                    timeout_ms: 20_000,
                    concurrent: false,
                },
            )
            .await;
        let replies = r.replies.clone().unwrap_or_default();
        let replies_correct = replies.len() == payloads.len()
            && replies
                .iter()
                .zip(&payloads)
                .all(|(got, sent)| *got == echoed(sent));
        let delivered = echo_log.lock().expect("echo log").clone();
        let in_order = delivered.len() == payloads.len()
            && delivered
                .iter()
                .zip(&payloads)
                .all(|(got, sent)| *got == unhex(sent));

        // In flight together, on the same session.
        echo_log.lock().expect("echo log").clear();
        let burst: Vec<String> = (0..RELIABLE_IN_FLIGHT)
            .map(|i| hex(format!("s5-inflight-{i:04}-{:016x}", rand_u64()).as_bytes()))
            .collect();
        let c = script
            .run(
                "a",
                Step5::CallMany {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    payloads: burst.clone(),
                    timeout_ms: 20_000,
                    concurrent: true,
                },
            )
            .await;
        let burst_replies = c.replies.clone().unwrap_or_default();
        // Correlation, by index: reply i must be the echo of request
        // i. Every request body is distinct and carries a nonce, so
        // this cannot be satisfied by a reply that went to the wrong
        // pending call.
        let correlated = burst_replies.len() == burst.len()
            && burst_replies
                .iter()
                .zip(&burst)
                .all(|(got, sent)| *got == echoed(sent));
        let burst_seen = echo_log.lock().expect("echo log").clone();
        // Exactly once each, as a multiset: a duplicated handler
        // invocation fails this as surely as a missing one.
        let mut want: Vec<Vec<u8>> = burst.iter().map(|p| unhex(p)).collect();
        let mut got = burst_seen.clone();
        want.sort();
        got.sort();
        let handled_exactly_once = got == want;

        ledger.record(
            WITNESSES[1],
            r.ok && replies_correct && in_order && c.ok && correlated && handled_exactly_once,
            format!(
                "{RELIABLE_ROUND_TRIPS} SEQUENTIAL leaf nRPC round trips on one session: the \
                 browser received {} reply(ies) and every one carried `echo:` + its own \
                 request body={replies_correct}; the ANCHOR's handler ran {} time(s) and saw \
                 the bodies in exactly the order sent={in_order}{}. Then \
                 {RELIABLE_IN_FLIGHT} calls IN FLIGHT together on the same session (issued \
                 before any was awaited): {} reply(ies) returned and each matched its OWN \
                 request by index={correlated}; the anchor's handler saw exactly the \
                 {RELIABLE_IN_FLIGHT} distinct nonce bodies, once each={handled_exactly_once} \
                 (saw {} invocation(s)){}. What this does NOT establish: retransmission or \
                 reorder recovery — no loss is injected here and sequential loss-free calls \
                 are satisfied by a fire-and-forget stream too; the injected-loss reliable \
                 stream witness is the one that establishes that. ANCHOR STATE: {}",
                replies.len(),
                delivered.len(),
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                burst_replies.len(),
                burst_seen.len(),
                c.error
                    .as_deref()
                    .map(|e| format!("; concurrent error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 3 — nRPC to a native service, correlated
    //
    // The half Stage 4b explicitly did not have: the leaf's own nRPC
    // CLIENT. One call, one typed reply, and the native handler's own
    // record of the body — so a reply the wrapper synthesised
    // locally cannot pass.
    // ================================================================
    {
        echo_log.lock().expect("echo log").clear();
        let nonce = format!("s5-nrpc-{:016x}", rand_u64());
        let r = script
            .run(
                "a",
                Step5::Call {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    payload: hex(nonce.as_bytes()),
                    timeout_ms: 20_000,
                },
            )
            .await;
        let want = {
            let mut v = b"echo:".to_vec();
            v.extend_from_slice(nonce.as_bytes());
            hex(&v)
        };
        let reply_ok = r.reply.as_deref() == Some(want.as_str());
        let handler_saw = echo_log
            .lock()
            .expect("echo log")
            .iter()
            .any(|b| b.as_slice() == nonce.as_bytes());
        ledger.record(
            WITNESSES[2],
            r.ok && reply_ok && handler_saw,
            format!(
                "one `call({ECHO_SERVICE}, {nonce})` through the leaf's own nRPC client: the \
                 browser got the expected typed reply={reply_ok}; the native handler RAN with \
                 exactly that body={handler_saw}. A locally synthesised reply cannot satisfy \
                 the second half{}. ANCHOR STATE: {}",
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 4 — fire-and-forget under injected DataChannel loss
    //
    // The page's drop hook (`RTCDataChannel.prototype.send`) elides
    // every 4th outbound datagram. On a FIRE-AND-FORGET stream the
    // wire retains no retransmit descriptor, so an elided datagram
    // is genuinely lost. Three facts make the witness:
    //
    //   * the anchor's handler ran for the events that WERE sent and
    //     not for the elided ones — real loss, not a counter;
    //   * the LAST event arrived, so the receiver did not stall at
    //     the first sequence gap (the property that separates
    //     fire-and-forget from reliable);
    //   * the session survived, so loss is tolerated rather than
    //     escalated into a teardown.
    //
    // The payloads are nRPC REQUESTs built with the PRODUCTION
    // encoders on the native side and handed to `openStream(...)
    // .send()` verbatim, under the publish contract's own stream id
    // and channel hash — which is what makes a real handler, not a
    // frame counter, the observable.
    // ================================================================
    {
        sink_log.lock().expect("sink log").clear();
        sink_count.store(0, Ordering::SeqCst);
        // The leaf's own origin, read from the leaf — see the note
        // where it is captured. A payload built under any other
        // origin is dropped by the anchor before admission.
        let mut payloads = Vec::with_capacity(FAF_EVENTS);
        let mut bodies = Vec::with_capacity(FAF_EVENTS);
        let mut stream_id = String::new();
        let mut channel_hash = 0u16;
        for i in 0..FAF_EVENTS {
            let body = format!("s5-faf-{i:04}");
            let frame = rpc_request_frame(
                SINK_SERVICE,
                origin_hash,
                0x5000 + i as u64,
                body.as_bytes(),
            );
            stream_id = format!("{}", frame.stream_id);
            channel_hash = frame.channel_hash_u16;
            payloads.push(hex(&frame.payload));
            bodies.push(body);
        }
        let r = script
            .run(
                "a",
                Step5::StreamSend {
                    id: 0,
                    session: "main".into(),
                    reliable: false,
                    stream_id,
                    channel_hash,
                    payloads,
                    drop_every: FAF_DROP_EVERY,
                },
            )
            .await;
        let dropped = r.dropped.unwrap_or(0);
        // Settle: the surviving events are in flight.
        let expected = FAF_EVENTS as u64 - dropped;
        let _ = wait_for(
            || sink_count.load(Ordering::SeqCst) as u64 >= expected,
            Duration::from_secs(15),
        )
        .await;
        let seen = sink_log.lock().expect("sink log").clone();
        let seen_strings: Vec<String> = seen
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        // "The receiver did not stall at the first gap" is the
        // property fire-and-forget has and reliable does not, so it
        // is asserted as exactly that: the events AFTER the first
        // lost one arrived.
        //
        // It used to assert that the LAST event arrived, which this
        // arrangement can never satisfy: the hook elides every
        // `FAF_DROP_EVERY`-th outbound datagram and `FAF_EVENTS` is a
        // multiple of it, so the final event is always one of the
        // elided ones. The old criterion was unachievable by
        // construction, not a defect in the path under test.
        let first_gap = bodies.iter().position(|b| !seen_strings.contains(b));
        let arrived_after_gap = first_gap.map_or(0, |gap| {
            bodies[gap + 1..]
                .iter()
                .filter(|b| seen_strings.contains(b))
                .count()
        });
        let lost: Vec<&String> = bodies
            .iter()
            .filter(|b| !seen_strings.contains(b))
            .collect();
        let session_alive = cx.anchor.peer_session_id(node_id).is_some();
        // The drop hook may also elide a control datagram, so the
        // arithmetic is a bound, not an equality: at least
        // `FAF_EVENTS - dropped` events must arrive and at least one
        // must be missing, because a run in which NOTHING was lost
        // proves nothing about loss tolerance.
        let pass = r.ok
            && origin_hash != 0
            && dropped > 0
            && !lost.is_empty()
            && seen_strings.len() as u64 >= expected.saturating_sub(1)
            && arrived_after_gap > 0
            && session_alive;
        ledger.record(
            WITNESSES[3],
            pass,
            format!(
                "{FAF_EVENTS} nRPC REQUEST events on ONE fire-and-forget stream \
                 (reliability=fireAndForget → no retransmit descriptor retained), with the \
                 page's RTCDataChannel.send hook eliding every {FAF_DROP_EVERY}th outbound \
                 datagram: {dropped} datagram(s) elided; the anchor's REAL handler ran {} \
                 time(s); {} event(s) never arrived ({}), so the loss is observed as missing \
                 handler invocations rather than a counter; {arrived_after_gap} event(s) sent \
                 AFTER the first gap ({:?}) still arrived, so the receiver did not stall at \
                 that gap — which is the property fire-and-forget has and a reliable stream \
                 does not; \
                 the session survived the loss={session_alive}{}. The payloads were encoded \
                 under the LEAF's origin {origin_hex:?} (not its node id {node_hex:?}), which \
                 is what the anchor checks an EventMeta origin against — a leaf surface that \
                 does not report its origin leaves this 0 and fails this witness rather than \
                 sending frames that are dropped before admission. ANCHOR STATE: {}",
                seen_strings.len(),
                lost.len(),
                lost.iter()
                    .take(6)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                first_gap.map(|g| bodies[g].clone()),
                r.error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 4b — the REAL stream ABI, native → leaf and back (R11 / E6)
    //
    // Everything in this block is a call through the BUILT
    // `@net-mesh/browser` the page loaded from `/browser/index.js`,
    // against this process's real `MeshNode` over real WebRTC. No
    // inner stream object is constructed anywhere: the page calls
    // `openStream(...)` and the object it gets back is whatever the
    // package returns, with its `onMessage` callback AND its
    // `for await` async iterator both attached before a byte can
    // arrive.
    //
    // The four things it establishes, each its own witness because
    // each fails for its own reason:
    //
    //   * bytes the ANCHOR put on a stream reach both consumers of
    //     the package's `LeafStream`, in order and byte-exact, and
    //     the options the page asked for are the options the stream
    //     actually has (read from the stream, and from the built
    //     wasm's own option parser on the same object);
    //   * eight windows' worth of native-originated traffic gets
    //     through a deliberately small credit window, which is only
    //     possible if the browser leaf kept granting credit back —
    //     the grant round trip, counted on the sender;
    //   * a RELIABLE stream recovers real injected loss AND real
    //     injected reorder, observed as the native handler seeing
    //     every event in the order sent (the fire-and-forget witness
    //     above loses them, which is the contrast that makes this
    //     mean something);
    //   * a 96 KiB body fragments and reassembles in BOTH directions
    //     through the public API.
    // ================================================================
    {
        // ---- the stream the page holds open for this whole block --
        let direct_open = script
            .run(
                "a",
                Step5::StreamOpen {
                    id: 0,
                    session: "main".into(),
                    handle: "direct".into(),
                    reliable: true,
                    label: Some("abi-direct".into()),
                    stream_id: Some(ABI_DIRECT_STREAM_ID.to_string()),
                    channel_hash: None,
                },
            )
            .await;
        let want_direct_hex = format!("{ABI_DIRECT_STREAM_ID:016x}");
        let reported_id = stat_str(&direct_open, "stream_id");
        let reported_reliability = stat_str(&direct_open, "reliability");
        let effective = direct_open
            .stats
            .as_ref()
            .and_then(|s| s.get("effective_options"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // The stream's OWN answers, not the options object: a
        // package that silently dropped `streamId` would open a
        // node-allocated id and fail here.
        let handle_matches = reported_id == format!("\"{want_direct_hex}\"")
            && reported_reliability == "\"reliable\"";
        // The built wasm's own parser, on the object the page built.
        // This half is a PARSER reading, and on its own it proves
        // only that the artifact understands the options — the page
        // calls `effective_stream_options` itself, with
        // `BrowserNode.openStream` nowhere in between.
        let options_parsed = effective.get("streamId").and_then(|v| v.as_str())
            == Some(want_direct_hex.as_str())
            && effective.get("reliability").and_then(|v| v.as_str()) == Some("reliable")
            && effective.get("label").and_then(|v| v.as_str()) == Some("abi-direct");
        // FORWARDING, observed at the INNER call: the object the
        // production wrapper actually handed to
        // `LeafNode.open_stream`, recorded by the page from inside
        // that call. `streamId` and `reliability` are corroborated
        // downstream by the stream's own answers, but the LABEL is
        // not observable anywhere else — a wrapper that dropped only
        // `label` would satisfy `options_parsed`, `handle_matches`
        // and every byte of traffic below. So the label is required
        // HERE, at the seam, and the absence of a recording is a
        // failure rather than a pass: this is the direct surface, so
        // the inner call is synchronous and must have been seen.
        let forwarded = direct_open
            .stats
            .as_ref()
            .and_then(|s| s.get("forwarded_options"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // The id is compared BY VALUE, not by spelling. The page asks
        // in hex and the production wrapper forwards the same u64 as
        // a decimal string (u64s stay off the JS number path), so a
        // string comparison here would fail on a correct forward —
        // and "the witness disagreed about base" is not a defect
        // worth reporting as one.
        let forwarded_id = forwarded
            .get("stream_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok());
        let want_direct_id = u64::from_str_radix(&want_direct_hex, 16).ok();
        let label_forwarded_inner = forwarded.get("label").and_then(|v| v.as_str())
            == Some("abi-direct")
            && forwarded_id.is_some()
            && forwarded_id == want_direct_id
            && forwarded.get("reliability").and_then(|v| v.as_str()) == Some("reliable");
        let options_forwarded = options_parsed && label_forwarded_inner;
        let direct_is_direct = direct_open
            .stats
            .as_ref()
            .and_then(|s| s.get("proxied"))
            .and_then(serde_json::Value::as_bool)
            == Some(false);

        // ---- the anchor's side of the same id --------------------
        let direct_native = cx.anchor.open_stream(
            node_id,
            ABI_DIRECT_STREAM_ID,
            StreamConfig::new().with_reliability(Reliability::Reliable),
        );
        let direct_seed = 0x2100 ^ (rand_u64() & 0xFFFF);
        let mut native_send_error: Option<String> = None;
        match &direct_native {
            Ok(stream) => {
                for i in 0..ABI_DIRECT_EVENTS {
                    let payload = Bytes::from(gen_bytes(direct_seed + i as u64, ABI_DIRECT_SIZE));
                    if let Err(e) = cx
                        .anchor
                        .send_with_retry(stream, std::slice::from_ref(&payload), 40)
                        .await
                    {
                        native_send_error = Some(format!("payload {i}: {e}"));
                        break;
                    }
                }
            }
            Err(e) => native_send_error = Some(e.to_string()),
        }

        let direct_inbox = script
            .run(
                "a",
                Step5::StreamInbox {
                    id: 0,
                    handle: "direct".into(),
                    expect: ABI_DIRECT_EVENTS,
                    timeout_ms: 30_000,
                },
            )
            .await;
        let want_direct = expected_marks(direct_seed, ABI_DIRECT_SIZE, ABI_DIRECT_EVENTS);
        let callback_got = marks(&direct_inbox, "callback");
        let iterator_got = marks(&direct_inbox, "iterator");
        let callback_exact = callback_got == want_direct;
        let iterator_exact = iterator_got == want_direct;
        // WHERE a short list went, printed with the verdict. The CI
        // row that lost one of these twelve payloads carried no
        // counters at all, so "11 of 12 arrived" could not be told
        // apart from "the sender gave up", "the receive half was
        // reset", "the retransmit arrived and was deduped" or "the
        // bytes never left" without a local reproduction. All four
        // are distinguishable from these three readings: the leaf's
        // own drop counters, how long the wait actually took, and
        // the ANCHOR's reliable-stream ledger — whether it still
        // owned the packet, what the peer acknowledged, and which
        // sequence (if any) it abandoned.
        let direct_counters = stat_str(&direct_inbox, "counters");
        let direct_waited = stat_u64(&direct_inbox, "waited_ms");
        let direct_terminal = stat_str(&direct_inbox, "iterator_ended");
        let direct_sender = sender_ledger(cx.anchor, node_id, ABI_DIRECT_STREAM_ID);

        ledger.record(
            WITNESSES[9],
            direct_open.ok
                && direct_is_direct
                && handle_matches
                && options_forwarded
                && native_send_error.is_none()
                && callback_exact
                && iterator_exact,
            format!(
                "A stream opened through the BUILT package's public API on the DIRECT \
                 `connect()` object (open ok={}, the package returned a synchronous handle \
                 so this really is the direct surface={direct_is_direct}); nothing is \
                 hand-constructed — `LeafStream`'s inner object is the wasm stream \
                 `LeafNode.open_stream` returned. The stream reports \
                 streamId={reported_id} reliability={reported_reliability}, which is the \
                 id and mode the page ASKED for ({want_direct_hex}, \
                 reliable)={handle_matches}. OPTIONS, in two separate readings, because \
                 the first one alone was overbroad: the built wasm's own \
                 `effective_stream_options` on the object the page built reads back \
                 {effective} ({options_parsed}) — that is a PARSER reading, taken with \
                 `BrowserNode.openStream` nowhere in between, so a wrapper that dropped \
                 `label` on the way in would leave it intact. The forwarding itself is \
                 observed AT THE INNER CALL: the page records the object the production \
                 wrapper actually hands `LeafNode.open_stream`, from inside that call, \
                 and it read {forwarded} — label, id and mode all as asked \
                 ({label_forwarded_inner}). The label is the option that needs this: id \
                 and mode are corroborated by the stream's own answers and by the traffic \
                 below, while nothing downstream carries the label at all. No recording \
                 FAILS the leg — this is the synchronous direct surface, so the inner \
                 call must have been seen (forwarded={options_forwarded}). \
                 This process's `MeshNode` then opened the \
                 SAME stream id from its side and sent {ABI_DIRECT_EVENTS} payload(s) of \
                 {ABI_DIRECT_SIZE} B generated from seed {direct_seed:#x} (native send \
                 error: {native_send_error:?}). The package's `onMessage` CALLBACK \
                 received {} payload(s), byte-exact and in order={callback_exact}; its \
                 `for await` ASYNC ITERATOR received {} payload(s), byte-exact and in \
                 order={iterator_exact} — both consumers were attached at open, before a \
                 byte could arrive, so this is the package's fan-out and not whichever \
                 registered first. Comparison is length + FNV-1a/32 per payload against \
                 the generator the runner and the page both implement. The wait for those \
                 payloads is a CONDITION, not a nap — it ended as soon as both consumers \
                 held {ABI_DIRECT_EVENTS}, or at its 30 s ceiling, or early if the stream \
                 went terminal — and it took {direct_waited} ms with \
                 iterator_ended={direct_terminal}. WHERE A SHORT LIST WENT, so the next \
                 occurrence is diagnosable from this line alone: the LEAF's own counters \
                 at judgement were {direct_counters}, and the ANCHOR's ledger for this \
                 stream was {direct_sender}. A payload dropped at the leaf moves one of \
                 those drop counters; a sender that gave up names the sequence it \
                 abandoned; a sender that still owns the packet has pending=true with the \
                 peer's acknowledgement frontier short of what it issued. ANCHOR STATE: {}",
                direct_open.ok,
                callback_got.len(),
                iterator_got.len(),
                peer_state(cx.anchor, node_id),
            ),
        );

        // ---- sustained native → leaf past the credit window ------
        let window_open = script
            .run(
                "a",
                Step5::StreamOpen {
                    id: 0,
                    session: "main".into(),
                    handle: "window".into(),
                    reliable: true,
                    label: Some("abi-window".into()),
                    stream_id: Some(ABI_WINDOW_STREAM_ID.to_string()),
                    channel_hash: None,
                },
            )
            .await;
        let window_native = cx.anchor.open_stream(
            node_id,
            ABI_WINDOW_STREAM_ID,
            StreamConfig::new()
                .with_reliability(Reliability::Reliable)
                .with_window_bytes(ABI_WINDOW_BYTES),
        );
        let window_seed = 0x3300 ^ (rand_u64() & 0xFFFF);
        let mut window_error: Option<String> = None;
        match &window_native {
            Ok(stream) => {
                for i in 0..ABI_WINDOW_EVENTS {
                    let payload = Bytes::from(gen_bytes(window_seed + i as u64, ABI_WINDOW_SIZE));
                    if let Err(e) = cx
                        .anchor
                        .send_with_retry(stream, std::slice::from_ref(&payload), 200)
                        .await
                    {
                        window_error = Some(format!("payload {i}: {e}"));
                        break;
                    }
                }
            }
            Err(e) => window_error = Some(e.to_string()),
        }
        let window_inbox = script
            .run(
                "a",
                Step5::StreamInbox {
                    id: 0,
                    handle: "window".into(),
                    expect: ABI_WINDOW_EVENTS,
                    timeout_ms: 60_000,
                },
            )
            .await;
        let want_window = expected_marks(window_seed, ABI_WINDOW_SIZE, ABI_WINDOW_EVENTS);
        let window_callback = marks(&window_inbox, "callback");
        let window_iterator = marks(&window_inbox, "iterator");
        let window_delivered = window_callback == want_window && window_iterator == want_window;
        // The sender's own ledger. `credit_grants_received` is the
        // observable the criterion is named after: without a grant
        // round trip the transfer stops at the first window and the
        // retry loop exhausts.
        let window_stats = cx.anchor.stream_stats(node_id, ABI_WINDOW_STREAM_ID);
        let offered = (ABI_WINDOW_EVENTS * ABI_WINDOW_SIZE) as u64;
        let grants = window_stats.map_or(0, |s| s.credit_grants_received);
        let sent_bytes = window_stats.map_or(0, |s| s.tx_bytes_sent);
        let window_bytes = window_stats.map_or(0, |s| u64::from(s.tx_window));
        let backpressure = window_stats.map_or(0, |s| s.backpressure_events);
        let consumed = window_stats.map_or(0, |s| s.max_consumed_seen);
        let credit_left = window_stats.map_or(0, |s| u64::from(s.tx_credit_remaining));
        // Conservation, at a settled point: credit + in-flight ==
        // the window.
        let conserved =
            window_bytes > 0 && credit_left + sent_bytes.saturating_sub(consumed) == window_bytes;
        let past_the_window = sent_bytes > window_bytes;

        ledger.record(
            WITNESSES[10],
            window_open.ok
                && window_error.is_none()
                && window_delivered
                && grants > 0
                && past_the_window
                && conserved,
            format!(
                "SUSTAINED NATIVE → LEAF TRAFFIC PAST THE CREDIT WINDOW. The anchor opened \
                 stream {ABI_WINDOW_STREAM_ID:#018x} with a deliberately small \
                 `window_bytes = {ABI_WINDOW_BYTES}` and pushed {ABI_WINDOW_EVENTS} × \
                 {ABI_WINDOW_SIZE} B = {offered} B through it — {:.1}× the window, so the \
                 sender's initial credit is exhausted and the transfer can only finish if \
                 the BROWSER LEAF kept emitting `StreamWindow` grants and they kept \
                 arriving (page open ok={}, native send error: {window_error:?}). The \
                 sender's own ledger afterwards: credit_grants_received={grants} (this is \
                 the grant ROUND TRIP, counted where it lands), \
                 tx_bytes_sent={sent_bytes} > tx_window={window_bytes}={past_the_window}, \
                 backpressure_events={backpressure} (the typed, bounded exhaustion the \
                 retry loop rode), max_consumed_seen={consumed}, \
                 tx_credit_remaining={credit_left}; byte conservation \
                 `credit + (sent - consumed) == window` holds={conserved}. On the page, the \
                 package's callback and iterator each received all {ABI_WINDOW_EVENTS} \
                 payloads byte-exact and in order={window_delivered} (callback {}, \
                 iterator {}). ANCHOR STATE: {}",
                offered as f64 / f64::from(ABI_WINDOW_BYTES),
                window_open.ok,
                window_callback.len(),
                window_iterator.len(),
                peer_state(cx.anchor, node_id),
            ),
        );

        // ---- reliable recovery from injected loss AND reorder ----
        sink_log.lock().expect("sink log").clear();
        sink_count.store(0, Ordering::SeqCst);
        let mut recover_frames = Vec::with_capacity(ABI_RELIABLE_EVENTS);
        let mut recover_bodies = Vec::with_capacity(ABI_RELIABLE_EVENTS);
        let mut recover_stream_id = String::new();
        let mut recover_channel = 0u16;
        let mut recover_stream_num = 0u64;
        for i in 0..ABI_RELIABLE_EVENTS {
            let body = format!("s5-recover-{i:04}");
            let frame = rpc_request_frame(
                SINK_SERVICE,
                origin_hash,
                0x5B00 + i as u64,
                body.as_bytes(),
            );
            recover_stream_id = format!("{}", frame.stream_id);
            recover_channel = frame.channel_hash_u16;
            recover_stream_num = frame.stream_id;
            recover_frames.push(hex(&frame.payload));
            recover_bodies.push(body);
        }
        // The ANCHOR opens its side of the same stream, as a real
        // peer does. Without it the receiving session has no
        // per-stream reliability state to acknowledge or NACK
        // against, so the sender's retransmit has nothing to react
        // to and "the loss was not recovered" would be a fact about
        // the harness rather than about the path.
        let recover_native = cx
            .anchor
            .open_stream(
                node_id,
                recover_stream_num,
                StreamConfig::new().with_reliability(Reliability::Reliable),
            )
            .map_err(|e| e.to_string());
        // WHERE THIS WITNESS MEASURES ORDER, AND WHY IT MOVED.
        //
        // Net's ordering contract is on DELIVERY: one reliable
        // stream is released in sequence order, and the nRPC serve
        // bridge drains its inbound receiver from a single task and
        // disposes of each frame before it looks at the next. So the
        // sequence the bridge hands to the fold IS the order that
        // stream delivered, and this observation records exactly
        // that hand-off, inline, in the bridge's own task.
        //
        // It is NOT handler entry order, and it used to be. The fold
        // spawns one task per call ON PURPOSE — a slow or parked
        // handler must not hold its source's successors, which
        // `server_fold_runs_one_sources_handlers_concurrently` and
        // `a_long_first_poll_does_not_delay_its_sources_successors`
        // both pin — so when each handler BODY starts belongs to the
        // scheduler and nothing else. An earlier round chained
        // handler entry per source and added this leg against that
        // chain; the chain was removed (it raced neither
        // cancellation nor the deadline, and its queue was
        // unbounded), and measuring a withdrawn mechanism would
        // quietly re-impose it. Measured at a handler, 40 requests
        // delivered in order enter as e.g. 1, 0, 2, 4, 3, 14, 21, 6
        // — the scheduler, not the transport.
        //
        // Everything else this leg asserted is unchanged and still
        // asserted below: all 40 bodies arrive, byte-exact, exactly
        // once, settled inside the window.
        let handoff: Arc<std::sync::Mutex<Vec<u64>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let sink = Arc::clone(&handoff);
            cx.anchor.set_rpc_dispatch_observer_for_test(Arc::new(
                move |_service, _from, frame| {
                    if let Some(meta) = EventMeta::from_bytes(frame) {
                        sink.lock().expect("handoff log").push(meta.seq_or_ts);
                    }
                },
            ));
        }
        let recover_open = script
            .run(
                "a",
                Step5::StreamOpen {
                    id: 0,
                    session: "main".into(),
                    handle: "recover".into(),
                    reliable: true,
                    label: Some("abi-recover".into()),
                    stream_id: Some(recover_stream_id.clone()),
                    channel_hash: Some(recover_channel),
                },
            )
            .await;
        let recover_write = script
            .run(
                "a",
                Step5::StreamWrite {
                    id: 0,
                    handle: "recover".into(),
                    frames: recover_frames,
                    seed: 0,
                    size: 0,
                    count: 0,
                    drop_every: ABI_RELIABLE_DROP_EVERY,
                    reorder_every: ABI_RELIABLE_REORDER_EVERY,
                },
            )
            .await;
        let dropped = stat_u64(&recover_write, "dropped");
        let reordered = stat_u64(&recover_write, "reordered");
        // Recovery is not instant: the leaf's RTO sweep runs on the
        // 50 ms tick and the peer's NACKs have to get back.
        let recovered = wait_for(
            || sink_count.load(Ordering::SeqCst) >= ABI_RELIABLE_EVENTS,
            Duration::from_secs(45),
        )
        .await;
        let recover_seen: Vec<String> = sink_log
            .lock()
            .expect("sink log")
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        let all_arrived = recover_bodies
            .iter()
            .all(|body| recover_seen.contains(body));
        // DELIVERY order, at the hand-off the observation above
        // records: the call ids the bridge gave the fold, for this
        // burst only (its ids are `0x5B00 + i`; the fire-and-forget
        // witness above used `0x5000 + i` on the same service). Each
        // has to appear, once, strictly ascending — the wire order
        // was scrambled and eight datagrams were elided, so this is
        // the statement that the receiver put the stream back into
        // sequence order and the retransmit filled the holes before
        // anything was handed up.
        let handed: Vec<u64> = handoff
            .lock()
            .expect("handoff log")
            .iter()
            .copied()
            .filter(|id| (0x5B00..0x5B00 + ABI_RELIABLE_EVENTS as u64).contains(id))
            .collect();
        let want: Vec<u64> = (0..ABI_RELIABLE_EVENTS as u64)
            .map(|i| 0x5B00 + i)
            .collect();
        let delivered_in_order = handed == want;
        let exactly_once = recover_bodies
            .iter()
            .all(|body| recover_seen.iter().filter(|s| *s == body).count() == 1);
        // The leaf's own counters at the moment of judgement. A
        // short list has to be ATTRIBUTABLE: `stream_failed` /
        // `retransmits_exhausted` means the sender gave up,
        // `duplicate_sequence` means the retransmit did arrive, and
        // nothing moving at all means the events never left.
        let recover_counters = script
            .run(
                "a",
                Step5::StreamInbox {
                    id: 0,
                    handle: "recover".into(),
                    expect: 0,
                    timeout_ms: 1_000,
                },
            )
            .await;
        let leaf_counters = stat_str(&recover_counters, "counters");

        ledger.record(
            WITNESSES[11],
            recover_open.ok
                && recover_write.ok
                && dropped > 0
                && reordered > 0
                && recovered
                && all_arrived
                && delivered_in_order
                && exactly_once,
            format!(
                "DELIBERATE RELIABLE LOSS **AND** REORDER, recovered. \
                 {ABI_RELIABLE_EVENTS} natively encoded nRPC REQUEST events were sent on ONE \
                 stream opened `reliability: 'reliable'` through the package's public API \
                 (open ok={}, write ok={}), with two transport hooks armed together on the \
                 page: every {ABI_RELIABLE_DROP_EVERY}th outbound datagram ELIDED, and \
                 every {ABI_RELIABLE_REORDER_EVERY}th held back and submitted AFTER the \
                 datagram behind it — a real Net-level reorder on a DataChannel that is \
                 ordered and reliable and therefore cannot produce one by itself. Both \
                 hooks are required to have fired: {dropped} datagram(s) elided (>0={}) and \
                 {reordered} pair(s) swapped (>0={}); a run in which nothing was lost or \
                 nothing was reordered proves nothing and FAILS here. The anchor's REAL \
                 handler then saw {} invocation(s) within 45 s (settled={recovered}); every \
                 one of the {ABI_RELIABLE_EVENTS} bodies arrived={all_arrived}, each \
                 exactly once={exactly_once}, and the nRPC serve bridge handed the fold \
                 all {ABI_RELIABLE_EVENTS} call ids strictly ascending in send \
                 order={delivered_in_order} ({} handed over) — so the receiver reassembled \
                 the scrambled wire order back into sequence order and the sender's \
                 retransmit replaced what was elided BEFORE anything reached the \
                 application. That hand-off is where the ordering contract lives: per \
                 stream DELIVERY order, one bridge task, each frame disposed of before the \
                 next. Handler ENTRY order is deliberately NOT ordered — the fold spawns \
                 one task per call so a slow handler cannot hold its source's successors — \
                 so this leg measures the bridge, not the scheduler. \
                 The fire-and-forget witness above, on the same \
                 path with only the drop hook, LOSES those events; that contrast is what \
                 makes this a reliability result rather than a link that happened to be \
                 clean. The ANCHOR had opened its side of the same stream \
                 ({recover_native:?}), so the receiver holds reliability state to \
                 acknowledge and NACK against. LEAF COUNTERS at judgement: \
                 {leaf_counters}. ANCHOR STATE: {}",
                recover_open.ok,
                recover_write.ok,
                dropped > 0,
                reordered > 0,
                recover_seen.len(),
                handed.len(),
                peer_state(cx.anchor, node_id),
            ),
        );

        // ---- large messages, both directions ---------------------
        //
        // WHAT THIS MEASURES, AND WHY IT IS NOT "96 KiB WORKS".
        //
        // §9.2 of the report states the payload-size interoperability
        // this stage has and the two it does NOT: no native node
        // reassembles leaf fragments, so a leaf → native payload
        // above `MAX_EVENT_SIZE` (8 104 B) arrives as N partial
        // events and is not put back together; and above 64 832 B
        // the leaf does not even attempt it, returning a typed
        // `LeafError::Wire` that names streams as the way out —
        // never a truncation and never a silent drop.
        //
        // A witness that asserted a 96 KiB `call()` round trip would
        // therefore be asserting a property the stage explicitly
        // disclaims — measuring the wrong layer, and failing for a
        // reason that is documentation rather than defect. So the
        // four legs here are the four things that ARE contracts:
        //
        //   1. at the ceiling, the round trip is byte-exact in both
        //      directions through the public `call()`;
        //   2. past the hard limit, the refusal is TYPED and the
        //      anchor's handler is never entered — no truncation,
        //      no partial delivery, no silence;
        //   3. native → leaf on a stream AT the ceiling, byte-exact
        //      on both of the package's consumers;
        //   4. native → leaf on a stream ABOVE `MAX_EVENT_SIZE`, the
        //      symmetric half of §9.2: the sender refuses with a
        //      typed `StreamError::EventTooLarge` that names the
        //      limit, and the payload never appears. `Ok` plus
        //      silence was the defect; a truncation would be worse.
        echo_log.lock().expect("echo log").clear();
        let ceiling_seed = 0x4400 ^ (rand_u64() & 0xFFFF);
        let ceiling_body = gen_bytes(ceiling_seed, ABI_CEILING_SIZE);
        let mut ceiling_reply = b"echo:".to_vec();
        ceiling_reply.extend_from_slice(&ceiling_body);
        let want_request = Mark::of(&ceiling_body);
        let want_reply = Mark::of(&ceiling_reply);
        let sized = script
            .run(
                "a",
                Step5::CallSized {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    seed: ceiling_seed,
                    size: ABI_CEILING_SIZE,
                    timeout_ms: 60_000,
                },
            )
            .await;
        let got_request = sized
            .stats
            .as_ref()
            .and_then(|s| s.get("request"))
            .and_then(|v| serde_json::from_value::<Mark>(v.clone()).ok());
        let got_reply = sized
            .stats
            .as_ref()
            .and_then(|s| s.get("reply"))
            .and_then(|v| serde_json::from_value::<Mark>(v.clone()).ok());
        // The native handler's own record of the body it was given:
        // a reply the wrapper synthesised locally cannot satisfy it.
        let handler_got_ceiling = echo_log
            .lock()
            .expect("echo log")
            .iter()
            .any(|b| Mark::of(b) == want_request);
        let request_up = got_request == Some(want_request) && handler_got_ceiling;
        let reply_down = got_reply == Some(want_reply);

        // Leg 2: past the hard limit. A TYPED refusal, and the
        // handler must not have run — a truncated body reaching it
        // is the outcome §9.2 says can never happen, and it would
        // show up here as an extra echo-log entry.
        echo_log.lock().expect("echo log").clear();
        let over_seed = 0x4800 ^ (rand_u64() & 0xFFFF);
        let oversized = script
            .run(
                "a",
                Step5::CallSized {
                    id: 0,
                    session: "main".into(),
                    service: ECHO_SERVICE.into(),
                    seed: over_seed,
                    size: ABI_OVER_LIMIT_SIZE,
                    timeout_ms: 30_000,
                },
            )
            .await;
        let over_kind = oversized.kind.clone().unwrap_or_default();
        let over_message = oversized.message.clone().unwrap_or_default();
        // Settle, then read: a body that was going to arrive
        // truncated would have arrived by now.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let handler_untouched = echo_log.lock().expect("echo log").is_empty();
        // The EXACT refusal, not "some error happened".
        //
        // `!over_kind.is_empty()` was satisfied by ANY failure that
        // reached the page before dispatch — a deadline, a session
        // replacement, a leader loss — and every one of those also
        // leaves the handler untouched, so this leg could stay green
        // while the over-cap refusal it names had stopped happening
        // altogether. The no-delivery half was never the weak half;
        // the classifier was.
        //
        // The leaf-side identity of this refusal is
        // `LeafError::Wire` (kind `"wire"`, the browser package's
        // `WireError`) raised by `frame::split_payload`, whose text
        // names the framed length and the fragmentation ceiling.
        // Note it is NOT the `EventTooLarge` kind: that is the
        // NATIVE sender's `StreamError` variant, measured against
        // `MAX_EVENT_SIZE`, and leg 3b below requires it by value.
        // The leaf refuses EARLIER and for a different reason — at
        // `ABI_LEAF_FRAG_CEILING`, past which it will not even
        // attempt the eight-fragment split — so there is no
        // `EventTooLarge` kind on the leaf boundary to require here.
        // Requiring the ceiling clause is the equivalent exactness.
        //
        // The named byte count must also be OUR payload's: a
        // refusal about some smaller body would name a smaller
        // number, and the framed length is the request body plus its
        // nRPC framing, hence `>=` rather than `==` (pinning the
        // framing overhead would make this leg fail on an unrelated
        // header change).
        let over_named_bytes = refused_payload_bytes(&over_message);
        let refused_typed = !oversized.ok
            && over_kind == "wire"
            && over_message.contains(&format!(
                "exceeds the {ABI_LEAF_FRAG_CEILING}-byte fragmentation ceiling"
            ))
            && over_message.contains("use a stream")
            && over_named_bytes.is_some_and(|n| n >= ABI_OVER_LIMIT_SIZE)
            && handler_untouched;

        // Leg 3a, GATED: native → leaf on a stream at the same
        // ceiling, so "both directions" covers the stream API and
        // not only nRPC.
        //
        // Leg 3b, now GATED as well: the same send at 32 KiB, over
        // the per-event cap. This leg was RECORDED for one round
        // because §9.2 stated the leaf → native direction only and
        // gating a contract nobody had written would have been
        // asserting an assumption. The measurement came back
        // `Ok` + nothing delivered, and that is now repaired: the
        // native sender refuses an event above `MAX_EVENT_SIZE`
        // with a typed `StreamError::EventTooLarge` that NAMES the
        // limit, before any packet of the call reaches the wire.
        // Fragmentation was the alternative and was rejected on the
        // merits: `send_on_stream` is transport-agnostic, only the
        // browser leaf and the native RTC ingress reassemble, and
        // every other receive path reads into a `MAX_PACKET_SIZE`
        // buffer — so fragmenting here would hand a native peer's
        // application N partial events, trading a silent drop for a
        // silent corruption. The contract now reads the same in both
        // directions (§9.2), so this leg gates.
        //
        // BOTH legs ride a stream of their OWN
        // (`ABI_LARGE_STREAM_ID`), opened here, used by nothing
        // else. They used to share the direct witness's stream and
        // identify their payloads by INDEX — "everything after the
        // twelve that stream already carried" — which made this
        // verdict a function of another leg's arrival count in both
        // directions: a payload that arrived LATE there would have
        // shifted the index and leaked into `extra` here, and a
        // payload that never arrived there shifted it the other way
        // and hid the ceiling payload inside the skipped prefix, so
        // this leg reported "something else also arrived" about a
        // stream on which nothing had. On its own stream the
        // payloads are its own from sequence zero, `extra` is
        // literally everything that arrived, and no arithmetic
        // stands between the observation and the claim.
        let large_open = script
            .run(
                "a",
                Step5::StreamOpen {
                    id: 0,
                    session: "main".into(),
                    handle: "large".into(),
                    reliable: true,
                    label: Some("abi-large".into()),
                    stream_id: Some(ABI_LARGE_STREAM_ID.to_string()),
                    channel_hash: None,
                },
            )
            .await;
        let large_native = cx.anchor.open_stream(
            node_id,
            ABI_LARGE_STREAM_ID,
            StreamConfig::new().with_reliability(Reliability::Reliable),
        );
        let stream_ceiling = gen_bytes(over_seed ^ 0x3C3C, ABI_CEILING_SIZE);
        let want_stream_ceiling = Mark::of(&stream_ceiling);
        let stream_large = gen_bytes(over_seed ^ 0x5A5A, ABI_STREAM_LARGE_SIZE);
        let want_stream_large = Mark::of(&stream_large);
        let mut ceiling_send_error: Option<String> = None;
        let mut stream_large_error: Option<String> = None;
        // The TYPED refusal, not its Display form: a stringly-typed
        // check would pass on any error at all, including the
        // transport faults this leg must not accept.
        let mut stream_large_refusal: Option<(usize, usize)> = None;
        if let Ok(stream) = &large_native {
            let at_ceiling = Bytes::from(stream_ceiling.clone());
            if let Err(e) = cx
                .anchor
                .send_with_retry(stream, std::slice::from_ref(&at_ceiling), 200)
                .await
            {
                ceiling_send_error = Some(e.to_string());
            }
            let over = Bytes::from(stream_large.clone());
            if let Err(e) = cx
                .anchor
                .send_with_retry(stream, std::slice::from_ref(&over), 200)
                .await
            {
                stream_large_error = Some(e.to_string());
                if let StreamError::EventTooLarge { size, limit } = e {
                    stream_large_refusal = Some((size, limit));
                }
            }
        } else {
            ceiling_send_error = Some("the anchor never opened the large-message stream".into());
            stream_large_error = ceiling_send_error.clone();
        }
        // Exactly ONE payload is expected on this stream, so the wait
        // ends as soon as it arrives — a condition, not a nap, and a
        // ceiling that still FAILS if it never comes. The settle
        // below is what catches a regression that put the over-cap
        // payload on the wire after all: a second read once the link
        // has gone quiet.
        let ceiling_wait = script
            .run(
                "a",
                Step5::StreamInbox {
                    id: 0,
                    handle: "large".into(),
                    expect: 1,
                    timeout_ms: 30_000,
                },
            )
            .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let large_inbox = script
            .run(
                "a",
                Step5::StreamInbox {
                    id: 0,
                    handle: "large".into(),
                    expect: 1,
                    timeout_ms: 5_000,
                },
            )
            .await;
        let extra = marks(&large_inbox, "callback");
        let extra_iter = marks(&large_inbox, "iterator");
        let large_counters = stat_str(&large_inbox, "counters");
        let large_waited = stat_u64(&ceiling_wait, "waited_ms");
        let large_terminal = stat_str(&large_inbox, "iterator_ended");
        let large_sender = sender_ledger(cx.anchor, node_id, ABI_LARGE_STREAM_ID);
        // GATED: the ceiling payload must be the FIRST — and, with
        // this stream carrying nothing else, the only — payload to
        // arrive, byte-exact, on both consumers.
        let large_open_ok = large_open.ok;
        let stream_down = large_open_ok
            && ceiling_send_error.is_none()
            && extra.first() == Some(&want_stream_ceiling)
            && extra_iter.first() == Some(&want_stream_ceiling);
        // GATED: the over-cap send is refused TYPED, the error names
        // BOTH the offending size and the limit, and nothing beyond
        // the ceiling payload ever arrives on either consumer. An
        // `Ok` with nothing delivered, a truncation, and a late
        // delivery each fail here.
        let over_cap_refused = stream_large_refusal
            == Some((ABI_STREAM_LARGE_SIZE, MAX_EVENT_SIZE))
            && extra.len() == 1
            && extra_iter.len() == 1;
        // Each branch says what was actually OBSERVED. The old
        // wording claimed "something else also arrived" for every
        // failure that still had the typed refusal, so the round
        // where NOTHING arrived — the ceiling payload included —
        // was reported as a surplus when it was a shortfall, and
        // read as a defect in the refusal rather than in delivery.
        let arrived = extra.len().max(extra_iter.len());
        let over_cap_outcome = if over_cap_refused {
            "REFUSED at the native sender, typed, naming the limit, and nothing arrived".to_string()
        } else if stream_large_refusal.is_some() && arrived == 0 {
            "REFUSED typed, but the CEILING payload never arrived either — nothing at all \
             was delivered on this stream, so the failure is in delivery, not in the \
             refusal"
                .to_string()
        } else if stream_large_refusal.is_some() && extra.iter().any(|m| *m == want_stream_large) {
            "REFUSED typed at the sender, yet the over-cap payload ARRIVED — two senders \
             disagree about the cap"
                .to_string()
        } else if stream_large_refusal.is_some() {
            format!(
                "REFUSED typed, but {arrived} payload(s) arrived where exactly one (the \
                 ceiling) was due"
            )
        } else if stream_large_error.is_some() {
            "refused with the WRONG error — not EventTooLarge".to_string()
        } else if extra.iter().any(|m| *m == want_stream_large) {
            "ACCEPTED and delivered byte-exact — the sender no longer refuses".to_string()
        } else if arrived > 1 {
            "ACCEPTED and something OTHER than the payload arrived — a truncation".to_string()
        } else {
            "ACCEPTED with Ok and NOTHING arrived — the silent drop is back".to_string()
        };

        ledger.record(
            WITNESSES[12],
            sized.ok
                && request_up
                && reply_down
                && refused_typed
                && stream_down
                && over_cap_refused,
            format!(
                "LARGE MESSAGES THROUGH THE PUBLIC API, BOTH DIRECTIONS — measured against \
                 the interoperability §9.2 claims, in BOTH directions. (§9.2: no NATIVE \
                 node reassembles leaf fragments, so leaf → native above MAX_EVENT_SIZE = \
                 {MAX_EVENT_SIZE} B is not put back together, and above 64 832 B the leaf \
                 attempts nothing and returns a typed `LeafError::Wire` naming streams; \
                 native → leaf, the native sender refuses above the same \
                 {MAX_EVENT_SIZE} B with a typed `StreamError::EventTooLarge` naming the \
                 limit. Never a truncation, never a drop, in either direction.) \
                 LEG 1, AT THE CEILING: one `call('{ECHO_SERVICE}', <{ABI_CEILING_SIZE} B>)` \
                 through the package's public API (ok={}{}); the page built \
                 {got_request:?} and the ANCHOR's real handler recorded a body matching \
                 {want_request:?} byte for byte={handler_got_ceiling} — so a locally \
                 synthesised round trip cannot pass this. DOWN the same way: the echo \
                 service answers `echo:` + that body, {} B, and the page received \
                 {got_reply:?} against the expected {want_reply:?}={reply_down}. \
                 LEG 2, PAST THE HARD LIMIT: `call(<{ABI_OVER_LIMIT_SIZE} B>)` was refused \
                 with kind={over_kind:?} Display={over_message:?}, naming \
                 {over_named_bytes:?} framed bytes, and the anchor's handler was entered \
                 ZERO times afterwards={handler_untouched} (refused_typed={refused_typed}). \
                 The classifier is EXACT, not `some error`: kind must be `\"wire\"` — the \
                 leaf's `LeafError::Wire` from `frame::split_payload`, not the native \
                 sender's `EventTooLarge`, which measures `MAX_EVENT_SIZE` and is leg 3b's \
                 subject — the text must name the {ABI_LEAF_FRAG_CEILING}-byte \
                 fragmentation ceiling ({ABI_LEAF_MAX_FRAGMENTS} fragments of \
                 {MAX_EVENT_SIZE}) and the stream it points at, and the byte count it \
                 names must be at least the {ABI_OVER_LIMIT_SIZE} B we sent. It used to \
                 accept ANY nonempty kind, which a deadline, a session replacement or a \
                 leader loss would each have satisfied while also leaving the handler \
                 untouched — a green leg with the refusal gone. A silent drop, a \
                 truncation, a partial body dispatched to the handler, or a refusal for \
                 the wrong reason each fail this leg. \
                 LEG 3a, GATED, NATIVE → LEAF ON A STREAM at the same ceiling \
                 ({ABI_CEILING_SIZE} B), so `both directions` covers the stream API and \
                 not only nRPC. Both stream legs ride a stream of their OWN \
                 ({ABI_LARGE_STREAM_ID:#x}, page open ok={large_open_ok}), used by no \
                 other witness: \
                 they used to share the direct witness's stream and identify their \
                 payloads by INDEX — everything past the {ABI_DIRECT_EVENTS} that stream \
                 had carried — which made this verdict a function of ANOTHER leg's arrival \
                 count. A payload recovered late there would have leaked into this leg's \
                 surplus; a payload lost there shifted the index and hid THIS leg's \
                 ceiling payload inside the skipped prefix, so a round where nothing \
                 arrived was reported as a round where something extra did. On its own \
                 stream the payloads are its own from sequence zero and `extra` is \
                 literally everything delivered. The anchor's send returned \
                 {ceiling_send_error:?} and the package's callback AND iterator each \
                 received {want_stream_ceiling:?} byte-exact as the first payload on this \
                 stream={stream_down}; that wait is a CONDITION with a 30 s ceiling, not a \
                 sleep, and it took {large_waited} ms. \
                 LEG 3b, NOW GATED — the contract, not a measurement: the same send at \
                 {ABI_STREAM_LARGE_SIZE} B, over MAX_EVENT_SIZE = {MAX_EVENT_SIZE} B. This \
                 leg was RECORDED for one round, because §9.2 stated the LEAF → NATIVE \
                 direction only and the NATIVE → LEAF answer was unwritten; the \
                 measurement came back `Ok` plus NOTHING delivered — a silent drop on a \
                 reliable path — and it is repaired. The anchor's `send_with_retry` must \
                 now fail with the TYPED variant, checked as a variant and not as a \
                 string, carrying the offending size AND the limit: observed \
                 {stream_large_refusal:?}, required \
                 Some(({ABI_STREAM_LARGE_SIZE}, {MAX_EVENT_SIZE})); Display was \
                 {stream_large_error:?}. AND the payload must never appear: after a 2 s \
                 settle on a quiet link, everything that arrived on this stream is \
                 {extra:?} (iterator: {extra_iter:?}) — exactly ONE payload, the ceiling \
                 one, with {want_stream_large:?} absent. Outcome: {over_cap_outcome} \
                 (over_cap_refused={over_cap_refused}). An `Ok` with nothing delivered, a \
                 truncation, a late delivery, or an untyped/misnamed error each FAIL. \
                 WHERE A MISSING PAYLOAD WENT: the LEAF's counters at judgement were \
                 {large_counters} (iterator_ended={large_terminal}), and the ANCHOR's \
                 ledger for this stream was {large_sender}. \
                 Fragmenting instead was rejected on the merits: `send_on_stream` is \
                 transport-agnostic and only the browser leaf and the native RTC ingress \
                 reassemble, so fragmenting there would hand a native peer's application N \
                 partial events — a silent corruption in place of a silent drop. \
                 ANCHOR STATE: {}",
                sized.ok,
                sized
                    .error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                ceiling_reply.len(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 5 — a native peer's `find_best_node` returns the browser node
    //
    // The anchor is the browser's native peer, and the only one it
    // has: a leaf holds exactly one anchor session by construction,
    // so a second-hop native peer would be testing announcement
    // relay (Stage 6), not the browser leaf. The announcement is the
    // leaf's own, over the transport, and the resolution is the
    // core's own capability index — nothing here is harness-built.
    // ================================================================
    {
        let announced = script
            .run(
                "a",
                Step5::Announce {
                    id: 0,
                    session: "main".into(),
                    capabilities: vec![STAGE5_TAG.to_string()],
                },
            )
            .await;
        let req =
            CapabilityRequirement::from_filter(CapabilityFilter::new().require_tag(STAGE5_TAG));
        let resolved = wait_for(
            || cx.anchor.find_best_node(&req) == Some(node_id),
            Duration::from_secs(20),
        )
        .await;
        let got = cx.anchor.find_best_node(&req);
        // The leaf's own view, for the same fact from the other end.
        let queried = script
            .run(
                "a",
                Step5::Query {
                    id: 0,
                    session: "main".into(),
                    capability: STAGE5_TAG.into(),
                },
            )
            .await;
        ledger.record(
            WITNESSES[4],
            announced.ok && resolved,
            format!(
                "the leaf announced tag `{STAGE5_TAG}` over its own transport \
                 (announce ok={}); the NATIVE anchor's find_best_node(require_tag) \
                 resolved to {got:?} and the browser node is {node_id:#018x} \
                 (match={resolved}); the leaf's own query({STAGE5_TAG}) returned {}{}. \
                 ANCHOR STATE: {}",
                announced.ok,
                queried
                    .peers
                    .as_ref()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_else(|| "nothing".into()),
                announced
                    .error
                    .as_deref()
                    .map(|e| format!("; error: {e}"))
                    .unwrap_or_default(),
                peer_state(cx.anchor, node_id),
            ),
        );
    }

    // ================================================================
    // 9 — a direct node's event callback may call back into the node
    //
    // `wasm::Inner` is the leaf's one shared cell, and it exists only
    // behind a real `connect()` — so this schedule is unreachable
    // from the leaf's own wasm tests, which have no anchor, and from
    // the session surface, whose events are drained on a microtask.
    // On the direct surface the events were handed to the listeners
    // from inside `flush`, holding `Inner`'s mutable borrow, so the
    // documented `onMessage(p => stream.send(p))` echo — and a
    // listener doing anything else synchronous with the node —
    // re-entered that borrow and TRAPPED the module instead of
    // sending.
    //
    // The trigger is the session's own teardown event, because it is
    // the one event this harness can produce on demand: `close()`
    // drops the session, which pushes `Disconnected`, which is
    // delivered to the page's listener. What the listener then does
    // is two re-borrows of different kinds — `counters()` reads,
    // `openStream()` mutates — so a fix that released only one of
    // them is still visible here.
    //
    // The disposition is a *typed* answer, not a success: the node is
    // closed by the time its teardown is delivered, so `openStream`
    // must be refused with a kind. A trap carries no kind at all,
    // which is exactly what separates the two outcomes.
    // ================================================================
    {
        let armed = script
            .run(
                "a",
                Step5::ArmReentry {
                    id: 0,
                    session: "main".into(),
                },
            )
            .await;
        let closed = script
            .run(
                "a",
                Step5::Close {
                    id: 0,
                    session: "main".into(),
                },
            )
            .await;
        let report = script
            .run(
                "a",
                Step5::ReentryReport {
                    id: 0,
                    session: "main".into(),
                },
            )
            .await;
        let observed: serde_json::Value = report
            .info
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or(serde_json::Value::Null);
        let fired = observed.get("fired").and_then(serde_json::Value::as_u64);
        let counters_keys = observed
            .get("counters_keys")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let trapped = observed.get("trapped").and_then(serde_json::Value::as_str);
        let stream_outcome = observed
            .get("stream_outcome")
            .and_then(serde_json::Value::as_str);
        // A witness that proves nothing unless the listener really
        // ran: no event, no re-entry, no result.
        let listener_ran = fired.unwrap_or(0) > 0;
        let read_the_node = counters_keys > 0;
        let refused_typed = matches!(stream_outcome, Some(kind) if kind != "trapped");
        ledger.record(
            WITNESSES[8],
            armed.ok
                && closed.ok
                && report.ok
                && listener_ran
                && read_the_node
                && trapped.is_none()
                && refused_typed,
            format!(
                "a listener was armed on the GENERATED wasm node behind the direct \
                 `connect()` object — the same `on_event` surface the package's own event \
                 hub feeds from and `LeafStream.onMessage` registers on, and the one the \
                 wrapper detaches before teardown — and the session was then closed so its \
                 own `Disconnected` event is delivered to that listener (arm ok={}, \
                 close ok={}). The \
                 listener fired {fired:?} time(s) (ran={listener_ran}); its SYNCHRONOUS \
                 `counters()` re-borrow returned {counters_keys} counters \
                 (read={read_the_node}); its SYNCHRONOUS `openStream()` re-borrow came \
                 back as {stream_outcome:?} — a typed refusal, since the node is closed by \
                 the time its teardown is delivered (typed={refused_typed}); and nothing \
                 trapped ({trapped:?}). A callback invoked under `Inner`'s mutable borrow \
                 cannot do either of these: the first re-borrow panics the RefCell and the \
                 module traps, which arrives with no error kind. Report: {}",
                armed.ok,
                closed.ok,
                report
                    .info
                    .as_deref()
                    .unwrap_or("unreadable — the page returned no reentry state"),
            ),
        );
    }

    // ================================================================
    // 6 — two tabs, one identity, no eviction
    //
    // §8: one node per ORIGIN. Both tabs open through `openSession`,
    // which runs the Web Lock election: the holder is the LEADER and
    // runs the node, the other is a FOLLOWER attached over
    // BroadcastChannel and seeing the same API. The criterion is
    // three facts, all read where they are observable:
    //
    //   * both tabs report the SAME node id (one identity),
    //   * exactly one leader and one follower at the SAME generation
    //     (one node, not two racing ones),
    //   * the ANCHOR's session id is unchanged across the second
    //     tab's arrival (no eviction — a replacement session is an
    //     eviction under another name).
    //
    // What this witness does NOT gate on: whether `call()` works. It
    // records both tabs' call outcomes as evidence, because a
    // follower that cannot reach the leader is a real §8 defect —
    // but the reply-channel defect that currently reds every call is
    // a different witness's subject, and gating here would make this
    // one a duplicate of that one.
    // ================================================================
    {
        // Tab a must actually HOLD a session for "tab b did not
        // evict it" to mean anything. The witnesses above deliberately
        // push traffic a provisional peer is not allowed to send, so
        // this session can legitimately be gone by now — reclaimed by
        // §12's bounds or its TTL. Re-establish it and SAY SO, rather
        // than reporting "did not evict = false" about a session that
        // nothing evicted because nothing was there.
        //
        // Tab a is re-opened through `openSession` so both tabs
        // contend for the same Web Lock; the plain `connect` session
        // from witness 1 holds no lock and would make tab b the only
        // claimant. It was already closed by the re-entry witness
        // above, which needed that close to deliver a teardown event.
        let leader = script.run("a", connect_as("lead", false, true)).await;
        let leader_hex = leader.node_id.clone().unwrap_or_default();
        let leader_node = u64::from_str_radix(leader_hex.trim_start_matches("0x"), 16).unwrap_or(0);
        let session_before = if leader_node == 0 {
            None
        } else {
            cx.anchor.peer_session_id(leader_node)
        };

        let url_b = format!("{}/leaf5.html?tab=b", cx.page_origin);
        let opened = cx.driver.open_page("leaf5-b", &url_b).await;
        let second = if opened.is_ok() {
            script.run("b", connect_as("second", false, true)).await
        } else {
            fail(format!(
                "the second tab would not open: {}",
                opened.as_ref().err().cloned().unwrap_or_default()
            ))
        };
        let second_hex = second.node_id.clone().unwrap_or_default();
        let same_identity = !leader_hex.is_empty() && second_hex == leader_hex;
        let session_after = if leader_node == 0 {
            None
        } else {
            cx.anchor.peer_session_id(leader_node)
        };
        let no_eviction = session_before.is_some() && session_before == session_after;

        // Roles and generations, read from both tabs.
        let info_a = script
            .run(
                "a",
                Step5::Info {
                    id: 0,
                    session: "lead".into(),
                },
            )
            .await;
        let info_b = script
            .run(
                "b",
                Step5::Info {
                    id: 0,
                    session: "second".into(),
                },
            )
            .await;
        let role_a = info_a.role.clone().unwrap_or_default();
        let role_b = info_b.role.clone().unwrap_or_default();
        let gen_a = info_a.generation.clone().unwrap_or_default();
        let gen_b = info_b.generation.clone().unwrap_or_default();
        // Exactly one leader, and both at the same generation: two
        // leaders, or one leader and a follower fenced at a stale
        // generation, are both "two nodes" wearing one identity.
        let one_leader = (role_a == "leader" && role_b == "follower")
            || (role_a == "follower" && role_b == "leader");
        let same_generation = !gen_a.is_empty() && gen_a == gen_b;

        // Which tab holds the lock decides the whole schedule below,
        // so it is asserted rather than assumed: tab a ran
        // `openSession` to completion before tab b was opened, so tab
        // a must be the leader. A run where that is not true cannot
        // be driven by this script and says so instead of quietly
        // closing the wrong tab.
        let leader_is_a = role_a == "leader" && role_b == "follower";

        // GATED, and the O1 repair: the FOLLOWER's own nRPC must
        // reach the anchor's REAL handler and its reply must come
        // back to the follower. That chain is proxy → leader → the
        // leader's node → DataChannel → the anchor's dispatcher, and
        // breaking only the follower half of it used to leave every
        // named real-browser verdict green. Recording both calls as
        // "evidence" was an observation, not an acceptance predicate.
        let leader_nonce = format!("tab-leader-{:016x}", rand_u64());
        let follower_nonce = format!("tab-follower-{:016x}", rand_u64());
        echo_log.lock().expect("echo log").clear();
        let call_a = script
            .run(
                "a",
                Step5::Call {
                    id: 0,
                    session: "lead".into(),
                    service: ECHO_SERVICE.into(),
                    payload: hex(leader_nonce.as_bytes()),
                    timeout_ms: 15_000,
                },
            )
            .await;
        let call_b = if same_identity && leader_is_a {
            script
                .run(
                    "b",
                    Step5::Call {
                        id: 0,
                        session: "second".into(),
                        service: ECHO_SERVICE.into(),
                        payload: hex(follower_nonce.as_bytes()),
                        timeout_ms: 15_000,
                    },
                )
                .await
        } else {
            fail("not attempted: the two tabs did not reach one shared identity with tab a leading")
        };
        let echoed = |r: &StepResult, nonce: &str| {
            let mut want = b"echo:".to_vec();
            want.extend_from_slice(nonce.as_bytes());
            r.ok && r.reply.as_deref() == Some(hex(&want).as_str())
        };
        let handler_bodies = echo_log.lock().expect("echo log").clone();
        let handler_saw = |nonce: &str| {
            handler_bodies
                .iter()
                .any(|b| b.as_slice() == nonce.as_bytes())
        };
        let leader_rpc = echoed(&call_a, &leader_nonce) && handler_saw(&leader_nonce);
        let follower_rpc = echoed(&call_b, &follower_nonce) && handler_saw(&follower_nonce);

        // The frozen-tab leg, with a MEASURED correction from
        // slice 3: on current Chromium a frozen tab KEEPS its Web
        // Lock, so freezing the leader must NOT promote the
        // follower. That is what is asserted — a promotion here
        // would mean the fence can be jumped by a tab the browser
        // merely suspended.
        let frozen = cx.driver.set_lifecycle("leaf5-a", "frozen").await;
        let frozen_info_b = if frozen.is_ok() {
            script
                .run(
                    "b",
                    Step5::Info {
                        id: 0,
                        session: "second".into(),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let thawed = cx.driver.set_lifecycle("leaf5-a", "active").await;
        let still_follower = frozen_info_b.role.as_deref() == Some("follower")
            && frozen_info_b.generation.as_deref() == Some(gen_b.as_str());
        let freeze_leg = if let (Ok(()), Ok(())) = (&frozen, &thawed) {
            format!(
                "tab a was FROZEN through CDP Page.setWebLifecycleState and thawed again; \
                 tab b stayed role={:?} generation={:?} while it was frozen \
                 (still_follower={still_follower}) — which is the CORRECT behaviour and a \
                 correction to the plan's D2: a frozen tab KEEPS its Web Lock on this \
                 engine, so a suspended leader must NOT hand the identity over. Promotion \
                 needs the tab CLOSED, and that path is the leader-lifecycle witness's \
                 subject, not this one's",
                frozen_info_b.role, frozen_info_b.generation
            )
        } else {
            format!(
                "the frozen-tab leg is UNPROVEN on engine {}: {}",
                cx.engine.as_str(),
                frozen
                    .clone()
                    .err()
                    .or_else(|| thawed.clone().err())
                    .unwrap_or_else(|| "no reason reported".into())
            )
        };
        // A leg that could not run does not gate; a leg that ran and
        // reported a promotion does.
        let freeze_pass = frozen.is_err() || thawed.is_err() || still_follower;

        // ---- the leader CLOSES, with work pending and state to
        // ---- restore (the half O1 says this witness never took)
        //
        // The script used to close the FOLLOWER, which proves
        // nothing about handoff: the remaining tab was already the
        // leader. Here the LEADER stands down while a follower is
        // attached, a call is genuinely outstanding on the anchor,
        // and a capability was announced at RUNTIME through the
        // leader by the other tab. Four things then have to hold, and
        // all four are gated:
        //
        //   * the pending call FAILS, typed, and the anchor's handler
        //     was entered exactly ONCE — a silent replay would show
        //     up as two invocations;
        //   * the follower is PROMOTED, at a strictly higher
        //     generation;
        //   * the successor REBOOTSTRAPS: the anchor holds a session
        //     for the same node id again, and it is a DIFFERENT
        //     session id, so a retained one cannot pass;
        //   * the announcement intent SURVIVES — the anchor resolves
        //     the runtime tag after the handoff — and a new nRPC
        //     round trip works end to end.
        let restore_req =
            CapabilityRequirement::from_filter(CapabilityFilter::new().require_tag(RESTORE_TAG));
        let driveable = same_identity && leader_is_a;
        let announced_at_runtime = if driveable {
            script
                .run(
                    "b",
                    Step5::Announce {
                        id: 0,
                        session: "second".into(),
                        capabilities: vec![STAGE5_TAG.to_string(), RESTORE_TAG.to_string()],
                    },
                )
                .await
        } else {
            fail("not attempted: the two tabs did not reach one shared identity with tab a leading")
        };
        park_log.lock().expect("park log").clear();
        let park_nonce = format!("across-handoff-{:016x}", rand_u64());
        let pending_issued = if driveable {
            script
                .run(
                    "b",
                    Step5::CallBegin {
                        id: 0,
                        session: "second".into(),
                        service: PARK_SERVICE.into(),
                        payload: hex(park_nonce.as_bytes()),
                        timeout_ms: 60_000,
                        handle: "across-handoff".into(),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        // The call is only "pending" if it actually reached the
        // anchor. Without this the leg would also pass for a call
        // that never left the tab.
        let park_entered = pending_issued.ok
            && wait_for(
                || !park_log.lock().expect("park log").is_empty(),
                Duration::from_secs(20),
            )
            .await;

        let session_before_close = cx.anchor.peer_session_id(leader_node);
        let close_at = std::time::Instant::now();
        let leader_stood_down = if driveable {
            script
                .run(
                    "a",
                    Step5::Close {
                        id: 0,
                        session: "lead".into(),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        // The predecessor page STAYS OPEN. Destroying it here is what
        // made this leg unable to discriminate the thing it is named
        // after: page teardown independently kills the old JS, wasm
        // module, DataChannel and node, so a `close()` that retired
        // nothing looked identical to one that retired everything.
        // Explicit retirement with the owner still alive is the case
        // that matters, and it is the only one a page can cause.

        // Pending work: typed failure, once, naming the generation
        // that owned it.
        let pending_outcome = if park_entered {
            script
                .run(
                    "b",
                    Step5::CallAwait {
                        id: 0,
                        handle: "across-handoff".into(),
                        timeout_ms: 45_000,
                    },
                )
                .await
        } else {
            fail("not attempted: nothing was pending on the anchor")
        };
        // Exactly `leader-lost`, and exactly the predecessor's
        // generation. "Some kind is present" would also accept a
        // `session-lost`, an `rpc-timeout` or the node's own reported
        // text — all of which lose the answer to the question a page
        // asks next, which is *which* leader it lost.
        let pending_kind = pending_outcome.kind.clone().unwrap_or_default();
        let pending_message = pending_outcome.message.clone().unwrap_or_default();
        // EXACT equality against the predecessor's generation, read
        // from the failure's own STRUCTURED field (the browser
        // package's `RpcError.failure.generation`), not asked of the
        // Display text. `pending_message.contains(&gen_a)` accepted
        // any generation with the expected one as a substring — "1"
        // sits inside "31" and inside "12" — and any message that
        // mentioned the number for an unrelated reason, so it could
        // not tell "named the predecessor" from "named someone
        // else". An absent structured generation now FAILS: the
        // typed error's whole point is that a page does not have to
        // parse prose to learn which leader it lost.
        let pending_generation = pending_outcome.generation.clone().unwrap_or_default();
        let names_old_generation = !gen_a.is_empty() && pending_generation == gen_a;
        let pending_failed_typed = !pending_outcome.ok
            && pending_outcome.info.as_deref() != Some("never settled")
            && pending_kind == "leader-lost"
            && names_old_generation;
        let park_invocations = park_log.lock().expect("park log").len();
        // The parked handler is deliberately NOT released yet. It is
        // released after the successor has rebootstrapped, below, so
        // the delayed work is let go into a world that already has a
        // live successor — which is the interleaving a replay would
        // show up in, and the one a release before promotion cannot
        // reach.

        // Promotion, polled on the surviving tab.
        let old_generation = gen_a.parse::<u128>().ok();
        let mut promoted = fail("not attempted");
        for _ in 0..40 {
            if !driveable {
                break;
            }
            let info = script
                .run(
                    "b",
                    Step5::Info {
                        id: 0,
                        session: "second".into(),
                    },
                )
                .await;
            let is_leader = info.role.as_deref() == Some("leader");
            promoted = info;
            if is_leader {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let promoted_role = promoted.role.clone().unwrap_or_default();
        let promoted_gen = promoted.generation.clone().unwrap_or_default();
        let generation_advanced = match (old_generation, promoted_gen.parse::<u128>().ok()) {
            (Some(old), Some(new)) => new > old,
            _ => false,
        };

        // Successor rebootstrap, read at the anchor.
        let rebootstrapped = wait_for(
            || {
                let now = cx.anchor.peer_session_id(leader_node);
                now.is_some() && now != session_before_close
            },
            Duration::from_secs(45),
        )
        .await;
        let successor_session = cx.anchor.peer_session_id(leader_node);

        // Restoration of announcement intent, read at the anchor.
        let restored_tag = rebootstrapped
            && wait_for(
                || cx.anchor.find_best_node(&restore_req) == Some(leader_node),
                Duration::from_secs(30),
            )
            .await;

        // And traffic again, from the promoted tab, through the real
        // native handler.
        let after_nonce = format!("after-handoff-{:016x}", rand_u64());
        echo_log.lock().expect("echo log").clear();
        let call_after = if rebootstrapped {
            script
                .run(
                    "b",
                    Step5::Call {
                        id: 0,
                        session: "second".into(),
                        service: ECHO_SERVICE.into(),
                        payload: hex(after_nonce.as_bytes()),
                        timeout_ms: 30_000,
                    },
                )
                .await
        } else {
            fail("not attempted: the successor never rebootstrapped")
        };
        let after_bodies = echo_log.lock().expect("echo log").clone();
        let recovered_rpc = echoed(&call_after, &after_nonce)
            && after_bodies
                .iter()
                .any(|b| b.as_slice() == after_nonce.as_bytes());

        // NOW the delayed work is released, with a live successor
        // already carrying traffic. Its reply has nowhere to go,
        // which is the honest shape of "the remote operation may
        // still have executed" — and what must NOT happen is a
        // second invocation, which is the only shape a replay could
        // take.
        park_release.add_permits(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let park_invocations_after = park_log.lock().expect("park log").len();

        // Predecessor silence, observed on the page that is still
        // alive rather than assumed from its absence. Its session
        // object is retained by the harness after `close()`, so it
        // can be asked — and a retired owner must answer with a
        // typed refusal rather than serving, and must still name its
        // own generation rather than having adopted the successor's.
        let predecessor = if driveable {
            script
                .run(
                    "a",
                    Step5::Call {
                        id: 0,
                        session: "lead".into(),
                        service: ECHO_SERVICE.into(),
                        payload: hex(b"after-retirement"),
                        timeout_ms: 5_000,
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let predecessor_refused = !predecessor.ok && predecessor.kind.is_some();
        let predecessor_info = if driveable {
            script
                .run(
                    "a",
                    Step5::Info {
                        id: 0,
                        session: "lead".into(),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let predecessor_generation = predecessor_info.generation.clone().unwrap_or_default();
        let predecessor_kept_its_generation = predecessor_generation == gen_a;
        let echo_after_predecessor = echo_log.lock().expect("echo log").len();
        let interruption_ms = close_at.elapsed().as_millis();

        ledger.record(
            WITNESSES[5],
            same_identity
                && one_leader
                && leader_is_a
                && same_generation
                && no_eviction
                && freeze_pass
                && leader_rpc
                && follower_rpc
                && announced_at_runtime.ok
                && leader_stood_down.ok
                && park_entered
                && pending_failed_typed
                && park_invocations == 1
                && park_invocations_after == 1
                && predecessor_refused
                && predecessor_kept_its_generation
                && echo_after_predecessor == 1
                && promoted_role == "leader"
                && generation_advanced
                && rebootstrapped
                && restored_tag
                && recovered_rpc,
            format!(
                "both tabs opened through §8's openSession on one Web Lock scope. Tab a is \
                 node {} role={role_a:?} generation={gen_a:?}; tab b reported {}{} \
                 role={role_b:?} generation={gen_b:?} — same identity={same_identity}, \
                 exactly one leader and one follower={one_leader}, tab a is the \
                 leader={leader_is_a}, same generation={same_generation}. The ANCHOR's \
                 session for that node id was {session_before:?} before tab b arrived and \
                 {session_after:?} after, so tab b did NOT evict tab a's \
                 session={no_eviction} (a replacement session would be an eviction under \
                 another name). {freeze_leg}. GATED I/O: the LEADER's nRPC nonce reached the \
                 anchor's real handler and its exact reply came back={leader_rpc}; the \
                 FOLLOWER's nonce did the same, across proxy → leader → node → DataChannel \
                 → dispatcher={follower_rpc}{}. LEADER CLOSE with work pending: tab b \
                 announced `{RESTORE_TAG}` at runtime through the leader (ok={}), a call to \
                 `{PARK_SERVICE}` was issued and left outstanding and the anchor's handler \
                 confirmed it ENTERED={park_entered}; tab a's session then stood down \
                 (ok={}) and **its page was left alive**. The pending call failed with a \
                 TYPED rejection={pending_failed_typed} (kind={:?} message={:?} info={:?}), \
                 carrying the predecessor's own generation in its STRUCTURED field: \
                 {pending_generation:?} compared by EQUALITY against {gen_a:?} \
                 ={names_old_generation} — not a `contains` on the Display text, which \
                 a generation of 1 would satisfy inside a generation of 31, \
                 and the anchor's parked handler was entered {park_invocations} time(s) — \
                 exactly once is the requirement, so a silent replay fails here. Tab b was \
                 promoted to role={promoted_role:?} at generation={promoted_gen:?}, strictly \
                 higher than {gen_a:?}={generation_advanced}. The anchor then held a session \
                 for the SAME node id again: {successor_session:?} vs {session_before_close:?} \
                 before the close, a different incarnation={rebootstrapped}. \
                 find_best_node(require_tag=`{RESTORE_TAG}`) resolved to the node after the \
                 handoff={restored_tag}, so the runtime announcement intent survived it. A \
                 new nRPC round trip from the promoted tab reached the real handler and came \
                 back={recovered_rpc}. The DELAYED work was released only after that \
                 recovery, and the anchor's parked handler was still entered exactly \
                 {park_invocations_after} time(s) afterwards, so letting it go beside a \
                 live successor produced no second execution. PREDECESSOR SILENCE, read on \
                 the page that is still open: its retired session refused a fresh call \
                 with a typed failure={predecessor_refused} (kind={:?}), still reports its \
                 own generation {predecessor_generation:?} rather than the successor's \
                 ({predecessor_kept_its_generation}), and the anchor's echo handler was \
                 entered {echo_after_predecessor} time(s) in total after the handoff — the \
                 successor's one round trip and nothing from the retired owner. \
                 End-to-end interruption, close → recovered round trip: \
                 {interruption_ms} ms. ANCHOR STATE: {}",
                if leader_hex.is_empty() {
                    "nothing"
                } else {
                    leader_hex.as_str()
                },
                if second_hex.is_empty() {
                    "nothing"
                } else {
                    second_hex.as_str()
                },
                second
                    .error
                    .as_deref()
                    .map(|e| format!(" (error: {e})"))
                    .unwrap_or_default(),
                call_b
                    .error
                    .as_deref()
                    .map(|e| format!("; follower error: {e}"))
                    .unwrap_or_default(),
                announced_at_runtime.ok,
                leader_stood_down.ok,
                pending_outcome.kind,
                pending_outcome.message,
                pending_outcome.info,
                predecessor.kind,
                peer_state(cx.anchor, leader_node),
            ),
        );
    }

    // ================================================================
    // 6c — the same stream ABI, LEADER-PROXIED
    //
    // The other half of R11. Everything in 4b ran on the direct
    // `connect()` object, whose `openStream` is synchronous and whose
    // packets go straight onto its own DataChannel. A FOLLOWER's
    // `MeshSession.openStream` is a different code path end to end:
    // the open is a proxy round trip over BroadcastChannel, the
    // handle Rust stamps is carried back across it, `send` resolves
    // only once the LEADER tab has put the packet on the wire, and
    // inbound payloads make the same trip in reverse before the
    // package's `LeafStream` ever sees them.
    //
    // It runs here because this is the only point in the script
    // where a real leader and a real follower coexist on one
    // identity: tab b was promoted by the witness above, so tab a
    // can join its lock scope as a follower without minting a second
    // node or displacing the anchor's session.
    // ================================================================
    {
        let joined = script.run("a", connect_as("abi-follow", false, true)).await;
        let role_info = script
            .run(
                "a",
                Step5::Info {
                    id: 0,
                    session: "abi-follow".into(),
                },
            )
            .await;
        let follower_role = role_info.role.clone().unwrap_or_default();
        let is_follower = follower_role == "follower";

        let proxy_open = if is_follower {
            script
                .run(
                    "a",
                    Step5::StreamOpen {
                        id: 0,
                        session: "abi-follow".into(),
                        handle: "proxied".into(),
                        reliable: true,
                        label: Some("abi-proxied".into()),
                        stream_id: Some(ABI_PROXY_STREAM_ID.to_string()),
                        channel_hash: None,
                    },
                )
                .await
        } else {
            fail(format!(
                "not attempted: tab a joined as role={follower_role:?}, not a follower, so \
                 there is no proxied surface to exercise"
            ))
        };
        let want_proxy_hex = format!("{ABI_PROXY_STREAM_ID:016x}");
        // The package returned a PROMISE, which is the declared
        // difference between the two surfaces — a direct handle here
        // would mean this leg silently retested 4b.
        let really_proxied = proxy_open
            .stats
            .as_ref()
            .and_then(|s| s.get("proxied"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let proxy_handle_matches = stat_str(&proxy_open, "stream_id")
            == format!("\"{want_proxy_hex}\"")
            && stat_str(&proxy_open, "reliability") == "\"reliable\"";

        // Native → follower.
        let proxy_native = if proxy_open.ok {
            cx.anchor
                .open_stream(
                    node_id,
                    ABI_PROXY_STREAM_ID,
                    StreamConfig::new().with_reliability(Reliability::Reliable),
                )
                .map_err(|e| e.to_string())
        } else {
            Err("not attempted: the follower never opened a stream".into())
        };
        let proxy_seed = 0x6600 ^ (rand_u64() & 0xFFFF);
        let mut proxy_send_error: Option<String> = None;
        match &proxy_native {
            Ok(stream) => {
                for i in 0..ABI_PROXY_EVENTS {
                    let payload = Bytes::from(gen_bytes(proxy_seed + i as u64, ABI_PROXY_SIZE));
                    if let Err(e) = cx
                        .anchor
                        .send_with_retry(stream, std::slice::from_ref(&payload), 60)
                        .await
                    {
                        proxy_send_error = Some(format!("payload {i}: {e}"));
                        break;
                    }
                }
            }
            Err(e) => proxy_send_error = Some(e.clone()),
        }
        let proxy_inbox = if proxy_open.ok {
            script
                .run(
                    "a",
                    Step5::StreamInbox {
                        id: 0,
                        handle: "proxied".into(),
                        expect: ABI_PROXY_EVENTS,
                        timeout_ms: 45_000,
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let want_proxy = expected_marks(proxy_seed, ABI_PROXY_SIZE, ABI_PROXY_EVENTS);
        let proxy_callback = marks(&proxy_inbox, "callback");
        let proxy_iterator = marks(&proxy_inbox, "iterator");
        let proxy_callback_exact = proxy_callback == want_proxy;
        let proxy_iterator_exact = proxy_iterator == want_proxy;

        // Follower → native, through the same proxy in reverse: a
        // natively encoded nRPC REQUEST on a proxied stream must
        // reach the anchor's REAL handler.
        sink_log.lock().expect("sink log").clear();
        let out_body = format!("s5-proxied-out-{:016x}", rand_u64());
        let out_frame = rpc_request_frame(SINK_SERVICE, origin_hash, 0x6C00, out_body.as_bytes());
        let out_open = if is_follower {
            script
                .run(
                    "a",
                    Step5::StreamOpen {
                        id: 0,
                        session: "abi-follow".into(),
                        handle: "proxied-out".into(),
                        reliable: true,
                        label: Some("abi-proxied-out".into()),
                        stream_id: Some(format!("{}", out_frame.stream_id)),
                        channel_hash: Some(out_frame.channel_hash_u16),
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let out_write = if out_open.ok {
            script
                .run(
                    "a",
                    Step5::StreamWrite {
                        id: 0,
                        handle: "proxied-out".into(),
                        frames: vec![hex(&out_frame.payload)],
                        seed: 0,
                        size: 0,
                        count: 0,
                        drop_every: 0,
                        reorder_every: 0,
                    },
                )
                .await
        } else {
            fail("not attempted")
        };
        let outbound_landed = out_write.ok
            && wait_for(
                || {
                    sink_log
                        .lock()
                        .expect("sink log")
                        .iter()
                        .any(|b| b.as_slice() == out_body.as_bytes())
                },
                Duration::from_secs(30),
            )
            .await;

        ledger.record(
            WITNESSES[13],
            joined.ok
                && is_follower
                && proxy_open.ok
                && really_proxied
                && proxy_handle_matches
                && proxy_send_error.is_none()
                && proxy_callback_exact
                && proxy_iterator_exact
                && outbound_landed,
            format!(
                "THE LEADER-PROXIED STREAM SURFACE, exercised through the built package. \
                 Tab a rejoined the promoted leader's lock scope through §8's \
                 `openSession` (ok={}) and reports role={follower_role:?} — a \
                 FOLLOWER={is_follower}, so every stream operation below is a proxy round \
                 trip over BroadcastChannel to the tab that owns the DataChannel, not the \
                 direct path 4b exercised. `MeshSession.openStream` returned a PROMISE \
                 rather than a handle, which is the declared difference between the two \
                 surfaces={really_proxied}, and the stream it resolved to reports \
                 streamId={} reliability={} — the id and mode asked for \
                 ({want_proxy_hex}, reliable)={proxy_handle_matches}, so the options \
                 survived the proxy hop and the handle Rust stamped came back across it. \
                 NATIVE → FOLLOWER: the anchor opened the same id and sent \
                 {ABI_PROXY_EVENTS} × {ABI_PROXY_SIZE} B from seed {proxy_seed:#x} (error: \
                 {proxy_send_error:?}); the package's `onMessage` callback received {} \
                 payload(s) byte-exact and in order={proxy_callback_exact} and its async \
                 iterator received {} byte-exact and in order={proxy_iterator_exact} — both \
                 after leader → follower relay. FOLLOWER → NATIVE: a natively encoded nRPC \
                 REQUEST written to a second proxied stream reached the anchor's REAL \
                 `{SINK_SERVICE}` handler with body {out_body:?}={outbound_landed} (open \
                 ok={}, write ok={}), so the proxy carries bytes in both directions and \
                 not merely the open. ANCHOR STATE: {}",
                joined.ok,
                stat_str(&proxy_open, "stream_id"),
                stat_str(&proxy_open, "reliability"),
                proxy_callback.len(),
                proxy_iterator.len(),
                out_open.ok,
                out_write.ok,
                peer_state(cx.anchor, node_id),
            ),
        );

        // Hand the follower back before the displacement witness
        // runs: a live follower in tab a would promote itself when
        // tab b's page closes and bootstrap a node of its own, which
        // the UDP witness's "the identity is free" wait cannot see
        // coming.
        let _ = script
            .run(
                "a",
                Step5::Close {
                    id: 0,
                    session: "abi-follow".into(),
                },
            )
            .await;
    }

    // ================================================================
    // 6b — a reconnect that DISPLACES an extant busy incumbent
    //
    // The browser counterpart of the native R12 witness, and the O2
    // repair. The retry control in witness 7 below waits for the
    // anchor to FORGET the identity before reconnecting, and
    // continues either way — so a green run can take the
    // absent-session branch and never exercise the responder's
    // busy-displacement path at all. Here the premise is asserted
    // before the reconnect is attempted:
    //
    //   * the anchor holds a session for this node id, and
    //   * that session is BUSY by the gate's own definition — at
    //     least one application stream open, not just the signalling
    //     stream.
    //
    // Then the SAME custodial identity connects again from the same
    // tab: a new DataChannel, the anchor as RESPONDER, the incumbent
    // busy on a different RTC endpoint. It must succeed, the
    // anchor's session must be a DIFFERENT incarnation afterwards,
    // and traffic must work on the successor.
    // ================================================================
    {
        let name = WITNESSES[6];
        let signalling = u64::from(net::adapter::net::rtc::SUBPROTOCOL_RTC_SIGNAL);
        let busy_ids = |node: u64| -> Vec<u64> {
            cx.anchor
                .peer_session_for_test(node)
                .map(|s| {
                    s.stream_ids()
                        .into_iter()
                        .filter(|id| *id != signalling)
                        .collect()
                })
                .unwrap_or_default()
        };

        // Make the incumbent busy the way an application does: one
        // nRPC REQUEST event on a stream the page opens, encoded
        // natively under the leaf's own origin so the anchor admits
        // and DISPATCHES it.
        let frame = rpc_request_frame(SINK_SERVICE, origin_hash, 0x6B00, b"s5-busy-premise");
        let busied = script
            .run(
                "b",
                Step5::StreamSend {
                    id: 0,
                    session: "second".into(),
                    reliable: false,
                    stream_id: format!("{}", frame.stream_id),
                    channel_hash: frame.channel_hash_u16,
                    payloads: vec![hex(&frame.payload)],
                    drop_every: 0,
                },
            )
            .await;
        let incumbent_live =
            wait_for(|| !busy_ids(node_id).is_empty(), Duration::from_secs(20)).await;
        let incumbent_session = cx.anchor.peer_session_id(node_id);
        let open_streams = busy_ids(node_id);
        // HARD premise. An absent or quiet incumbent means the
        // displacement branch was never reached, which is a FAILURE
        // of this witness rather than a pass by another route.
        let premise = incumbent_session.is_some() && incumbent_live;

        let reconnected = if premise {
            script.run("b", connect_step("reconnect", false)).await
        } else {
            fail(
                "not attempted: there was no extant BUSY incumbent to displace, so the \
                  responder's displacement branch could not be reached",
            )
        };
        let reconnect_node = reconnected
            .node_id
            .as_deref()
            .map(|h| u64::from_str_radix(h.trim_start_matches("0x"), 16).unwrap_or(0))
            .unwrap_or(0);
        let same_identity_again = reconnect_node == node_id;
        let displaced = premise
            && reconnected.ok
            && wait_for(
                || {
                    let now = cx.anchor.peer_session_id(node_id);
                    now.is_some() && now != incumbent_session
                },
                Duration::from_secs(30),
            )
            .await;
        let successor = cx.anchor.peer_session_id(node_id);

        // The successor has to carry traffic, or "displaced" would
        // only mean the incumbent was destroyed.
        let nonce = format!("after-displacement-{:016x}", rand_u64());
        echo_log.lock().expect("echo log").clear();
        let call = if displaced {
            script
                .run(
                    "b",
                    Step5::Call {
                        id: 0,
                        session: "reconnect".into(),
                        service: ECHO_SERVICE.into(),
                        payload: hex(nonce.as_bytes()),
                        timeout_ms: 30_000,
                    },
                )
                .await
        } else {
            fail("not attempted: nothing was displaced")
        };
        let mut want = b"echo:".to_vec();
        want.extend_from_slice(nonce.as_bytes());
        let handler_saw_it = echo_log
            .lock()
            .expect("echo log")
            .iter()
            .any(|b| b.as_slice() == nonce.as_bytes());
        let successor_works =
            call.ok && call.reply.as_deref() == Some(hex(&want).as_str()) && handler_saw_it;

        ledger.record(
            name,
            premise && reconnected.ok && same_identity_again && displaced && successor_works,
            format!(
                "PREMISE FIRST, because the absent-session branch proves nothing about \
                 displacement: before the reconnect the anchor held session \
                 {incumbent_session:?} for node {node_id:#018x} and that session was BUSY \
                 by the gate's own definition — application streams {open_streams:?} open \
                 besides the signalling stream {signalling:#x} (busy={incumbent_live}, the \
                 event that made it so ok={}). The SAME custodial identity then connected \
                 again from the same tab (a new DataChannel, so the anchor is the RESPONDER \
                 facing a busy incumbent on a DIFFERENT RTC endpoint): ok={}{}, reported node \
                 {:?} which is the same identity={same_identity_again}. The anchor's session \
                 afterwards is {successor:?} — a different incarnation than the one \
                 displaced={displaced}. A nonce round trip on the successor reached the \
                 anchor's real handler and came back exactly={successor_works}. ANCHOR \
                 STATE: {}",
                busied.ok,
                reconnected.ok,
                reconnected
                    .error
                    .as_deref()
                    .map(|e| format!(" (error: {e})"))
                    .unwrap_or_default(),
                reconnected.node_id,
                peer_state(cx.anchor, node_id),
            ),
        );

        // Hand the identity back through the session's own API, then
        // let tab b go and bring tab a back for the last witness.
        for session in ["reconnect", "second"] {
            let _ = script
                .run(
                    "b",
                    Step5::Close {
                        id: 0,
                        session: session.into(),
                    },
                )
                .await;
        }
        let _ = cx.driver.close_page("leaf5-b").await;
        if let Err(e) = cx.driver.open_page("leaf5-a", &url_a).await {
            println!("[stage5] WARNING: could not reopen tab a for the UDP witness: {e}");
        }
    }

    // ================================================================
    // 7 — a prompt TYPED failure under the UDP-blocked profile
    //
    // LAST, because the profile takes UDP away for as long as it is
    // installed — a host-wide firewall rule on Linux, and this
    // browser's WebRTC UDP on Windows. See `udp_block.rs` for which
    // mechanism runs where and why natsim was rejected.
    //
    // # The falsifier, twice
    //
    // `UdpBlocked` is produced from ONE piece of evidence: the HTTPS
    // bootstrap succeeded while a STUN binding to the anchor's
    // published `rtc_addr` went unanswered. That inference is only
    // sound if a HEALTHY anchor DOES answer an unauthenticated
    // binding — an anchor that silently drops them would make every
    // ICE timeout anywhere look like blocked UDP. So the witness
    // probes the healthy anchor FIRST and requires `reflexive` or
    // `stunError`; `unanswered` there fails this witness with that as
    // the reason, because the typing would be unfalsifiable.
    //
    // Then, with the profile installed, the SAME probe must come back
    // `unanswered`. That is the second half of the same falsifier and
    // it is what proves the profile is actually in force: a
    // relaunched browser that quietly kept its UDP would otherwise
    // let a `connect` that failed for an unrelated reason pass as
    // "typed failure under a blocked network".
    // ================================================================
    {
        let name = WITNESSES[7];
        // The sessions of the two witnesses above are already handed
        // back through their own APIs (the displacement witness
        // closes `reconnect` and `second`; the leader-close leg
        // stands `lead` down). Tab a was reopened fresh for this
        // witness, so there is nothing of its own to close — but the
        // ANCHOR's side of those departures is asynchronous, which is
        // what the wait below is for. This witness connects the SAME
        // identity twice more (blocked, then the control), and an
        // anchor still holding a live peer entry for that node id
        // closes the next bootstrap dialog — observed as `the anchor
        // closed the bootstrap dialog: 1006` on the control leg,
        // which is one identity arriving twice, not a defect in the
        // typed failure.
        // Closing is asynchronous ON THE ANCHOR: the peer entry goes
        // when the transport actually drops, not when the page
        // returns. The control leg below presents the SAME custodial
        // identity, and an anchor still holding a session for it
        // closes the new bootstrap dialog with `1006` — reddening
        // this witness for a reason that has nothing to do with UDP
        // (measured on Chromium: every other condition passed and
        // only the control leg failed). So wait for the identity to
        // be free, and say so if it never is.
        let identity_freed = wait_for(
            || cx.anchor.peer_session_id(node_id).is_none(),
            Duration::from_secs(20),
        )
        .await;
        if !identity_freed {
            println!(
                "[stage5] WARNING: the anchor still holds a session for {node_id:#018x} \
                 20 s after the page closed it; the control leg may fail with 1006 for \
                 that reason rather than for anything about UDP"
            );
        }
        // Witness 7's legs keep the SHARED custodial identity, on
        // purpose and by ruling.
        //
        // Giving them a fresh one makes the control leg green, and
        // was reverted: it would hide the case this witness must be
        // able to see — a page that got a typed failure and RETRIES
        // being refused. The `1006` observed on the control leg is
        // not an identity collision anyway; it is a refused WebSocket
        // HANDSHAKE (HTTP 404 from the trickle route, which a browser
        // reports as a bare 1006 with no reason) caused by the
        // completion owner retiring the bootstrap attempt at
        // DataChannel-open, before the install commits — the same
        // `no such dialog on this anchor` the 4b sweep logs. That
        // repair is core+sdk and is owned elsewhere; this witness's
        // job is to keep failing until it lands.
        let probe_step = || Step5::StunProbe {
            id: 0,
            addr: cx.anchor_rtc_addr.to_string(),
        };
        let healthy = script.run("a", probe_step()).await;
        let healthy_outcome = healthy
            .info
            .clone()
            .unwrap_or_else(|| "no outcome reported".into());
        let probe_is_sound =
            healthy.ok && matches!(healthy_outcome.as_str(), "reflexive" | "stunError");
        match UdpProfile::install(cx.anchor_rtc_addr, cx.engine).await {
            Err(why) => ledger.record(name, false, why),
            Ok(profile) => {
                // The engine-policy profile needs the browser
                // relaunched; the firewall one does not. `establish`
                // is a no-op for the latter, so the sequence below
                // reads the same for both.
                let established = match &profile {
                    UdpProfile::Firewall(_) => Ok(String::from(
                        "the kernel rule needs no relaunch: it applies to the running browser",
                    )),
                    UdpProfile::EngineUdpOff { .. } => {
                        relaunch(&cx, true, &url_a, "with its non-proxied UDP taken away").await
                    }
                };
                let blocked_probe = if established.is_ok() {
                    script.run("a", probe_step()).await
                } else {
                    fail("not attempted: the profile was not established")
                };
                let blocked_outcome = blocked_probe
                    .info
                    .clone()
                    .unwrap_or_else(|| "no outcome reported".into());
                let profile_in_force = blocked_probe.ok && blocked_outcome == "unanswered";
                let blocked = if established.is_ok() {
                    script.run("a", connect_step("blocked", true)).await
                } else {
                    fail("not attempted: the profile was not established")
                };
                // H2: the removal result is no longer discarded. A
                // rule the kernel refused to delete would otherwise
                // be reported as "removed" on the strength of the
                // command having been issued, and left behind on the
                // host.
                let removed = profile.remove().await;
                let kind = blocked.kind.clone().unwrap_or_default();
                let message = blocked.message.clone().unwrap_or_default();
                let elapsed = blocked.elapsed_ms.unwrap_or(f64::NAN);
                // The evidence pair `UdpBlocked` is defined by.
                let bootstrap_ok = blocked
                    .evidence
                    .as_ref()
                    .and_then(|e| e.get("bootstrapOk"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                let stun_failed = blocked
                    .evidence
                    .as_ref()
                    .and_then(|e| e.get("stunProbeFailed"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                // Both halves: the kind the wrapper reports AND the
                // Rust Display it claims to carry. `IceTimeout`'s
                // text also mentions UDP, so the prefix is matched,
                // never a substring.
                let typed = kind == "udp-blocked"
                    && message.starts_with("rtc: UDP appears blocked:")
                    && !blocked.test_error.unwrap_or(false);
                let prompt = elapsed.is_finite() && elapsed < 45_000.0;
                // The control: with the profile gone the same connect
                // must succeed, so the typed failure is attributable
                // to the missing UDP and not to a broken anchor. For
                // the engine profile "gone" means relaunched without
                // the flag, which is also what undoes it.
                let restored = match &profile {
                    UdpProfile::Firewall(_) => removed
                        .clone()
                        .map(|()| {
                            String::from("the kernel rule was removed and the kernel took it")
                        })
                        .map_err(|e| format!("the kernel REFUSED the removal: {e}")),
                    UdpProfile::EngineUdpOff { .. } => {
                        relaunch(&cx, false, &url_a, "with its UDP restored").await
                    }
                };
                let control = if restored.is_ok() {
                    script.run("a", connect_step("unblocked", false)).await
                } else {
                    fail("not attempted: the browser could not be restored")
                };
                ledger.record(
                    name,
                    typed
                        && prompt
                        && bootstrap_ok
                        && stun_failed
                        && control.ok
                        && probe_is_sound
                        && profile_in_force
                        && removed.is_ok(),
                    format!(
                        "FALSIFIER FIRST: against the HEALTHY anchor an unauthenticated STUN \
                         binding to {} came back {healthy_outcome:?}, so the probe the typing \
                         rests on can distinguish blocked from merely-timed-out \
                         (sound={probe_is_sound}; `unanswered` here would make every ICE \
                         timeout look like blocked UDP). UDP-blocked profile then installed \
                         (never natsim, never a simulated failure): {}. Establishing it: {}. \
                         The SAME probe then came back {blocked_outcome:?}, so the profile \
                         was really in force (in_force={profile_in_force}) rather than a \
                         browser that quietly kept its UDP. connect() rejected in \
                         {elapsed:.0} ms (prompt={prompt}) with kind={kind:?} and \
                         Display={message:?} — the narrow type, matched on the full prefix \
                         rather than a substring because IceTimeout's text also mentions UDP \
                         (typed={typed}); the leaf's own evidence: bootstrapOk={bootstrap_ok}, \
                         stunProbeFailed={stun_failed}. CONTROL: {}; the same connect then \
                         succeeded={} — so the typed failure is attributable to the absent \
                         UDP and not to an unhealthy anchor{}",
                        cx.anchor_rtc_addr,
                        profile.how(),
                        established.as_ref().map_or_else(
                            |e| format!("FAILED — {e}"),
                            std::string::ToString::to_string
                        ),
                        restored.as_ref().map_or_else(
                            |e| format!("the browser could NOT be restored — {e}"),
                            std::string::ToString::to_string
                        ),
                        control.ok,
                        control
                            .error
                            .as_deref()
                            .map(|e| format!("; control error: {e}"))
                            .unwrap_or_default(),
                    ),
                );
            }
        }
    }

    let _ = script.run("a", Step5::Done { id: 0 }).await;
    let _ = cx.driver.close_page("leaf5-a").await;
    println!(
        "[stage5] engine {} ({})",
        cx.engine.as_str(),
        if cx.engine.is_gate() {
            "gate"
        } else {
            "best effort, recorded"
        }
    );
    Ok(())
}

/// A nonce without pulling in a dependency: the wall clock's low bits
/// mixed with the address of a local, which is enough for a body that
/// only has to be unlike the last run's.
fn rand_u64() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let local = 0u8;
    now ^ ((&raw const local as usize as u64) << 13)
}

/// 32 bytes for a custodial secret. Not cryptographic quality and it
/// does not need to be: it only has to be a fresh, valid scalar that
/// two tabs in ONE harness run share and no other run reuses.
fn random_32() -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut state = rand_u64() | 1;
    for chunk in out.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    // Clamp away the two values Ed25519/X25519 scalars must not be.
    out[0] &= 0xF8;
    out[31] = (out[31] & 0x7F) | 0x40;
    out
}

// ===================================================================
// The byte vocabulary the page and the runner share
// ===================================================================

/// The deterministic payload generator, implemented identically in
/// `page/leaf5.js` (`genBytes`).
///
/// Both sides derive the bytes from `(seed, len)`, so a
/// multi-megabyte exercise costs nothing on the HTTP step channel
/// and the comparison is still byte-exact. Keep the two in step: a
/// drift shows up as every payload mismatching, which is loud.
fn gen_bytes(seed: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| {
            let i = i as u64;
            (seed.wrapping_add(i * 167).wrapping_add((i >> 8) * 13) & 0xFF) as u8
        })
        .collect()
}

/// FNV-1a/32, identical to the page's `fnv1a`.
///
/// Reported beside the length, so a payload that arrived truncated,
/// duplicated, reordered or corrupted is a mismatch rather than a
/// count that happens to agree.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// One payload as both sides describe it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct Mark {
    len: usize,
    fnv: u32,
}

impl Mark {
    fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len(),
            fnv: fnv1a(bytes),
        }
    }
}

/// The marks the page reported under `field`, in arrival order.
fn marks(result: &StepResult, field: &str) -> Vec<Mark> {
    result
        .stats
        .as_ref()
        .and_then(|stats| stats.get(field))
        .and_then(|value| serde_json::from_value::<Vec<Mark>>(value.clone()).ok())
        .unwrap_or_default()
}

/// The marks `count` generated payloads from `seed` must produce.
fn expected_marks(seed: u64, size: usize, count: usize) -> Vec<Mark> {
    (0..count)
        .map(|i| Mark::of(&gen_bytes(seed + i as u64, size)))
        .collect()
}

/// The byte count a leaf over-cap refusal names, read out of its
/// `Display` text: `"wire: payload of <N> bytes exceeds the …"`.
///
/// `None` when the message is not that refusal at all, which is
/// exactly the discrimination leg 2 of the large-message witness
/// needs: a deadline or session error carries no such number, so it
/// cannot satisfy the predicate by accident. Split-based rather than
/// prefix-based so the `wire: ` the browser package prepends does
/// not have to be spelled here twice.
fn refused_payload_bytes(message: &str) -> Option<usize> {
    message
        .split("payload of ")
        .nth(1)?
        .split(" bytes")
        .next()?
        .parse()
        .ok()
}

/// A `stats` field read as a string, for the ledger text.
fn stat_str(result: &StepResult, field: &str) -> String {
    result
        .stats
        .as_ref()
        .and_then(|stats| stats.get(field))
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| "absent".into())
}

/// A `stats` field read as a `u64`.
fn stat_u64(result: &StepResult, field: &str) -> u64 {
    result
        .stats
        .as_ref()
        .and_then(|stats| stats.get(field))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}
