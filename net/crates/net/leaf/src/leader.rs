//! §8 leader election and the D2 leader lifecycle: one node per
//! origin, a Web Lock, a fenced generation, and the follower proxy.
//!
//! # The shape of it
//!
//! One identity per origin means several tabs would otherwise present
//! the same node id from independent sessions and evict each other
//! under the identity-rebind rules. So exactly one tab runs the node:
//!
//! - Tabs contend for Web Lock `net-mesh/<origin>/<fingerprint>`.
//! - The holder is the **leader** and runs `crate::wasm::LeafNode`
//!   on the main thread (S0b: `RTCPeerConnection` is undefined in
//!   both a dedicated `Worker` and a `SharedWorker`, so there is no
//!   worker option to weigh).
//! - Everyone else is a **follower** and gets the same session API
//!   over a `BroadcastChannel`, with nRPC futures, stream objects and
//!   events proxied.
//!
//! # Generations and fencing (D2)
//!
//! On acquisition the leader reads the generation from IndexedDB,
//! increments it and writes it back inside one transaction
//! (`crate::storage::IdentityVault::next_generation`), then stamps
//! that value on **every** message it sends and every operation it
//! performs. Three parties enforce it:
//!
//! 1. A follower refuses any message below the highest generation it
//!    has seen ([`GenerationGate`]).
//! 2. The leader refuses any follower request that is not stamped with
//!    its own generation ([`ProxyServer::on_message`]).
//! 3. The storage layer refuses to act for a generation it does not
//!    record (`crate::storage::IdentityVault::fence`).
//!
//! A tab that was suspended and resumes therefore cannot act as
//! leader: its lock is gone *or* it still believes it holds one, and
//! either way every message it emits carries a generation that all
//! three of those refuse. The lock alone would not be enough — a
//! frozen document is not required to have dropped its locks — so the
//! generation, not the lock, is what the fence rests on.
//!
//! # Why this module is not all `wasm32`
//!
//! Everything above the two browser primitives — the gate, the
//! follower registry, the proxy protocol and both ends of the proxy
//! — is plain Rust with a [`ProxyTransport`] seam, and is tested
//! natively. `web_sys` appears only in the Web Lock request, the
//! `BroadcastChannel` adapter and the `wasm_bindgen` session type at
//! the bottom of the file. That is deliberate: the lifecycle is the
//! part with the subtle failure modes, and it should not need a
//! browser to review.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;

use bytes::Bytes;
use futures_channel::oneshot;
use serde_json::{Map, Value};

use crate::error::{LeafError, Result, RpcError, RtcError, UdpBlockedEvidence};
use crate::stream::Reliability;

/// The proxy protocol version. Bumped when a body changes shape; an
/// envelope carrying anything else is refused rather than guessed at,
/// because two tabs running different builds of the package is an
/// ordinary consequence of a deploy.
///
/// `2` because [`ProxyBody::Attach`] now carries the follower's
/// **capability** intent beside its subscriptions: D2 promises the
/// current announcement is re-published on takeover, and a successor
/// that only learned about channels could restore half of it.
///
/// `3` because the org serve plane changed shape (the proxy-plane
/// repair: `OrgServeUnregister` carries its registration alone — the
/// leader unregisters by its own recorded name, LEAF-4; the accept
/// doc carries the handle alone and the projection rides the new
/// `OrgServeCaller` verb, LEAF-6). A mixed pair would half-work
/// without the gate — an old accept decoder silently drops the new
/// doc — so the version refuses instead.
///
/// `4` because `OrgServeRetired` now ANSWERS for a normally completed
/// call (an `END` envelope) and the leader releases the served call
/// there (§23 audit, LEAF-13). A `3` follower would re-ask, find the
/// call released, and misread the refusal as `LeaderLost`.
pub const PROXY_VERSION: u64 = 4;

/// The Web Lock and `BroadcastChannel` name for an identity on an
/// origin.
///
/// Both carriers share one name: they are the same scope — "the node
/// for this identity on this origin" — and a mismatch between them
/// would let a tab hold the lock while talking on a channel nobody
/// listens to.
pub fn scope_name(origin: &str, fingerprint: &str) -> String {
    format!("net-mesh/{origin}/{fingerprint}")
}

// ─────────────────────────────── fencing ───────────────────────────────

/// What admitting a generation meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The generation already seen: ordinary traffic from the current
    /// leader.
    Current,
    /// A generation above the highest seen: leadership moved, and the
    /// previous generation's in-flight work is now dead.
    NewLeader {
        /// The generation that was current until this message. `0`
        /// when this is the first leader this follower has seen.
        previous: u64,
    },
}

/// The fence a follower applies to every inbound message.
///
/// Monotonic and never reset: refusing to go backwards is the entire
/// mechanism, and a reset would be a way to re-admit a resumed tab.
#[derive(Debug, Clone, Default)]
pub struct GenerationGate {
    highest: u64,
}

impl GenerationGate {
    /// A gate that has seen nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// The highest generation admitted so far. `0` before any.
    #[inline]
    pub fn highest(&self) -> u64 {
        self.highest
    }

    /// Admit a message stamped `generation`.
    ///
    /// Refuses anything below the highest seen with
    /// [`LeafError::NotLeader`], which names both the generation the
    /// sender presented and the one actually in force — that is the
    /// stale-leader refusal, and it is typed so a page can tell it
    /// apart from a transport failure.
    pub fn admit(&mut self, generation: u64) -> Result<Admission> {
        if generation < self.highest {
            return Err(LeafError::NotLeader {
                presented: generation,
                current: Some(self.highest),
            });
        }
        if generation == self.highest {
            return Ok(Admission::Current);
        }
        let previous = self.highest;
        self.highest = generation;
        Ok(Admission::NewLeader { previous })
    }
}

/// The live generation a leader's outbound effects are admitted
/// under — D2's fence as an object rather than as a number.
///
/// [`GenerationGate`] is the *receiving* half: it refuses a message
/// stamped by a leader that has been superseded. This is the
/// *emitting* half, and it exists because a number cannot be revoked.
/// Stand-down revokes the lease **before** the node is closed and
/// before the lock is released, so everything that could still be
/// produced by work admitted earlier — a reply from an operation that
/// was parked before dispatch, that operation's next poll, the node's
/// ticker — consults a fence that has already moved.
///
/// Cheap on purpose: a `Cell` read, not a storage lookup. The storage
/// generation (`crate::storage::IdentityVault::fence`) is the
/// authority on *who holds the lock*, and a leader revalidates against
/// it periodically; but a per-effect IndexedDB transaction would put
/// an await in front of every send, and an await is exactly the window
/// this is closing.
///
/// A revoked lease is never re-armed. A tab that stood down and later
/// takes leadership again gets a new lease for its new generation,
/// which is what makes "admitted under generation *n*" mean something
/// after the tab has been leader twice.
#[derive(Debug, Clone)]
pub struct GenerationLease {
    inner: Rc<LeaseState>,
}

#[derive(Debug)]
struct LeaseState {
    generation: u64,
    revoked: Cell<bool>,
    successor: Cell<Option<u64>>,
    refused: Cell<u64>,
}

impl GenerationLease {
    /// A live lease for `generation`.
    pub fn new(generation: u64) -> Self {
        Self {
            inner: Rc::new(LeaseState {
                generation,
                revoked: Cell::new(false),
                successor: Cell::new(None),
                refused: Cell::new(0),
            }),
        }
    }

    /// The generation this lease admits.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.inner.generation
    }

    /// Whether effects are still admitted.
    #[inline]
    pub fn is_live(&self) -> bool {
        !self.inner.revoked.get()
    }

    /// The generation that displaced this one, when it is known.
    #[inline]
    pub fn successor(&self) -> Option<u64> {
        self.inner.successor.get()
    }

    /// Revoke the lease. Idempotent: standing down twice fences once.
    ///
    /// `successor` is the generation that displaced this one when the
    /// tab learned it (a higher-generation proxy message), and `None`
    /// on an orderly close, where nobody has taken over yet.
    pub fn revoke(&self, successor: Option<u64>) {
        self.inner.revoked.set(true);
        if successor.is_some() {
            self.inner.successor.set(successor);
        }
    }

    /// Admit one outbound effect, or refuse it typed.
    ///
    /// Every call that refuses is counted, which is what makes "no
    /// further outbound effects after the fence" an assertion rather
    /// than an absence a test has to infer.
    pub fn admit(&self) -> Result<()> {
        if self.is_live() {
            return Ok(());
        }
        self.inner.refused.set(self.inner.refused.get() + 1);
        Err(self.refusal())
    }

    /// How many effects this lease has refused since it was revoked.
    #[inline]
    pub fn refused(&self) -> u64 {
        self.inner.refused.get()
    }

    /// The refusal a revoked lease produces.
    pub fn refusal(&self) -> LeafError {
        LeafError::NotLeader {
            presented: self.inner.generation,
            current: self.inner.successor.get(),
        }
    }
}

// ──────────────────────────── follower registry ────────────────────────

/// Which followers are attached and what each one declared.
///
/// The leader keeps this so a *new* leader can re-establish the
/// subscriptions **and the announcement** its followers depend on
/// (D2 "Restoration"). It is the union, not the last writer: two
/// followers wanting two channels both get theirs.
#[derive(Debug, Clone, Default)]
pub struct FollowerRegistry {
    followers: HashMap<u64, Declaration>,
}

/// One follower's declared intent: what it needs subscribed, and what
/// it needs announced.
///
/// Both travel the same way for the same reason. A capability a
/// follower announced through the session API is that tab's current
/// intent, not a one-off network operation, so a successor that does
/// not know it would silently narrow the origin's announcement at the
/// next handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Declaration {
    /// Channels this follower needs kept subscribed.
    pub subscriptions: BTreeSet<String>,
    /// Capabilities this follower needs kept announced.
    pub capabilities: BTreeSet<String>,
}

impl FollowerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach `follower` with what it declared. Re-attach replaces the
    /// declaration, which is what a follower that survived a leader
    /// change sends.
    pub fn attach(
        &mut self,
        follower: u64,
        subscriptions: impl IntoIterator<Item = String>,
        capabilities: impl IntoIterator<Item = String>,
    ) {
        self.followers.insert(
            follower,
            Declaration {
                subscriptions: subscriptions.into_iter().collect(),
                capabilities: capabilities.into_iter().collect(),
            },
        );
    }

    /// Record a channel a follower subscribed to after attaching.
    pub fn declare(&mut self, follower: u64, channel: &str) {
        self.followers
            .entry(follower)
            .or_default()
            .subscriptions
            .insert(channel.to_string());
    }

    /// Release one follower's claim on `channel`, and say whether the
    /// channel is still wanted by anyone.
    ///
    /// `true` means some other tab still declares it, so the leader
    /// keeps the membership and this caller has merely stopped being
    /// one of its consumers. `false` means this was the **last**
    /// consumer and the wire membership is now the leader's to give
    /// up.
    ///
    /// This is the whole of the last-consumer rule, in one place and
    /// returning the decision rather than performing it: one tab
    /// closing a store must not silently cancel delivery to another
    /// tab that is still reading the same channel, and a leader that
    /// unsubscribed on every release would do exactly that.
    pub fn release(&mut self, follower: u64, channel: &str) -> bool {
        if let Some(declared) = self.followers.get_mut(&follower) {
            declared.subscriptions.remove(channel);
        }
        self.followers
            .values()
            .any(|declared| declared.subscriptions.contains(channel))
    }

    /// Record the capabilities a follower announced after attaching.
    ///
    /// Replaces rather than unions: `announce(caps)` publishes a
    /// document, and its argument is the whole of what that tab wants
    /// announced. Accumulating would make a tab that dropped a tag get
    /// it back at the next handoff.
    pub fn declare_capabilities(&mut self, follower: u64, capabilities: &[String]) {
        self.followers.entry(follower).or_default().capabilities =
            capabilities.iter().cloned().collect();
    }

    /// Forget a follower.
    pub fn detach(&mut self, follower: u64) {
        self.followers.remove(&follower);
    }

    /// How many followers are attached.
    pub fn len(&self) -> usize {
        self.followers.len()
    }

    /// Whether no follower is attached.
    pub fn is_empty(&self) -> bool {
        self.followers.is_empty()
    }
    /// The union of every follower's declared subscriptions, sorted.
    ///
    /// Sorted because it drives a sequence of network operations and a
    /// nondeterministic order would make the restoration witness flaky
    /// for no reason.
    pub fn subscription_union(&self) -> Vec<String> {
        let mut union = BTreeSet::new();
        for declared in self.followers.values() {
            union.extend(declared.subscriptions.iter().cloned());
        }
        union.into_iter().collect()
    }

    /// The union of every follower's declared capabilities, sorted.
    pub fn capability_union(&self) -> Vec<String> {
        let mut union = BTreeSet::new();
        for declared in self.followers.values() {
            union.extend(declared.capabilities.iter().cloned());
        }
        union.into_iter().collect()
    }
}

/// Whether `channel` is still wanted once a claim on it has been
/// dropped: by this tab itself, or by any attached follower.
///
/// The two sets together are the whole last-consumer question, and
/// this function exists so the answer is testable without a browser.
/// The session layer that owns `own` is `wasm32`-only, so an
/// arithmetic left up there would compile — and be witnessed — only in
/// the wasm job, and "the leader cancelled a sibling tab's delivery"
/// is not a defect to discover on a runner.
pub fn membership_still_wanted(
    own: &BTreeSet<String>,
    followers: &[String],
    channel: &str,
) -> bool {
    own.contains(channel) || followers.iter().any(|held| held == channel)
}

// ──────────────────────────── proxy protocol ───────────────────────────

/// One thing a follower asks the leader to do on its behalf.
///
/// The whole follower-visible surface: there is no operation a
/// follower can perform locally that a leader performs over the mesh,
/// which is what makes the proxy an API rather than a subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderRequest {
    /// An nRPC call.
    Call {
        /// The service name.
        service: String,
        /// The request payload.
        payload: Bytes,
        /// The call deadline, when the caller set one.
        timeout_ms: Option<u32>,
    },
    /// Subscribe to a channel.
    Subscribe {
        /// The channel name.
        channel: String,
    },
    /// Release this tab's claim on a channel.
    ///
    /// Not the mirror image of [`Self::Subscribe`]: a subscribe is one
    /// tab declaring an interest, and the leader holds ONE membership
    /// for the union of them, so an unsubscribe releases a claim and
    /// only the last one released gives up the membership. The
    /// decision is [`FollowerRegistry::release`]'s.
    Unsubscribe {
        /// The channel name.
        channel: String,
    },
    /// Publish to a channel.
    Publish {
        /// The channel name.
        channel: String,
        /// The payload.
        payload: Bytes,
    },
    /// Publish this node's capability announcement.
    Announce {
        /// The capabilities to announce.
        capabilities: Vec<String>,
    },
    /// Find the nodes offering a capability.
    Query {
        /// The capability.
        capability: String,
    },
    /// Open an application stream.
    StreamOpen {
        /// The stream label.
        label: String,
        /// Reliable or fire-and-forget.
        reliability: Reliability,
        /// Used verbatim when present, so a proxied stream can match
        /// a publish contract a native handler dispatches on — the
        /// same fidelity a leader-local `open_stream` has.
        stream_id: Option<u64>,
        /// Likewise verbatim: the channel hash the stream rides.
        channel_hash: Option<u16>,
        /// The node the stream addresses; the leader's anchor when
        /// absent.
        ///
        /// Carried because a follower's stream is opened by the
        /// leader tab's node. Without it a peer-addressed stream
        /// could only be refused on a follower, which made §9's
        /// "`connectPeer` installs a direct session and this puts
        /// application bytes on it" a leader-only sentence.
        peer: Option<u64>,
    },
    /// Send on an open stream.
    StreamSend {
        /// The **backend handle** for this open, not the wire stream
        /// id.
        ///
        /// Two opens can legitimately share one wire stream id — the
        /// id is derived from a label and the peer is not part of that
        /// derivation, so one label opened to two peers is one id on
        /// two sessions (the supported composition
        /// [`crate::node::LeafEvent::StreamData`] describes). Keying
        /// the backend's open-stream table by the wire id therefore
        /// let the second open overwrite the first's slot, and this
        /// request could then send through *another peer's* stream.
        /// The handle names one open.
        handle: u64,
        /// The payload.
        payload: Bytes,
    },
    /// Close an open stream.
    StreamClose {
        /// The **backend handle** for this open — see
        /// [`Self::StreamSend`]. Closing by wire id could close a
        /// different peer's stream.
        handle: u64,
    },
    /// Sign and send a session-independent signalling envelope.
    Signal {
        /// The recipient's node id.
        peer: u64,
        /// The dialog.
        dialog: u64,
        /// The envelope kind, as the boundary spells it.
        kind: String,
        /// The SDP, candidate line or reason.
        payload: Bytes,
    },
    /// Mint an offer and start an attempt with `peer` (§9 step 3).
    ///
    /// No dialog: this is the call that *creates* one, and the
    /// answer carries it. Every later step in the attempt names it.
    PeerOffer {
        /// The peer to offer to.
        peer: u64,
    },
    /// Answer the offer `peer` already filed.
    ///
    /// Also dialogless on the way in — the dialog is read off the
    /// signed envelope the offerer minted, not chosen here.
    PeerAcceptOffer {
        /// The peer whose offer is being answered.
        peer: u64,
    },
    /// Service one polling step of an attempt: harvest and apply
    /// candidates, and report the reading.
    ///
    /// **Names its dialog**, and that is the ownership rule rather
    /// than a convenience. A proxied request is served after
    /// crossing a channel, so the attempt it was issued for may have
    /// been superseded; the leader refuses a dialog that is not the
    /// live one *before* servicing anything, so a stale request
    /// cannot drive its own replacement.
    PeerCandidate {
        /// The peer.
        peer: u64,
        /// The attempt this request belongs to.
        dialog: u64,
    },
    /// Run the Noise handshake for an attempt whose channel is open.
    ///
    /// Names its dialog for the same reason, and it matters most
    /// here: the Noise wait is the longest await on the surface, and
    /// the session it installs must belong to the attempt that
    /// negotiated the channel.
    PeerHandshake {
        /// The peer.
        peer: u64,
        /// The attempt this request belongs to.
        dialog: u64,
    },
    /// Run the enrollment exchange, if this node is not enrolled.
    ///
    /// Proxied rather than leader-only: one node per origin means one
    /// enrollment per origin, so a follower asking to enroll is
    /// asking the *only* session there is to enroll. A hole here
    /// would mean the same page code worked in the first tab and
    /// raised an unknown error in the second.
    Enroll,
    /// Whether the node has completed enrollment.
    IsEnrolled,
    /// The leaf's counters.
    Counters,

    // ───────── org-scoped calls and serves (Stage 4) ─────────
    //
    // The org transport is a **transparent envelope over the
    // existing bytes/stream envelope variants**: the request bodies,
    // response items and terminal diagnostics these ops carry are
    // the same bytes the nRPC frames carry, and every one of the four
    // call shapes resolves through `ProxyValue::Bytes` /
    // `ProxyFailure` — a new call shape costs zero proxy vocabulary.
    //
    // **Attribution, stated once and enforced everywhere below.**
    // Per-call correlation is the FOLLOWER's self-minted `call` id
    // (`correlation_seed()`-derived, monotonic across generations:
    // never a wire id, never an incarnation — a stream's resolved
    // identity comes back in `ProxyValue::Stream` and is not a
    // correlation). The gate generation rides every envelope
    // (`ProxyEnvelope::generation`) and a generation move fails every
    // pending correlation typed `LeaderLost`, never resumed; a
    // successor generation's calls carry fresh, higher ids so a late
    // reply to a dead generation's call can never land on a live one.
    // Per-follower attribution is the follower's own handle — its
    // `call` id — and the backend keys its relay state by exactly
    // that; delivering a call's items under another follower's
    // handle is the REQUIRED witness inverse and must fail.
    /// Follower → leader: open one org-protected call.
    ///
    /// The proof material crosses as opaque credential wire bytes;
    /// the leader's node mints the signed opening (the entity key
    /// never leaves the node). `shape` is `unary`,
    /// `server-streaming`, `client-streaming` or `duplex`.
    OrgCall {
        /// The follower's self-minted call id — the per-call
        /// correlation, never reused across generations.
        call: u64,
        /// The call shape.
        shape: String,
        /// The service name.
        service: String,
        /// The request body (SS) or first upload item (CS/DX).
        body: Bytes,
        /// The membership certificate wire bytes.
        membership: Bytes,
        /// The dispatcher grant wire bytes.
        dispatcher: Bytes,
        /// The capability grant wire bytes, for granted calls.
        capability_grant: Option<Bytes>,
        /// The acting org id, 64 hex.
        acting_org: String,
        /// The provider's owner org id, 64 hex.
        provider_org: String,
        /// The pinned provider entity id, 64 hex.
        provider: String,
        /// Proof TTL in seconds (`1..=30`).
        ttl_secs: u64,
        /// Absolute deadline, unix nanoseconds (`0` = none at this
        /// layer; the caller-side applies its facade default first).
        deadline_ns: u64,
        /// The unary call deadline, milliseconds.
        timeout_ms: Option<u32>,
        /// Response-direction window (SS/DX).
        stream_window_initial: Option<u32>,
        /// Upload-direction window (CS/DX).
        request_window_initial: Option<u32>,
    },
    /// Follower → leader: push one upload item (CS/DX).
    OrgSend {
        /// The per-call correlation.
        call: u64,
        /// The item bytes.
        payload: Bytes,
    },
    /// Follower → leader: half-close the upload (CS finish / DX
    /// `finishSending`).
    OrgFinishSending {
        /// The per-call correlation.
        call: u64,
    },
    /// Follower → leader: **long-pull** for the next response item.
    ///
    /// The reply is a transparent envelope in
    /// [`ProxyValue::Bytes`]: `0x00 ‖ item` for an item, `0x01 ‖
    /// body` for the success terminal (the CS aggregate or the empty
    /// `end` marker). A typed terminal answers [`ProxyBody::Failed`]
    /// with the frozen vocabulary. The pull is held until an item,
    /// a terminal, or the generation moves — and a generation move
    /// settles it `LeaderLost`, never resumed.
    OrgNext {
        /// The per-call correlation.
        call: u64,
    },
    /// Follower → leader: cancel the call (exactly one CANCEL on the
    /// mesh; the terminal is latched).
    OrgCancel {
        /// The per-call correlation.
        call: u64,
    },

    // ───────── serve-through-leader ─────────
    //
    // A follower's service registration rides the proxy registry:
    // the registration is declared to whichever tab holds the lock
    // and re-declared on every generation this follower has not yet
    // declared under (the `Attach` re-declare discipline). Inbound
    // calls dispatch to the REGISTERING follower — the registration
    // id is the follower's own handle — and teardown follows the
    // existing retire discipline: `ProxyServer::retire`
    // fence → shutdown → `LeaderLost`, and nothing here is ever
    // resurrected.
    /// Follower → leader: register one org service whose handler
    /// runs in this follower.
    OrgServeRegister {
        /// The follower's self-minted registration id.
        registration: u64,
        /// The service name.
        service: String,
        /// `same-org` or `granted`.
        access: String,
        /// The provider's owner org id, 64 hex.
        owner_org: String,
        /// The served shape (as [`Self::OrgCall`]'s `shape`).
        shape: String,
    },
    /// Follower → leader: **long-pull** for the next admitted call on
    /// this registration.
    ///
    /// The reply is `0x03 ‖ <JSON>` carrying `{ call }` — the
    /// leader-minted bridge handle for the admitted call. The verified
    /// `OrgCaller` projection is deliberately NOT carried here: it is
    /// resolved from the leader's own [`crate::rpc_serve::ServeCall`]
    /// state through [`Self::OrgServeCaller`], keyed by that handle
    /// (LEAF-6: a projection reaching a handler is produced under the
    /// leader's verified serve state, never taken on an envelope
    /// claim). Admitted calls are dispatched to the registering
    /// follower's handle and to no other.
    OrgServeAccept {
        /// The registration this pull waits on.
        registration: u64,
    },
    /// Follower → leader: the verified `OrgCaller` projection for one
    /// served call.
    ///
    /// Answers [`ProxyValue::Text`] with the projection built from
    /// the leader's own [`crate::rpc_serve::ServeCall::caller`] —
    /// never an echo of anything a follower sent. Sender-bound: only
    /// the follower that owns the registration the call was admitted
    /// to can resolve it, so a guessed or foreign handle resolves to
    /// nothing.
    OrgServeCaller {
        /// The served call's correlation.
        call: u64,
    },
    /// Follower → leader: **long-pull** for the next request item of
    /// one served call (`0x00 ‖ item`, then `0x01` at EOF).
    OrgServeRequest {
        /// The served call's correlation.
        call: u64,
    },
    /// Follower → leader: push one response item for a served call.
    OrgServeSend {
        /// The served call's correlation.
        call: u64,
        /// The item bytes.
        payload: Bytes,
    },
    /// Follower → leader: complete one served call.
    ///
    /// `status` is the terminal `RpcStatus` wire value (`0` = `Ok`,
    /// whose terminal is the `end` marker); `message` is the
    /// diagnostic body for a non-`Ok` terminal.
    OrgServeFinish {
        /// The served call's correlation.
        call: u64,
        /// The terminal status.
        status: u16,
        /// The diagnostic body (ignored for `Ok`).
        message: String,
    },
    /// Follower → leader: **long-pull** for one served call's
    /// retirement signal (`0x02 ‖ <reason>` with the frozen
    /// `OrgRetireReason` string).
    OrgServeRetired {
        /// The served call's correlation.
        call: u64,
    },
    /// Follower → leader: close a registration (C9's protected
    /// split — its live calls retire with their exact terminals and
    /// new openings are refused).
    ///
    /// **Owned registrations only (LEAF-4).** The leader unregisters
    /// by the service name IT recorded when this follower registered
    /// — a request naming another service (or another follower's
    /// registration) affects nothing but its own typed refusal.
    OrgServeUnregister {
        /// The registration.
        registration: u64,
    },
}

/// What the leader answers a request with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyValue {
    /// Bytes: an nRPC reply, or empty for an operation that only
    /// succeeds or fails.
    Bytes(Bytes),
    /// Text: the `query` and `counters` JSON documents.
    Text(String),
    /// A yes-or-no answer (`is_enrolled`).
    Flag(bool),
    /// A stream was opened.
    ///
    /// Carries the **resolved** identity of the stream the leader's
    /// node actually opened, read back off that stream rather than
    /// echoed from the request's options: a follower asking for a
    /// peer it may not get, or asking for none at all, must still
    /// learn which authenticated peer its bytes will come from and go
    /// to. Without this a proxied handle cannot filter by peer, and a
    /// peer-blind filter is the R4-10 defect.
    Stream {
        /// The backend handle that owns this open
        /// ([`LeaderRequest::StreamSend`]).
        handle: u64,
        /// The wire stream id, which is what the application filters
        /// on and which two peers may share.
        stream_id: u64,
        /// The authenticated peer this stream is with.
        peer: u64,
        /// The incarnation of the session it was opened on.
        incarnation: u64,
    },
}

/// Why a proxied request failed.
///
/// Two cases, and the split is not laziness — it is the answer to
/// "who owns the taxonomy":
///
/// - [`ProxyFailure::Typed`] is a failure the lifecycle itself
///   produced: the fence refusing a stale generation, leadership
///   moving under an in-flight call, a leader standing down. Those
///   are the ones D2 puts observable requirements on, so they cross
///   the channel as a machine tag plus fields and arrive as the same
///   [`LeafError`] value the leader constructed.
/// - [`ProxyFailure::Reported`] is a failure that came out of the
///   node across the `wasm_bindgen` boundary, where the only thing
///   left of it is the message `LeafError`'s `Display` produced.
///   `@net-mesh/browser` already re-types exactly that text for a
///   *local* call, so it is carried verbatim and re-typed by the same
///   parser. A second parser in Rust would be a second taxonomy to
///   keep in step with the first, and the two would drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyFailure {
    /// A failure the proxy typed itself.
    Typed(LeafError),
    /// A failure carried as the text the boundary produced.
    Reported(String),
}

impl ProxyFailure {
    /// The message a follower's rejection carries — identical to what
    /// a local call would have thrown, in both cases.
    pub fn message(&self) -> String {
        match self {
            Self::Typed(error) => error.to_string(),
            Self::Reported(text) => text.clone(),
        }
    }

    /// The typed error, when the lifecycle produced it.
    pub fn typed(&self) -> Option<&LeafError> {
        match self {
            Self::Typed(error) => Some(error),
            Self::Reported(_) => None,
        }
    }
}

/// How a proxied request settled.
///
/// Not [`crate::error::Result`]: the error side is a
/// [`ProxyFailure`], because some failures cross the channel as a
/// typed value and some as the text the `wasm_bindgen` boundary
/// produced, and collapsing the two would mean inventing a type for
/// the second.
pub type ProxyOutcome = core::result::Result<ProxyValue, ProxyFailure>;

/// Who an envelope came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxySide {
    /// The tab holding the lock.
    Leader,
    /// An attached follower, by the id it minted for itself.
    Follower(u64),
}

/// One message on the proxy channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyBody {
    /// Follower → leader: "I am here, and these are the channels I
    /// depend on."
    ///
    /// The one body exempt from the fence: a tab that has just opened
    /// does not know the current generation, and this is the message
    /// that tells it.
    Attach {
        /// The channels this follower wants kept subscribed.
        subscriptions: Vec<String>,
        /// The capabilities this follower wants kept announced — its
        /// constructor's list plus anything a later `announce()`
        /// declared, so a successor restores the current intent and
        /// not one tab's opening options.
        capabilities: Vec<String>,
    },
    /// Follower → leader: "I am going away."
    Detach,
    /// Follower → leader: do this, and answer under `correlation`.
    Request {
        /// The follower's correlation id.
        correlation: u64,
        /// What to do.
        ///
        /// Boxed: `LeaderRequest` is itself the largest thing this
        /// channel carries (`OrgCall` alone holds every credential
        /// and the request body), and an unboxed one would make every
        /// `ProxyBody` — replies and broadcasts included — pay its
        /// ~300 bytes.
        request: Box<LeaderRequest>,
    },
    /// Leader → all: "I hold the lock at this generation."
    Leadership {
        /// The leader's node id.
        node_id: u64,
    },
    /// Leader → follower: the answer to a request.
    Reply {
        /// The follower's correlation id.
        correlation: u64,
        /// The value.
        value: ProxyValue,
    },
    /// Leader → follower: the request failed, typed.
    Failed {
        /// The follower's correlation id.
        correlation: u64,
        /// Why.
        failure: ProxyFailure,
    },
    /// Leader → all: a subscription was (re-)established.
    ///
    /// D2's "the follower is told it was re-established" — a follower
    /// that survived a leader change needs to know its channel is
    /// live again, and a silent re-subscribe would leave it guessing.
    Restored {
        /// The channel.
        channel: String,
    },
    /// Leader → all: one node event, verbatim JSON.
    Event {
        /// [`crate::node::LeafEvent::to_json`]'s output.
        json: String,
    },
    /// Leader → all: the work of `lost` is dead. Sent by a leader
    /// that is standing down in an orderly way.
    LeaderLost {
        /// The generation whose in-flight work just failed.
        lost: u64,
    },
}

/// A proxy message with its provenance and its fenced generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEnvelope {
    /// The generation the sender holds, or believes it holds. Present
    /// on every message without exception: that is the fence.
    pub generation: u64,
    /// Who sent it.
    pub from: ProxySide,
    /// What it says.
    pub body: ProxyBody,
}

impl ProxyEnvelope {
    /// Encode to the JSON one `BroadcastChannel` message carries.
    ///
    /// `u64`s are decimal strings throughout: a structured-clone of a
    /// JS number would round a generation or a stream id above 2^53,
    /// and a rounded generation is a fence that silently stops
    /// fencing.
    pub fn to_json(&self) -> String {
        let mut map = Map::new();
        map.insert("v".into(), Value::from(PROXY_VERSION.to_string()));
        map.insert(
            "generation".into(),
            Value::from(self.generation.to_string()),
        );
        match self.from {
            ProxySide::Leader => {
                map.insert("from".into(), Value::from("leader"));
            }
            ProxySide::Follower(id) => {
                map.insert("from".into(), Value::from("follower"));
                map.insert("follower".into(), Value::from(id.to_string()));
            }
        }
        match &self.body {
            ProxyBody::Attach {
                subscriptions,
                capabilities,
            } => {
                map.insert("kind".into(), Value::from("attach"));
                map.insert("subscriptions".into(), strings(subscriptions));
                map.insert("capabilities".into(), strings(capabilities));
            }
            ProxyBody::Detach => {
                map.insert("kind".into(), Value::from("detach"));
            }
            ProxyBody::Request {
                correlation,
                request,
            } => {
                map.insert("kind".into(), Value::from("request"));
                map.insert("correlation".into(), Value::from(correlation.to_string()));
                map.insert("request".into(), encode_request(request));
            }
            ProxyBody::Leadership { node_id } => {
                map.insert("kind".into(), Value::from("leadership"));
                map.insert("node_id".into(), Value::from(node_id.to_string()));
            }
            ProxyBody::Reply { correlation, value } => {
                map.insert("kind".into(), Value::from("reply"));
                map.insert("correlation".into(), Value::from(correlation.to_string()));
                map.insert("value".into(), encode_value(value));
            }
            ProxyBody::Failed {
                correlation,
                failure,
            } => {
                map.insert("kind".into(), Value::from("failed"));
                map.insert("correlation".into(), Value::from(correlation.to_string()));
                map.insert("error".into(), encode_failure(failure));
            }
            ProxyBody::Restored { channel } => {
                map.insert("kind".into(), Value::from("restored"));
                map.insert("channel".into(), Value::from(channel.clone()));
            }
            ProxyBody::Event { json } => {
                map.insert("kind".into(), Value::from("event"));
                map.insert("event".into(), Value::from(json.clone()));
            }
            ProxyBody::LeaderLost { lost } => {
                map.insert("kind".into(), Value::from("leader_lost"));
                map.insert("lost".into(), Value::from(lost.to_string()));
            }
        }
        Value::Object(map).to_string()
    }

    /// Parse one proxy message.
    pub fn from_json(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| LeafError::ControlPlane(format!("proxy message is not JSON: {e}")))?;
        let version = u64_field(&value, "v")?;
        if version != PROXY_VERSION {
            return Err(LeafError::ControlPlane(format!(
                "proxy protocol version {version} is not {PROXY_VERSION}"
            )));
        }
        let generation = u64_field(&value, "generation")?;
        let from = match str_field(&value, "from")? {
            "leader" => ProxySide::Leader,
            "follower" => ProxySide::Follower(u64_field(&value, "follower")?),
            other => {
                return Err(LeafError::ControlPlane(format!(
                    "proxy message from unknown side {other:?}"
                )))
            }
        };
        let body = match str_field(&value, "kind")? {
            "attach" => ProxyBody::Attach {
                subscriptions: string_list(&value, "subscriptions")?,
                capabilities: string_list(&value, "capabilities")?,
            },
            "detach" => ProxyBody::Detach,
            "request" => ProxyBody::Request {
                correlation: u64_field(&value, "correlation")?,
                request: Box::new(decode_request(field(&value, "request")?)?),
            },
            "leadership" => ProxyBody::Leadership {
                node_id: u64_field(&value, "node_id")?,
            },
            "reply" => ProxyBody::Reply {
                correlation: u64_field(&value, "correlation")?,
                value: decode_value(field(&value, "value")?)?,
            },
            "failed" => ProxyBody::Failed {
                correlation: u64_field(&value, "correlation")?,
                failure: decode_failure(field(&value, "error")?)?,
            },
            "restored" => ProxyBody::Restored {
                channel: str_field(&value, "channel")?.to_string(),
            },
            "event" => ProxyBody::Event {
                json: str_field(&value, "event")?.to_string(),
            },
            "leader_lost" => ProxyBody::LeaderLost {
                lost: u64_field(&value, "lost")?,
            },
            other => {
                return Err(LeafError::ControlPlane(format!(
                    "unknown proxy message kind {other:?}"
                )))
            }
        };
        Ok(Self {
            generation,
            from,
            body,
        })
    }
}

// ───────────────────────────── the carriers ────────────────────────────

/// The proxy's carrier. One method, because that is all a
/// `BroadcastChannel` is.
///
/// A seam rather than a hard dependency so both ends of the proxy are
/// testable without a browser — which is the difference between a
/// reviewable lifecycle and one that only runs.
pub trait ProxyTransport {
    /// Post one encoded envelope to the other tabs on the channel.
    fn post(&self, message: &str);
}

/// A recording transport: what was posted, in order.
///
/// Not test-only scaffolding — it is also how a page can trace the
/// proxy, and it is what makes the native lifecycle tests possible.
#[derive(Debug, Default)]
pub struct RecordingTransport {
    posted: std::cell::RefCell<Vec<String>>,
}

impl RecordingTransport {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every message posted so far, decoded.
    pub fn envelopes(&self) -> Vec<ProxyEnvelope> {
        self.posted
            .borrow()
            .iter()
            .map(|text| ProxyEnvelope::from_json(text).expect("this end encoded it"))
            .collect()
    }

    /// The raw posted text, in order.
    pub fn raw(&self) -> Vec<String> {
        self.posted.borrow().clone()
    }

    /// Forget everything posted so far.
    pub fn clear(&self) {
        self.posted.borrow_mut().clear();
    }
}

impl ProxyTransport for RecordingTransport {
    fn post(&self, message: &str) {
        self.posted.borrow_mut().push(message.to_string());
    }
}

// ──────────────────────────── the leader side ──────────────────────────

/// The one-shot answer channel for a request.
///
/// Consumes itself on use, so a double reply is a compile error rather
/// than a duplicate correlation. Dropping it without answering sends a
/// typed failure instead of leaving the caller's promise pending
/// forever — a leader that forgets a request must not look like a slow
/// one.
///
/// Two sinks, one backend. A follower's request is answered onto the
/// channel; the leader's **own** operations go through the same
/// [`LeaderBackend`] and are answered straight back to the caller, so
/// a leader-local call does not broadcast its reply to every other tab
/// on the origin.
pub struct Replier {
    sink: Option<ReplySink>,
    lease: GenerationLease,
    correlation: u64,
}

enum ReplySink {
    Channel(Rc<dyn ProxyTransport>),
    Local(oneshot::Sender<ProxyOutcome>),
}

impl Replier {
    /// Answer with bytes: an nRPC reply, or empty for an operation
    /// that only succeeds or fails.
    pub fn bytes(mut self, payload: Bytes) {
        self.send(ProxyBody::Reply {
            correlation: self.correlation,
            value: ProxyValue::Bytes(payload),
        });
    }

    /// Answer with a JSON document (`query`, `counters`).
    pub fn text(mut self, text: String) {
        self.send(ProxyBody::Reply {
            correlation: self.correlation,
            value: ProxyValue::Text(text),
        });
    }

    /// Answer yes or no (`is_enrolled`).
    pub fn flag(mut self, flag: bool) {
        self.send(ProxyBody::Reply {
            correlation: self.correlation,
            value: ProxyValue::Flag(flag),
        });
    }

    /// Answer with an opened stream: its owning handle, its wire id,
    /// and the authenticated identity it resolved to.
    pub fn stream(mut self, handle: u64, stream_id: u64, peer: u64, incarnation: u64) {
        self.send(ProxyBody::Reply {
            correlation: self.correlation,
            value: ProxyValue::Stream {
                handle,
                stream_id,
                peer,
                incarnation,
            },
        });
    }

    /// Answer with a failure.
    pub fn fail(mut self, failure: ProxyFailure) {
        self.send(ProxyBody::Failed {
            correlation: self.correlation,
            failure,
        });
    }

    /// Answer the leader's own caller, not the channel.
    ///
    /// The correlation is `0`: a local answer is delivered to one
    /// waiting future, so there is nothing to correlate against.
    pub fn local(answer: oneshot::Sender<ProxyOutcome>, lease: GenerationLease) -> Self {
        Self {
            sink: Some(ReplySink::Local(answer)),
            lease,
            correlation: 0,
        }
    }

    /// The generation this answer was admitted under.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.lease.generation()
    }

    fn send(&mut self, body: ProxyBody) {
        let Some(sink) = self.sink.take() else {
            return;
        };
        // THE fence, on the emitting side. An operation admitted
        // before stand-down can still reach this line afterwards —
        // it was parked on an await, and the reply is the first thing
        // it does when it resumes. Its answer is not this tab's to
        // give any more, so what the caller gets is the typed reason
        // and not a success produced by a node that no longer holds
        // the identity.
        let fenced = self.lease.admit().is_err();
        let body = if fenced {
            ProxyBody::Failed {
                correlation: self.correlation,
                failure: ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                    generation: self.lease.generation(),
                })),
            }
        } else {
            body
        };
        match sink {
            ReplySink::Channel(transport) => {
                // A follower's correlation is already dead by the time
                // the fence fires: stand-down broadcasts `LeaderLost`
                // before it closes the node, and the follower's
                // pending map is drained by that message. Posting a
                // second answer would be a message from a tab that
                // has stood down — which is exactly what the fence
                // exists to stop — so a fenced channel answer is
                // counted and dropped, not sent.
                if !fenced {
                    transport.post(
                        &ProxyEnvelope {
                            generation: self.lease.generation(),
                            from: ProxySide::Leader,
                            body,
                        }
                        .to_json(),
                    );
                }
            }
            ReplySink::Local(answer) => {
                let outcome = match body {
                    ProxyBody::Reply { value, .. } => Ok(value),
                    ProxyBody::Failed { failure, .. } => Err(failure),
                    // `send` is private and every call site above
                    // passes one of those two.
                    other => unreachable!("a replier cannot send {other:?}"),
                };
                let _ = answer.send(outcome);
            }
        }
    }
}

impl Replier {
    /// A replier whose answer is delivered locally, for a test.
    ///
    /// Test-only: it exists so the shared stream dispatch
    /// ([`crate::stream_ownership::answer_stream_request`]) can be
    /// witnessed natively — the refusal paths in particular, which a
    /// browser fixture holding an infallible stream type cannot reach.
    #[cfg(test)]
    pub(crate) fn local_for_test(generation: u64) -> (Self, oneshot::Receiver<ProxyOutcome>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                sink: Some(ReplySink::Local(tx)),
                lease: GenerationLease::new(generation),
                correlation: 0,
            },
            rx,
        )
    }
}

impl Drop for Replier {
    /// A replier dropped without an answer is a lost correlation, and
    /// a follower's promise that never settles is worse than a
    /// failure: it looks like latency forever. So dropping answers.
    ///
    /// Which failure depends on the fence, and that is how stand-down
    /// cancels work: dropping the future an operation was parked in
    /// drops the replier it moved, and a replier whose lease has been
    /// revoked settles its caller with `LeaderLost` for the generation
    /// that admitted the operation — once, because the sink is taken.
    fn drop(&mut self) {
        let correlation = self.correlation;
        self.send(ProxyBody::Failed {
            correlation,
            failure: ProxyFailure::Typed(LeafError::Rpc(RpcError::SessionLost)),
        });
    }
}

/// What the leader's node can do on a follower's behalf.
///
/// The seam between the lifecycle and the node:
/// `crate::leader_session::MeshSession` wires the real
/// `crate::wasm::LeafNode` into it, and the native
/// lifecycle tests wire a recording double, which is how "close the
/// leader, pending calls fail typed, subscriptions are restored" is
/// proven without an anchor.
pub trait LeaderBackend {
    /// The leader's node id.
    fn node_id(&self) -> u64;

    /// Perform one request on behalf of `from`, answering through
    /// `reply`.
    ///
    /// `from` is the requesting side — the follower's own
    /// [`ProxySide::Follower`] id as it attached, or
    /// [`ProxySide::Leader`] for this tab's local operations. It is
    /// the sender half of handle attribution (LEAF-5): every handle a
    /// request names resolves only within that sender's own
    /// namespace, so a request naming a guessed or colliding id owns
    /// nothing rather than another follower's call.
    ///
    /// The replier is taken by value and consumed by whichever
    /// answer it gets, so a double answer is a compile error and a
    /// forgotten one is caught by its `Drop`. An implementation that
    /// needs to await something moves the replier into the future.
    fn perform(&mut self, from: ProxySide, request: LeaderRequest, reply: Replier);

    /// Retire the node this backend owns, and say how many of its
    /// pending calls that failed.
    ///
    /// Called by [`ProxyServer::retire`] **after** the generation has
    /// been fenced and **before** the lock is released. Three things
    /// are owed, and the fence is what makes them finite: close the
    /// transport so the node emits nothing further, let go of the
    /// operations that were admitted under the fenced generation (the
    /// replier each one carries settles its caller as it is dropped),
    /// and fail the node's own pending calls once with a typed error.
    ///
    /// The count is the observable half. "Pending calls fail exactly
    /// once" is a claim, and a number a witness can read is the
    /// difference between checking it and believing it.
    fn shutdown(&mut self, generation: u64) -> usize;
}

/// A boxed backend is a backend.
///
/// The session holds `Box<dyn LeaderBackend>` so the node can be
/// supplied by a factory — which is what lets the lifecycle be driven
/// without one. Without it the boxing would force the server to be
/// generic over a concrete node type, and a `wasm_bindgen` type cannot
/// be generic.
impl LeaderBackend for Box<dyn LeaderBackend> {
    fn node_id(&self) -> u64 {
        (**self).node_id()
    }

    fn perform(&mut self, from: ProxySide, request: LeaderRequest, reply: Replier) {
        (**self).perform(from, request, reply);
    }

    fn shutdown(&mut self, generation: u64) -> usize {
        (**self).shutdown(generation)
    }
}

/// What retiring a leader did, so a caller can assert on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retirement {
    /// The generation that was fenced.
    pub generation: u64,
    /// How many of the node's pending calls the backend failed.
    pub pending_failed: usize,
    /// Whether this call is the one that fenced the generation, as
    /// opposed to a second stand-down finding it already fenced.
    pub fenced_here: bool,
}

/// How many recently-delivered request correlations the at-most-once
/// window remembers.
const REQUEST_DEDUP_WINDOW: usize = 1024;

/// The bounded at-most-once window over request deliveries (LEAF-12).
///
/// A request's `correlation` is its idempotence key: one follower
/// mints each correlation exactly once ([`ProxyClient::issue`]), so a
/// second delivery of the same `(follower, correlation)` pair is a
/// **replayed envelope** — the same bytes re-posted at the channel —
/// and not a second request. Without a window, replaying a captured
/// `OrgSend`/`OrgServeSend` envelope re-executes the verb: a
/// duplicated stream item at the provider, a duplicated response
/// item to the caller (terminals and CANCEL are latched, which is
/// why only the non-idempotent verbs felt it).
///
/// The window is bounded on purpose: the newest
/// [`REQUEST_DEDUP_WINDOW`] keys are remembered and older ones
/// forgotten, so a long-lived leader pays O(window) memory and every
/// replay inside the window is answered once (the first reply stands)
/// and never re-performed.
#[derive(Default)]
struct RequestDedup {
    seen: HashSet<(u64, u64)>,
    order: VecDeque<(u64, u64)>,
    /// How many deliveries were refused as replays — observable so a
    /// witness can tell "not re-performed" from "never arrived".
    replayed: u64,
}

impl RequestDedup {
    /// Record one request delivery. `false` when this exact request
    /// was already delivered inside the window (a replay).
    fn admit(&mut self, follower: u64, correlation: u64) -> bool {
        let key = (follower, correlation);
        if !self.seen.insert(key) {
            self.replayed += 1;
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > REQUEST_DEDUP_WINDOW {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

/// The leader's half of the proxy.
pub struct ProxyServer<B: LeaderBackend> {
    backend: B,
    transport: Rc<dyn ProxyTransport>,
    lease: GenerationLease,
    followers: FollowerRegistry,
    /// The at-most-once window over request deliveries (LEAF-12).
    dedup: RequestDedup,
    stamped: u64,
    superseded: Option<u64>,
    /// Follower announcements parked on the next authoritative union
    /// publication. See [`Self::take_pending_announcements`].
    pending_announcements: Vec<Replier>,
    /// Follower releases parked on the last-consumer decision, with
    /// the channel each one asked to leave.
    ///
    /// Parked for the same reason announcements are: the answer
    /// depends on state this server cannot see. A release is only the
    /// last one if the leader tab's **own** declarations do not carry
    /// the channel either, and `declared` lives a layer up. See
    /// [`Self::take_pending_releases`].
    pending_releases: Vec<(String, Replier)>,
    closed: bool,
}

impl<B: LeaderBackend> ProxyServer<B> {
    /// A server for `generation`, posting on `transport`, with a fresh
    /// lease for that generation.
    pub fn new(backend: B, transport: Rc<dyn ProxyTransport>, generation: u64) -> Self {
        Self::with_lease(backend, transport, GenerationLease::new(generation))
    }

    /// A server over a lease the caller already holds.
    ///
    /// The lifecycle needs this: the backend is connected *before* the
    /// server exists, and the operations it spawns have to be admitted
    /// under the same lease this server's replies are. A lease created
    /// here would be a second fence, and two fences are none.
    pub fn with_lease(
        backend: B,
        transport: Rc<dyn ProxyTransport>,
        lease: GenerationLease,
    ) -> Self {
        Self {
            backend,
            transport,
            lease,
            followers: FollowerRegistry::new(),
            dedup: RequestDedup::default(),
            stamped: 0,
            superseded: None,
            pending_announcements: Vec::new(),
            pending_releases: Vec::new(),
            closed: false,
        }
    }

    /// The lease every effect of this leader is admitted under.
    pub fn lease(&self) -> GenerationLease {
        self.lease.clone()
    }

    /// Retire this leader, in the order D2 requires.
    ///
    /// 1. **Fence the generation.** The lease is revoked first, so
    ///    from here on a reply, a resumed operation or the node's
    ///    ticker is refused rather than emitted — including the ones
    ///    admitted a microtask ago.
    /// 2. **Close the node.** The backend cancels the operations the
    ///    fenced generation admitted and fails the node's pending
    ///    calls once, typed.
    /// 3. **Tell the followers**, so their pending work fails against
    ///    the generation that owned it rather than against the
    ///    successor's.
    ///
    /// Releasing the lock is deliberately **not** here: the lock is a
    /// browser object this module cannot see. What is here is the
    /// guarantee that it is the *last* step — everything above has
    /// already happened by the time this returns, so a caller whose
    /// next statement drops the lock cannot get the order wrong.
    pub fn retire(&mut self, successor: Option<u64>) -> Retirement {
        let generation = self.lease.generation();
        let fenced_here = self.lease.is_live();
        self.lease.revoke(successor);
        let pending_failed = self.backend.shutdown(generation);
        self.closed = true;
        // Announced last, and announced even when the lease was
        // already revoked: a follower that never heard `LeaderLost`
        // would keep a promise pending until the successor's higher
        // generation happened to arrive.
        self.post_fenced(ProxyBody::LeaderLost { lost: generation });
        Retirement {
            generation,
            pending_failed,
            fenced_here,
        }
    }

    /// The generation this leader holds.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.lease.generation()
    }

    /// The node id this leader runs.
    #[inline]
    pub fn node_id(&self) -> u64 {
        self.backend.node_id()
    }

    /// Stop serving.
    ///
    /// A leader that has stood down must not keep answering: a
    /// follower whose request it served after losing the lock would
    /// have been served by a tab that no longer holds the identity.
    /// After this, inbound messages are refused rather than ignored,
    /// so the follower gets a typed answer instead of silence.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Whether this server has stood down.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// How many messages this leader has stamped.
    ///
    /// Observable on purpose: "the generation is stamped on every
    /// message" is a claim, and this is the counter that lets a test
    /// hold it to account.
    #[inline]
    pub fn stamped(&self) -> u64 {
        self.stamped
    }

    /// How many request deliveries this leader refused as replays.
    ///
    /// Observable for the same reason [`Self::stamped`] is: "a
    /// replayed envelope is performed exactly once" is a claim, and
    /// this is the count that says the second delivery was seen —
    /// and dropped rather than re-executed.
    #[inline]
    pub fn replayed(&self) -> u64 {
        self.dedup.replayed
    }

    /// The generation that superseded this leader, once it has seen
    /// one. A live-but-obsolete tab learns it here.
    #[inline]
    pub fn superseded(&self) -> Option<u64> {
        self.superseded
    }

    /// How many followers are attached.
    #[inline]
    pub fn followers(&self) -> usize {
        self.followers.len()
    }

    /// The union of the followers' declared subscriptions — what a new
    /// leader re-subscribes (D2 "Restoration").
    pub fn restoration(&self) -> Vec<String> {
        self.followers.subscription_union()
    }

    /// The union of the followers' declared capabilities — what a new
    /// leader re-announces beside its own.
    pub fn capability_restoration(&self) -> Vec<String> {
        self.followers.capability_union()
    }

    /// Take the follower announcements waiting on the origin's next
    /// authoritative union publication, so their callers can be
    /// settled with its actual outcome.
    ///
    /// The counterpart of the `Announce` arm in [`Self::on_message`]:
    /// a follower's declaration is recorded here and published there,
    /// and this is the handoff between the two.
    pub fn take_pending_announcements(&mut self) -> Vec<Replier> {
        core::mem::take(&mut self.pending_announcements)
    }

    /// Take the parked releases, each with the channel it asked to
    /// leave.
    ///
    /// The caller decides: it can see this tab's own declarations as
    /// well as [`Self::restoration`], and only the two together say
    /// whether a release was the last one. Each replier must be
    /// settled — dropping one answers it typed, which is the right
    /// failure but the wrong answer.
    pub fn take_pending_releases(&mut self) -> Vec<(String, Replier)> {
        core::mem::take(&mut self.pending_releases)
    }

    /// The backend, for the leader's own local operations.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Tell every tab who holds the lock.
    pub fn announce_leadership(&mut self) {
        let node_id = self.backend.node_id();
        self.post(ProxyBody::Leadership { node_id });
    }

    /// Tell every tab a subscription is live again.
    pub fn announce_restored(&mut self, channel: &str) {
        self.post(ProxyBody::Restored {
            channel: channel.to_string(),
        });
    }

    /// Proxy one node event to the followers.
    pub fn broadcast_event(&mut self, json: &str) {
        self.post(ProxyBody::Event {
            json: json.to_string(),
        });
    }

    /// Handle one inbound message.
    ///
    /// Returns `Err` when the message was refused, so a caller can
    /// count refusals; the refusal has already been sent to the
    /// follower when it had a correlation to answer.
    pub fn on_message(&mut self, text: &str) -> Result<()> {
        let envelope = ProxyEnvelope::from_json(text)?;

        // Stood down. A follower's request gets the typed refusal
        // rather than silence: this tab no longer holds the identity,
        // and serving it would be exactly the stale-leader action the
        // fence exists to stop.
        if self.closed {
            let error = LeafError::NotLeader {
                presented: envelope.generation,
                current: self.superseded,
            };
            if let ProxyBody::Request { correlation, .. } = &envelope.body {
                let correlation = *correlation;
                self.replier(correlation)
                    .fail(ProxyFailure::Typed(error.clone()));
            }
            return Err(error);
        }

        // Another leader. If it is ahead of us we have been
        // superseded and must say so out loud rather than keep
        // serving; if it is behind, it is a stale tab and its
        // messages are not ours to act on either way.
        if envelope.from == ProxySide::Leader {
            if envelope.generation > self.generation() {
                self.superseded = Some(envelope.generation);
            }
            return Ok(());
        }
        let ProxySide::Follower(follower) = envelope.from else {
            unreachable!("the leader arm returned above")
        };

        match envelope.body {
            // `Attach` is the fence's one exemption: a tab that just
            // opened has no generation yet, and this is the message
            // that gets it one.
            ProxyBody::Attach {
                subscriptions,
                capabilities,
            } => {
                self.followers.attach(follower, subscriptions, capabilities);
                self.announce_leadership();
                Ok(())
            }
            ProxyBody::Detach => {
                self.followers.detach(follower);
                Ok(())
            }
            ProxyBody::Request {
                correlation,
                request,
            } => {
                if envelope.generation > self.generation() {
                    self.superseded = Some(envelope.generation);
                }
                if envelope.generation != self.generation() {
                    let error = LeafError::NotLeader {
                        presented: envelope.generation,
                        current: Some(self.generation()),
                    };
                    self.replier(correlation)
                        .fail(ProxyFailure::Typed(error.clone()));
                    return Err(error);
                }
                // # At-most-once under replay (LEAF-12)
                //
                // A request's `correlation` is its idempotence key:
                // one follower mints each correlation exactly once, so
                // a second delivery of this exact `(follower,
                // correlation)` is a REPLAYED envelope — the same
                // bytes re-posted at the channel — and not a second
                // request. Re-performing it would duplicate the verb's
                // effect (a second `OrgSend` item at the provider, a
                // second `OrgServeSend` response item), so it is
                // counted and dropped: the first delivery's reply
                // stands and nothing re-executes.
                if !self.dedup.admit(follower, correlation) {
                    return Ok(());
                }
                // A declaration, not just an operation. Both of these
                // are that follower's standing intent, and a successor
                // that only learned the operations would restore the
                // channels and quietly narrow the announcement.
                if let LeaderRequest::Subscribe { channel } = &*request {
                    self.followers.declare(follower, channel);
                }
                // A release is bookkeeping HERE and a decision
                // elsewhere, and the split is the whole point.
                //
                // Dropping the follower's claim is safe and necessary:
                // a successor must not restore a subscription the tab
                // has given up. Acting on it is NOT safe from here.
                // This server sees its followers' declarations and
                // **not** the leader tab's own `declared` set, so a
                // follower releasing a channel the leader's own page
                // is still reading would look like the last consumer
                // and cancel the leader's delivery. The last-consumer
                // decision needs both sets, so it belongs one layer up
                // where `declared` and `restoration()` are both
                // visible — the same place the announcement union is
                // computed, and for the same reason.
                //
                // So the claim is dropped here and the caller is
                // parked on the decision, exactly as an `Announce` is
                // parked on the publication that actually happens.
                // What the follower is told is the outcome of the real
                // decision, not a success for an operation nobody
                // made.
                if let LeaderRequest::Unsubscribe { channel } = &*request {
                    self.followers.release(follower, channel);
                    let reply = self.replier(correlation);
                    self.pending_releases.push((channel.clone(), reply));
                    return Ok(());
                }
                // # A follower does not publish the document
                //
                // An `Announce` performed verbatim publishes *that
                // tab's* list, and the announcement is the origin's
                // union — this leader's own intent beside every
                // attached follower's. Performing the raw request
                // withdrew the leader's live capabilities, and the
                // union publisher could not repair it: its cache
                // still recorded the union as the published
                // document, so a follower re-declaring the intent it
                // already held narrowed the network indefinitely.
                //
                // The declaration is recorded, and the caller is
                // parked on the publication that actually happens.
                // Its completion therefore still means what it says:
                // the outcome the follower gets is the union
                // publication's, not a success for a document that
                // was never the one published. A retirement that
                // drops these repliers settles each of them typed,
                // like any other admitted operation.
                if let LeaderRequest::Announce { capabilities } = &*request {
                    self.followers.declare_capabilities(follower, capabilities);
                    let reply = self.replier(correlation);
                    self.pending_announcements.push(reply);
                    return Ok(());
                }
                let reply = self.replier(correlation);
                self.backend
                    .perform(ProxySide::Follower(follower), *request, reply);
                Ok(())
            }
            // A follower does not send these.
            other => Err(LeafError::ControlPlane(format!(
                "a follower sent a leader-only body: {other:?}"
            ))),
        }
    }

    fn replier(&mut self, correlation: u64) -> Replier {
        self.stamped += 1;
        Replier {
            sink: Some(ReplySink::Channel(self.transport.clone())),
            lease: self.lease.clone(),
            correlation,
        }
    }

    /// Post one leader message, unless the generation has been
    /// fenced.
    ///
    /// The fence covers what this leader *says* as well as what its
    /// node does: a `Leadership`, `Restored` or `Event` broadcast from
    /// a tab that has stood down is a stale leader talking, and a
    /// follower would have to distrust it on arrival instead of never
    /// receiving it.
    fn post(&mut self, body: ProxyBody) {
        if self.lease.admit().is_err() {
            return;
        }
        self.post_fenced(body);
    }

    /// Post one message the fence does not gate.
    ///
    /// Exactly one body needs this, and it is the one that *publishes*
    /// the fence: a stand-down's `LeaderLost`. Gating it behind the
    /// lease it just revoked would leave every follower's promise
    /// pending until the successor happened to announce itself.
    fn post_fenced(&mut self, body: ProxyBody) {
        self.stamped += 1;
        self.transport.post(
            &ProxyEnvelope {
                generation: self.lease.generation(),
                from: ProxySide::Leader,
                body,
            }
            .to_json(),
        );
    }
}

// ─────────────────────────── the follower side ─────────────────────────

/// What a follower learned from one inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowerEvent {
    /// A leader announced itself. On a generation change this is
    /// emitted *after* the pending work has been failed.
    Leader {
        /// The new generation.
        generation: u64,
        /// The leader's node id.
        node_id: u64,
        /// The generation that was current until now. `0` for the
        /// first leader this follower saw.
        previous: u64,
    },
    /// A subscription was (re-)established by the leader.
    Restored {
        /// The channel.
        channel: String,
    },
    /// A node event, verbatim JSON, to hand to the page.
    Event {
        /// [`crate::node::LeafEvent::to_json`]'s output.
        json: String,
    },
    /// A message was refused by the fence. Carries what the sender
    /// presented and what is in force.
    Fenced {
        /// The generation the sender presented.
        presented: u64,
        /// The generation in force.
        current: u64,
    },
    /// The leader said the work of a generation is dead, and the
    /// pending calls for it have been failed.
    LeaderLost {
        /// The dead generation.
        lost: u64,
        /// How many pending requests were failed.
        failed: usize,
    },
}

/// The follower's half of the proxy: the same session API, over the
/// channel, with the generation on every message.
pub struct ProxyClient {
    transport: Rc<dyn ProxyTransport>,
    follower: u64,
    gate: GenerationGate,
    leader_node: Option<u64>,
    subscriptions: Vec<String>,
    capabilities: Vec<String>,
    pending: HashMap<u64, oneshot::Sender<ProxyOutcome>>,
    next_correlation: u64,
}

impl ProxyClient {
    /// A client for `follower`, declaring `subscriptions` and
    /// `capabilities`.
    ///
    /// `correlation_seed` is the first correlation id, for the same
    /// reason [`crate::rpc::CallTable::with_seed`] exists: a follower
    /// that reattaches after a leader change must not reuse a
    /// predecessor's ids, or a late reply would land on a live
    /// correlation.
    pub fn new(
        transport: Rc<dyn ProxyTransport>,
        follower: u64,
        subscriptions: Vec<String>,
        capabilities: Vec<String>,
        correlation_seed: u64,
    ) -> Self {
        Self {
            transport,
            follower,
            gate: GenerationGate::new(),
            leader_node: None,
            subscriptions,
            capabilities,
            pending: HashMap::new(),
            next_correlation: correlation_seed,
        }
    }

    /// The generation this follower will accept, and stamp.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.gate.highest()
    }

    /// The leader's node id, once one has announced itself.
    #[inline]
    pub fn leader_node(&self) -> Option<u64> {
        self.leader_node
    }

    /// How many requests are in flight.
    #[inline]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// This follower's id.
    #[inline]
    pub fn follower_id(&self) -> u64 {
        self.follower
    }

    /// Declare what this follower depends on and ask whoever holds
    /// the lock to identify itself.
    pub fn attach(&mut self) {
        let subscriptions = self.subscriptions.clone();
        let capabilities = self.capabilities.clone();
        self.post(ProxyBody::Attach {
            subscriptions,
            capabilities,
        });
    }

    /// Say goodbye, so the leader stops keeping this follower's
    /// subscriptions alive.
    pub fn detach(&mut self) {
        self.post(ProxyBody::Detach);
    }

    /// Issue one request and return the future its answer settles.
    ///
    /// With no leader known yet the request is refused immediately
    /// with [`LeafError::NotLeader`] rather than queued: a queue would
    /// silently convert "there is no node" into latency, and D2's rule
    /// is that nothing is retried behind the caller's back.
    pub fn request(&mut self, request: LeaderRequest) -> oneshot::Receiver<ProxyOutcome> {
        self.issue(request).1
    }

    /// Issue one request and return its correlation id beside the
    /// future.
    ///
    /// The id is what a **local deadline** needs: a follower's call
    /// carries the caller's timeout in the request, but the deadline
    /// that expires it is the leader's, pumped on the leader's node —
    /// so a leader that is frozen while holding its lock produces no
    /// expiry at all and the caller waits for the suspension. The
    /// session arms its own timer over this id and settles the entry
    /// through [`Self::expire`].
    pub fn issue(
        &mut self,
        request: LeaderRequest,
    ) -> (Option<u64>, oneshot::Receiver<ProxyOutcome>) {
        let (tx, rx) = oneshot::channel();
        let generation = self.gate.highest();
        if generation == 0 {
            let _ = tx.send(Err(ProxyFailure::Typed(LeafError::NotLeader {
                presented: 0,
                current: None,
            })));
            return (None, rx);
        }
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);
        // Standing intent, recorded so a re-attach after a leader
        // change carries it: whichever tab is promoted restores what
        // its followers currently want, not what they asked for once.
        match &request {
            LeaderRequest::Subscribe { channel } => {
                if !self.subscriptions.iter().any(|c| c == channel) {
                    self.subscriptions.push(channel.clone());
                }
            }
            LeaderRequest::Unsubscribe { channel } => {
                // Symmetric with the arm above, and for the same
                // reason: a re-attach after a leader change must carry
                // what this tab wants NOW. Leaving a released channel
                // in the standing intent would have the next leader
                // faithfully restore a subscription this tab had
                // already given up.
                self.subscriptions.retain(|c| c != channel);
            }
            LeaderRequest::Announce { capabilities } => {
                self.capabilities = capabilities.clone();
            }
            _ => {}
        }
        self.pending.insert(correlation, tx);
        self.post(ProxyBody::Request {
            correlation,
            request: Box::new(request),
        });
        (Some(correlation), rx)
    }

    /// Settle one in-flight request with `failure`, if it is still
    /// in flight. `true` when this call is what settled it.
    ///
    /// The honest half of a local deadline: it removes the
    /// correlation so a later reply lands nowhere, and it does **not**
    /// re-issue anything. The operation may have executed on the
    /// leader; that is what the failure says.
    pub fn expire(&mut self, correlation: u64, failure: ProxyFailure) -> bool {
        match self.pending.remove(&correlation) {
            Some(tx) => {
                let _ = tx.send(Err(failure));
                true
            }
            None => false,
        }
    }

    /// Handle one inbound message.
    ///
    /// `Ok(None)` means the message was for someone else (another
    /// follower's reply, or our own echo).
    pub fn on_message(&mut self, text: &str) -> Result<Option<FollowerEvent>> {
        let envelope = ProxyEnvelope::from_json(text)?;

        // Followers do not act on each other.
        if let ProxySide::Follower(_) = envelope.from {
            return Ok(None);
        }

        let admission = match self.gate.admit(envelope.generation) {
            Ok(admission) => admission,
            Err(LeafError::NotLeader { presented, current }) => {
                // THE fence. A resumed leader's message lands here
                // and goes no further.
                return Ok(Some(FollowerEvent::Fenced {
                    presented,
                    current: current.unwrap_or(0),
                }));
            }
            Err(other) => return Err(other),
        };

        // Leadership moved: everything the old leader owed us is
        // dead, and it is dead *before* the new leader is visible, so
        // no caller can observe a reply from one generation against a
        // future issued in another.
        if let Admission::NewLeader { previous } = admission {
            if previous != 0 {
                self.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                    generation: previous,
                })));
            }
        }

        match envelope.body {
            ProxyBody::Leadership { node_id } => {
                self.leader_node = Some(node_id);
                let previous = match admission {
                    Admission::NewLeader { previous } => previous,
                    Admission::Current => envelope.generation,
                };
                // Re-declare, on **every** generation this follower
                // has not yet declared under — including the first.
                //
                // The old guard was `previous != 0`, on the reasoning
                // that the first `Leadership` is the answer to the
                // `Attach` this follower has just sent. It is not
                // always: a tab can attach into a leader that has no
                // server yet (it is inside its backend connect) or
                // into an incumbent that is itself between states
                // (still a follower, about to be promoted), and in
                // both cases the message is not acted on. The
                // follower then learned the generation and never
                // re-declared, so its subscriptions reached no node
                // at all. Re-declaring here costs one extra message
                // pair at startup and terminates immediately after
                // it: the answering `Leadership` carries the same
                // generation, which admits as `Current`.
                if matches!(admission, Admission::NewLeader { .. }) {
                    self.attach();
                }
                Ok(Some(FollowerEvent::Leader {
                    generation: envelope.generation,
                    node_id,
                    previous,
                }))
            }
            ProxyBody::Reply { correlation, value } => {
                if let Some(tx) = self.pending.remove(&correlation) {
                    let _ = tx.send(Ok(value));
                }
                Ok(None)
            }
            ProxyBody::Failed {
                correlation,
                failure,
            } => {
                if let Some(tx) = self.pending.remove(&correlation) {
                    let _ = tx.send(Err(failure));
                }
                Ok(None)
            }
            ProxyBody::Restored { channel } => Ok(Some(FollowerEvent::Restored { channel })),
            ProxyBody::Event { json } => Ok(Some(FollowerEvent::Event { json })),
            ProxyBody::LeaderLost { lost } => {
                let failed =
                    self.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                        generation: lost,
                    })));
                Ok(Some(FollowerEvent::LeaderLost { lost, failed }))
            }
            other => Err(LeafError::ControlPlane(format!(
                "the leader sent a follower-only body: {other:?}"
            ))),
        }
    }

    /// Fail every in-flight request with `failure`, and return how
    /// many.
    ///
    /// D2's rule, and the reason this is a separate method: nothing is
    /// retried. The caller of each future is told, typed, and decides.
    pub fn fail_pending(&mut self, failure: ProxyFailure) -> usize {
        let count = self.pending.len();
        for (_, tx) in self.pending.drain() {
            let _ = tx.send(Err(failure.clone()));
        }
        count
    }

    /// The channels this follower has declared.
    pub fn subscriptions(&self) -> &[String] {
        &self.subscriptions
    }

    /// The capabilities this follower has declared — its constructor's
    /// list, replaced by whatever its last `announce()` asked for.
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    fn post(&mut self, body: ProxyBody) {
        self.transport.post(
            &ProxyEnvelope {
                generation: self.gate.highest(),
                from: ProxySide::Follower(self.follower),
                body,
            }
            .to_json(),
        );
    }
}

// ───────────────────────────── JSON helpers ────────────────────────────

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::from(s.clone())).collect())
}

fn b64(bytes: &[u8]) -> Value {
    use base64::Engine;
    Value::from(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn unb64(value: &Value, key: &str) -> Result<Bytes> {
    use base64::Engine;
    let text = str_field(value, key)?;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map(Bytes::from)
        .map_err(|e| LeafError::ControlPlane(format!("proxy field {key} is not base64: {e}")))
}

/// Transparent-envelope tags for the org transport's
/// [`ProxyValue::Bytes`] payloads.
///
/// The envelope is deliberately the wire's own byte grammar — request
/// items, response items and terminal diagnostics ride the proxy
/// **verbatim** ("a transparent envelope of the same nRPC frame
/// bytes"), so a new call shape adds no `ProxyBody`/`ProxyValue`
/// variant. Typed terminals do not ride the envelope at all: they
/// answer [`ProxyBody::Failed`] with the frozen
/// [`crate::error::LeafError`] vocabulary.
pub const ORG_ENVELOPE_ITEM: u8 = 0x00;
/// The success terminal: `0x01 ‖ body` (the CS aggregate; empty for
/// the SS/DX `end` marker).
pub const ORG_ENVELOPE_END: u8 = 0x01;
/// A retirement signal: `0x02 ‖ <reason>` with the frozen
/// `OrgRetireReason` string.
pub const ORG_ENVELOPE_RETIRED: u8 = 0x02;
/// An admitted serve call: `0x03 ‖ <JSON { call }>`.
///
/// Just the leader-minted bridge handle. The verified `OrgCaller`
/// projection is deliberately NOT carried as an envelope claim
/// (LEAF-6): it is resolved from the leader's own
/// [`crate::rpc_serve::ServeCall`] state through
/// [`LeaderRequest::OrgServeCaller`] once the handle is known.
pub const ORG_ENVELOPE_ADMITTED: u8 = 0x03;

/// Wrap bytes in the transparent envelope (the tags above).
pub fn envelope(tag: u8, payload: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(tag);
    out.extend_from_slice(payload);
    Bytes::from(out)
}

/// Read the `0x03 ‖ { call }` accept envelope's handle.
///
/// Strictly the producer's own shape: the tag must be
/// [`ORG_ENVELOPE_ADMITTED`] and the body a JSON object with a
/// decimal-string `call`. A bare-JSON body — which is what a forgery
/// posts, having no envelope to emit — decodes to nothing (LEAF-3:
/// the consumer this replaced ran `serde_json::from_slice` on the
/// WHOLE payload, so it parsed exactly the forged bare accepts and
/// dropped every well-formed one). Extra fields are ignored rather
/// than trusted: an attacker-added `caller` claim is never read here
/// (LEAF-6), the projection comes from the leader's serve state.
pub fn decode_admitted(payload: &[u8]) -> Option<u64> {
    let (&tag, rest) = payload.split_first()?;
    if tag != ORG_ENVELOPE_ADMITTED {
        return None;
    }
    let value: Value = serde_json::from_slice(rest).ok()?;
    value.get("call")?.as_str()?.parse::<u64>().ok()
}

/// Whether a failed serve registration is worth re-declaring (LEAF-14).
///
/// Only the generation/session-lifecycle moves are transient: the
/// leader changed under the request, or the reply went with the
/// generation that owed it. Both clear on their own — the successor
/// generation re-declares the registration — so the re-declare loop
/// retries them. Everything else is PERMANENT: the name is already
/// served, the argument strings did not parse, the session is
/// closed. A permanent refusal used to be misdiagnosed as transient
/// and retried at 50ms forever; now it reaches the caller typed and
/// the loop stops.
pub fn registration_refusal_is_transient(failure: &ProxyFailure) -> bool {
    matches!(
        failure.typed(),
        Some(LeafError::NotLeader { .. })
            | Some(LeafError::Rpc(RpcError::LeaderLost { .. }))
            | Some(LeafError::Rpc(RpcError::SessionLost))
    )
}

/// The typed "your call is closed" failure for an unknown handle —
/// "no org call for handle N", never another call's.
pub fn no_such_call(call: u64) -> ProxyFailure {
    ProxyFailure::Typed(LeafError::Session(format!("no org call for handle {call}")))
}

/// The typed refusal for a call handle that is live but already
/// claimed — a colliding insert names nothing but its own failure.
pub fn call_in_use(call: u64) -> ProxyFailure {
    ProxyFailure::Typed(LeafError::Session(format!(
        "org call handle {call} is already in use"
    )))
}

/// The typed "no such registration" failure — unknown AND foreign
/// alike, so a handle namespace cannot be probed by difference.
pub fn no_such_registration(registration: u64) -> ProxyFailure {
    ProxyFailure::Typed(LeafError::Session(format!(
        "no org registration for handle {registration}"
    )))
}

/// A fresh relay-handle seed: random for the reason the follower's
/// own ids are — a bridge handle names a live served call across a
/// channel every tab can read, and a counter from 1 is a guessable
/// name for it.
fn relay_seed() -> Result<u64> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
    Ok(u64::from_le_bytes(bytes))
}

/// The leader-side relay for proxied org calls and serves.
///
/// Attribution, enforced here: every entry is keyed by the handle the
/// FOLLOWER minted (its per-call / per-registration id) and carries
/// the [`ProxySide`] that claimed it. Every verb resolves a handle
/// only within the requesting sender's own namespace, so a request
/// naming a guessed or colliding id owns nothing ("no org call for
/// handle N", never another follower's call under the same id), and a
/// colliding insert never clobbers the live entry it names (dropping
/// a victim's [`crate::rpc_stream::CallHandle`] is its CANCEL). That
/// is exactly the required witness inverse.
#[derive(Default)]
pub struct OrgRelay {
    /// The node-side guards for follower calls (streaming shapes),
    /// each with the sender that claimed its handle.
    calls: HashMap<u64, (ProxySide, crate::rpc_stream::CallHandle)>,
    /// Served calls a follower's handler drives: bridge-call id →
    /// (owner, registration, the real [`crate::rpc_serve::ServeCall`]).
    serve_calls: HashMap<u64, (ProxySide, u64, crate::rpc_serve::ServeCall)>,
    /// Per-registration queues of admitted calls awaiting an accept.
    accepts: HashMap<u64, VecDeque<u64>>,
    /// Registrations: id → (owner, the service name RECORDED at
    /// registration). Unregistration acts on the recorded name and
    /// only for the owner — a request's own claim is never the key
    /// (LEAF-4).
    registrations: HashMap<u64, (ProxySide, String)>,
}

impl OrgRelay {
    /// The node-side call id behind a follower's streaming handle, if
    /// the requesting sender owns it and it is live here.
    pub fn stream_call(&self, from: ProxySide, call: u64) -> Option<u64> {
        match self.calls.get(&call) {
            Some((owner, handle)) if *owner == from => Some(handle.call_id),
            _ => None,
        }
    }

    /// Whether a call handle is live here (any owner) — the cheap
    /// pre-check that keeps a doomed open from starting.
    pub fn call_live(&self, call: u64) -> bool {
        self.calls.contains_key(&call)
    }

    /// Install a freshly opened call under `from`'s handle.
    ///
    /// A colliding insert (the handle is live) is refused and the new
    /// handle handed back: the live entry keeps its own handle —
    /// dropping a victim's [`crate::rpc_stream::CallHandle`] is its
    /// exactly-one CANCEL (LEAF-5).
    pub fn install_call(
        &mut self,
        from: ProxySide,
        call: u64,
        handle: crate::rpc_stream::CallHandle,
    ) -> core::result::Result<(), (ProxyFailure, crate::rpc_stream::CallHandle)> {
        if self.calls.contains_key(&call) {
            return Err((call_in_use(call), handle));
        }
        self.calls.insert(call, (from, handle));
        Ok(())
    }

    /// Release one call entry at terminal delivery (LEAF-13): the
    /// entry goes WITH the terminal, so a long-lived relay does not
    /// accumulate one handle-holding entry per call it ever carried,
    /// and a re-delivered terminal finds nothing left to re-apply.
    /// `false` — no state change — on a second release.
    pub fn release_call(&mut self, from: ProxySide, call: u64) -> bool {
        match self.calls.get(&call) {
            Some((owner, _)) if *owner == from => self.calls.remove(&call).is_some(),
            _ => false,
        }
    }

    /// The served call `from` owns under `call`, if any — the one
    /// resolution every serve verb goes through.
    pub fn serve_call(&self, from: ProxySide, call: u64) -> Option<&crate::rpc_serve::ServeCall> {
        match self.serve_calls.get(&call) {
            Some((owner, _, serve)) if *owner == from => Some(serve),
            _ => None,
        }
    }

    /// Park one admitted call for `from`'s registration, minting its
    /// unguessable bridge handle.
    ///
    /// `Err` hands the call back when the registration is gone
    /// (closed while this call was being admitted): no accept pull
    /// remains to take it, and the caller must settle typed rather
    /// than park on a queue nobody drains.
    pub fn install_serve(
        &mut self,
        from: ProxySide,
        registration: u64,
        serve: crate::rpc_serve::ServeCall,
    ) -> core::result::Result<u64, crate::rpc_serve::ServeCall> {
        if !self.registrations.contains_key(&registration) {
            return Err(serve);
        }
        let call = match self.next_bridge_call() {
            Ok(call) => call,
            Err(_) => return Err(serve),
        };
        self.serve_calls.insert(call, (from, registration, serve));
        self.accepts
            .entry(registration)
            .or_default()
            .push_back(call);
        Ok(call)
    }

    /// Pop the next admitted call for `from`'s registration.
    ///
    /// `Ok(None)` is the long-pull's "nothing yet"; `Err` is an
    /// unknown or foreign registration — typed, and never another
    /// follower's queue.
    pub fn accept_admitted(
        &mut self,
        from: ProxySide,
        registration: u64,
    ) -> core::result::Result<Option<u64>, ProxyFailure> {
        if !self.registration_owned_by(from, registration) {
            return Err(no_such_registration(registration));
        }
        Ok(self
            .accepts
            .get_mut(&registration)
            .and_then(|queue| queue.pop_front()))
    }

    /// Whether `from` owns `registration`.
    pub fn registration_owned_by(&self, from: ProxySide, registration: u64) -> bool {
        matches!(
            self.registrations.get(&registration),
            Some((owner, _)) if *owner == from
        )
    }

    /// Claim `registration` for `from`, bound to `service`.
    ///
    /// `Ok(true)` is a fresh claim — the node registration is the
    /// caller's to make, and [`Self::drop_registration`] rolls the
    /// claim back if it fails. `Ok(false)` is the re-declare
    /// idempotence: an identical claim is already recorded (and its
    /// node registration with it). A live handle re-bound to a
    /// different service, or claimed by a different sender, is
    /// refused — never overwritten (LEAF-5).
    pub fn claim_registration(
        &mut self,
        from: ProxySide,
        registration: u64,
        service: &str,
    ) -> core::result::Result<bool, ProxyFailure> {
        match self.registrations.get(&registration) {
            Some((owner, bound)) if *owner == from && bound == service => Ok(false),
            Some(_) if self.registration_owned_by(from, registration) => {
                Err(ProxyFailure::Typed(LeafError::Session(format!(
                    "org registration handle {registration} is already bound to another service"
                ))))
            }
            Some(_) => Err(no_such_registration(registration)),
            None => {
                self.registrations
                    .insert(registration, (from, service.to_string()));
                Ok(true)
            }
        }
    }

    /// The service name recorded for `from`'s registration — the name
    /// unregistration acts on (LEAF-4), never a request's claim.
    pub fn registration_service(&self, from: ProxySide, registration: u64) -> Option<String> {
        self.registrations
            .get(&registration)
            .filter(|(owner, _)| *owner == from)
            .map(|(_, service)| service.clone())
    }

    /// Drop `from`'s registration and everything it carried: the
    /// accept queue and every served call admitted to it. Refused
    /// (and untouched) for a registration `from` does not own.
    pub fn drop_registration(&mut self, from: ProxySide, registration: u64) -> bool {
        if !self.registration_owned_by(from, registration) {
            return false;
        }
        self.registrations.remove(&registration);
        self.accepts.remove(&registration);
        self.serve_calls
            .retain(|_, (owner, seen, _)| !(*owner == from && *seen == registration));
        true
    }

    /// Generation teardown: every entry goes (each dropped call
    /// handle emits its exactly-one CANCEL).
    pub fn clear(&mut self) {
        self.calls.clear();
        self.serve_calls.clear();
        self.accepts.clear();
        self.registrations.clear();
    }

    /// The next bridge-call id: a fresh CSPRNG draw per call, never 0
    /// and never a live id. A seeded counter (the pre-audit shape) made
    /// every later id predictable from one observed accept envelope
    /// (§23 audit).
    pub fn next_bridge_call(&mut self) -> Result<u64> {
        loop {
            let call = relay_seed()?;
            if call != 0 && !self.serve_calls.contains_key(&call) {
                return Ok(call);
            }
        }
    }

    /// Release a SETTLED served call `from` owns (LEAF-13): its terminal
    /// has committed on the node and the follower has been told the
    /// outcome, so nothing further can reach it. `false` — no state
    /// change — for an unsettled, foreign or unknown handle.
    pub fn release_serve(&mut self, from: ProxySide, call: u64) -> bool {
        match self.serve_calls.get(&call) {
            Some((owner, _, serve)) if *owner == from && serve.settled() => {
                self.serve_calls.remove(&call).is_some()
            }
            _ => false,
        }
    }

    /// How many served calls the relay holds (test observability).
    #[cfg(test)]
    pub(crate) fn serve_call_count(&self) -> usize {
        self.serve_calls.len()
    }
}

/// An optional `u32` in the `timeout_ms` spelling: decimal string or
/// null.
fn opt_u32(value: &Option<u32>) -> Value {
    match value {
        Some(n) => Value::from(n.to_string()),
        None => Value::Null,
    }
}

/// Read an [`opt_u32`] field back.
fn opt_u32_field(value: &Value, key: &str) -> Result<Option<u32>> {
    match field(value, key)? {
        Value::Null => Ok(None),
        _ => Ok(Some(u64_field(value, key)?.try_into().map_err(|_| {
            LeafError::ControlPlane(format!("proxy field {key} does not fit a u32"))
        })?)),
    }
}

fn encode_request(request: &LeaderRequest) -> Value {
    let mut map = Map::new();
    match request {
        LeaderRequest::Call {
            service,
            payload,
            timeout_ms,
        } => {
            map.insert("op".into(), Value::from("call"));
            map.insert("service".into(), Value::from(service.clone()));
            map.insert("payload".into(), b64(payload));
            map.insert(
                "timeout_ms".into(),
                match timeout_ms {
                    Some(ms) => Value::from(ms.to_string()),
                    None => Value::Null,
                },
            );
        }
        LeaderRequest::Subscribe { channel } => {
            map.insert("op".into(), Value::from("subscribe"));
            map.insert("channel".into(), Value::from(channel.clone()));
        }
        LeaderRequest::Unsubscribe { channel } => {
            map.insert("op".into(), Value::from("unsubscribe"));
            map.insert("channel".into(), Value::from(channel.clone()));
        }
        LeaderRequest::Publish { channel, payload } => {
            map.insert("op".into(), Value::from("publish"));
            map.insert("channel".into(), Value::from(channel.clone()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::Announce { capabilities } => {
            map.insert("op".into(), Value::from("announce"));
            map.insert("capabilities".into(), strings(capabilities));
        }
        LeaderRequest::Query { capability } => {
            map.insert("op".into(), Value::from("query"));
            map.insert("capability".into(), Value::from(capability.clone()));
        }
        LeaderRequest::StreamOpen {
            label,
            reliability,
            stream_id,
            channel_hash,
            peer,
        } => {
            map.insert("op".into(), Value::from("stream_open"));
            map.insert("label".into(), Value::from(label.clone()));
            map.insert(
                "reliability".into(),
                Value::from(match reliability {
                    Reliability::Reliable => "reliable",
                    Reliability::FireAndForget => "fireAndForget",
                }),
            );
            map.insert(
                "stream_id".into(),
                match stream_id {
                    Some(id) => Value::from(id.to_string()),
                    None => Value::Null,
                },
            );
            map.insert(
                "channel_hash".into(),
                match channel_hash {
                    Some(hash) => Value::from(*hash),
                    None => Value::Null,
                },
            );
            map.insert(
                "peer".into(),
                match peer {
                    Some(id) => Value::from(id.to_string()),
                    None => Value::Null,
                },
            );
        }
        LeaderRequest::StreamSend { handle, payload } => {
            map.insert("op".into(), Value::from("stream_send"));
            map.insert("handle".into(), Value::from(handle.to_string()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::StreamClose { handle } => {
            map.insert("op".into(), Value::from("stream_close"));
            map.insert("handle".into(), Value::from(handle.to_string()));
        }
        LeaderRequest::Signal {
            peer,
            dialog,
            kind,
            payload,
        } => {
            map.insert("op".into(), Value::from("signal"));
            map.insert("peer".into(), Value::from(peer.to_string()));
            map.insert("dialog".into(), Value::from(dialog.to_string()));
            map.insert("signal_kind".into(), Value::from(kind.clone()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::PeerOffer { peer } => {
            map.insert("op".into(), Value::from("peer_offer"));
            map.insert("peer".into(), Value::from(peer.to_string()));
        }
        LeaderRequest::PeerAcceptOffer { peer } => {
            map.insert("op".into(), Value::from("peer_accept_offer"));
            map.insert("peer".into(), Value::from(peer.to_string()));
        }
        LeaderRequest::PeerCandidate { peer, dialog } => {
            map.insert("op".into(), Value::from("peer_candidate"));
            map.insert("peer".into(), Value::from(peer.to_string()));
            map.insert("dialog".into(), Value::from(dialog.to_string()));
        }
        LeaderRequest::PeerHandshake { peer, dialog } => {
            map.insert("op".into(), Value::from("peer_handshake"));
            map.insert("peer".into(), Value::from(peer.to_string()));
            map.insert("dialog".into(), Value::from(dialog.to_string()));
        }
        LeaderRequest::Counters => {
            map.insert("op".into(), Value::from("counters"));
        }
        LeaderRequest::Enroll => {
            map.insert("op".into(), Value::from("enroll"));
        }
        LeaderRequest::IsEnrolled => {
            map.insert("op".into(), Value::from("is_enrolled"));
        }
        LeaderRequest::OrgCall {
            call,
            shape,
            service,
            body,
            membership,
            dispatcher,
            capability_grant,
            acting_org,
            provider_org,
            provider,
            ttl_secs,
            deadline_ns,
            timeout_ms,
            stream_window_initial,
            request_window_initial,
        } => {
            map.insert("op".into(), Value::from("org_call"));
            map.insert("call".into(), Value::from(call.to_string()));
            map.insert("shape".into(), Value::from(shape.clone()));
            map.insert("service".into(), Value::from(service.clone()));
            map.insert("body".into(), b64(body));
            map.insert("membership".into(), b64(membership));
            map.insert("dispatcher".into(), b64(dispatcher));
            map.insert(
                "capability_grant".into(),
                capability_grant
                    .as_ref()
                    .map_or(Value::Null, |grant| b64(grant.as_ref())),
            );
            map.insert("acting_org".into(), Value::from(acting_org.clone()));
            map.insert("provider_org".into(), Value::from(provider_org.clone()));
            map.insert("provider".into(), Value::from(provider.clone()));
            map.insert("ttl_secs".into(), Value::from(ttl_secs.to_string()));
            map.insert("deadline_ns".into(), Value::from(deadline_ns.to_string()));
            map.insert("timeout_ms".into(), opt_u32(timeout_ms));
            map.insert(
                "stream_window_initial".into(),
                opt_u32(stream_window_initial),
            );
            map.insert(
                "request_window_initial".into(),
                opt_u32(request_window_initial),
            );
        }
        LeaderRequest::OrgSend { call, payload } => {
            map.insert("op".into(), Value::from("org_send"));
            map.insert("call".into(), Value::from(call.to_string()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::OrgFinishSending { call } => {
            map.insert("op".into(), Value::from("org_finish_sending"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgNext { call } => {
            map.insert("op".into(), Value::from("org_next"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgCancel { call } => {
            map.insert("op".into(), Value::from("org_cancel"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgServeRegister {
            registration,
            service,
            access,
            owner_org,
            shape,
        } => {
            map.insert("op".into(), Value::from("org_serve_register"));
            map.insert("registration".into(), Value::from(registration.to_string()));
            map.insert("service".into(), Value::from(service.clone()));
            map.insert("access".into(), Value::from(access.clone()));
            map.insert("owner_org".into(), Value::from(owner_org.clone()));
            map.insert("shape".into(), Value::from(shape.clone()));
        }
        LeaderRequest::OrgServeAccept { registration } => {
            map.insert("op".into(), Value::from("org_serve_accept"));
            map.insert("registration".into(), Value::from(registration.to_string()));
        }
        LeaderRequest::OrgServeCaller { call } => {
            map.insert("op".into(), Value::from("org_serve_caller"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgServeRequest { call } => {
            map.insert("op".into(), Value::from("org_serve_request"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgServeSend { call, payload } => {
            map.insert("op".into(), Value::from("org_serve_send"));
            map.insert("call".into(), Value::from(call.to_string()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::OrgServeFinish {
            call,
            status,
            message,
        } => {
            map.insert("op".into(), Value::from("org_serve_finish"));
            map.insert("call".into(), Value::from(call.to_string()));
            map.insert("status".into(), Value::from(status.to_string()));
            map.insert("message".into(), Value::from(message.clone()));
        }
        LeaderRequest::OrgServeRetired { call } => {
            map.insert("op".into(), Value::from("org_serve_retired"));
            map.insert("call".into(), Value::from(call.to_string()));
        }
        LeaderRequest::OrgServeUnregister { registration } => {
            map.insert("op".into(), Value::from("org_serve_unregister"));
            map.insert("registration".into(), Value::from(registration.to_string()));
        }
    }
    Value::Object(map)
}

fn decode_request(value: &Value) -> Result<LeaderRequest> {
    Ok(match str_field(value, "op")? {
        "call" => LeaderRequest::Call {
            service: str_field(value, "service")?.to_string(),
            payload: unb64(value, "payload")?,
            timeout_ms: match field(value, "timeout_ms")? {
                Value::Null => None,
                _ => Some(u64_field(value, "timeout_ms")?.try_into().map_err(|_| {
                    LeafError::ControlPlane("proxy timeout_ms does not fit a u32".into())
                })?),
            },
        },
        "subscribe" => LeaderRequest::Subscribe {
            channel: str_field(value, "channel")?.to_string(),
        },
        "unsubscribe" => LeaderRequest::Unsubscribe {
            channel: str_field(value, "channel")?.to_string(),
        },
        "publish" => LeaderRequest::Publish {
            channel: str_field(value, "channel")?.to_string(),
            payload: unb64(value, "payload")?,
        },
        "announce" => LeaderRequest::Announce {
            capabilities: string_list(value, "capabilities")?,
        },
        "query" => LeaderRequest::Query {
            capability: str_field(value, "capability")?.to_string(),
        },
        "stream_open" => LeaderRequest::StreamOpen {
            label: str_field(value, "label")?.to_string(),
            reliability: match str_field(value, "reliability")? {
                "reliable" => Reliability::Reliable,
                "fireAndForget" => Reliability::FireAndForget,
                other => {
                    return Err(LeafError::ControlPlane(format!(
                        "unknown stream reliability {other:?}"
                    )))
                }
            },
            stream_id: match field(value, "stream_id")? {
                Value::Null => None,
                _ => Some(u64_field(value, "stream_id")?),
            },
            channel_hash: match field(value, "channel_hash")? {
                Value::Null => None,
                _ => Some(u64_field(value, "channel_hash")?.try_into().map_err(|_| {
                    LeafError::ControlPlane("proxy channel_hash does not fit a u16".into())
                })?),
            },
            peer: match field(value, "peer")? {
                Value::Null => None,
                _ => Some(u64_field(value, "peer")?),
            },
        },
        "stream_send" => LeaderRequest::StreamSend {
            handle: u64_field(value, "handle")?,
            payload: unb64(value, "payload")?,
        },
        "stream_close" => LeaderRequest::StreamClose {
            handle: u64_field(value, "handle")?,
        },
        "signal" => LeaderRequest::Signal {
            peer: u64_field(value, "peer")?,
            dialog: u64_field(value, "dialog")?,
            kind: str_field(value, "signal_kind")?.to_string(),
            payload: unb64(value, "payload")?,
        },
        "peer_offer" => LeaderRequest::PeerOffer {
            peer: u64_field(value, "peer")?,
        },
        "peer_accept_offer" => LeaderRequest::PeerAcceptOffer {
            peer: u64_field(value, "peer")?,
        },
        "peer_candidate" => LeaderRequest::PeerCandidate {
            peer: u64_field(value, "peer")?,
            dialog: u64_field(value, "dialog")?,
        },
        "peer_handshake" => LeaderRequest::PeerHandshake {
            peer: u64_field(value, "peer")?,
            dialog: u64_field(value, "dialog")?,
        },
        "counters" => LeaderRequest::Counters,
        "enroll" => LeaderRequest::Enroll,
        "is_enrolled" => LeaderRequest::IsEnrolled,
        "org_call" => LeaderRequest::OrgCall {
            call: u64_field(value, "call")?,
            shape: str_field(value, "shape")?.to_string(),
            service: str_field(value, "service")?.to_string(),
            body: unb64(value, "body")?,
            membership: unb64(value, "membership")?,
            dispatcher: unb64(value, "dispatcher")?,
            capability_grant: match field(value, "capability_grant")? {
                Value::Null => None,
                _ => Some(unb64(value, "capability_grant")?),
            },
            acting_org: str_field(value, "acting_org")?.to_string(),
            provider_org: str_field(value, "provider_org")?.to_string(),
            provider: str_field(value, "provider")?.to_string(),
            ttl_secs: u64_field(value, "ttl_secs")?,
            deadline_ns: u64_field(value, "deadline_ns")?,
            timeout_ms: opt_u32_field(value, "timeout_ms")?,
            stream_window_initial: opt_u32_field(value, "stream_window_initial")?,
            request_window_initial: opt_u32_field(value, "request_window_initial")?,
        },
        "org_send" => LeaderRequest::OrgSend {
            call: u64_field(value, "call")?,
            payload: unb64(value, "payload")?,
        },
        "org_finish_sending" => LeaderRequest::OrgFinishSending {
            call: u64_field(value, "call")?,
        },
        "org_next" => LeaderRequest::OrgNext {
            call: u64_field(value, "call")?,
        },
        "org_cancel" => LeaderRequest::OrgCancel {
            call: u64_field(value, "call")?,
        },
        "org_serve_register" => LeaderRequest::OrgServeRegister {
            registration: u64_field(value, "registration")?,
            service: str_field(value, "service")?.to_string(),
            access: str_field(value, "access")?.to_string(),
            owner_org: str_field(value, "owner_org")?.to_string(),
            shape: str_field(value, "shape")?.to_string(),
        },
        "org_serve_accept" => LeaderRequest::OrgServeAccept {
            registration: u64_field(value, "registration")?,
        },
        "org_serve_caller" => LeaderRequest::OrgServeCaller {
            call: u64_field(value, "call")?,
        },
        "org_serve_request" => LeaderRequest::OrgServeRequest {
            call: u64_field(value, "call")?,
        },
        "org_serve_send" => LeaderRequest::OrgServeSend {
            call: u64_field(value, "call")?,
            payload: unb64(value, "payload")?,
        },
        "org_serve_finish" => LeaderRequest::OrgServeFinish {
            call: u64_field(value, "call")?,
            status: u64_field(value, "status")?
                .try_into()
                .map_err(|_| LeafError::ControlPlane("proxy status does not fit a u16".into()))?,
            message: str_field(value, "message")?.to_string(),
        },
        "org_serve_retired" => LeaderRequest::OrgServeRetired {
            call: u64_field(value, "call")?,
        },
        "org_serve_unregister" => LeaderRequest::OrgServeUnregister {
            registration: u64_field(value, "registration")?,
        },
        other => {
            return Err(LeafError::ControlPlane(format!(
                "unknown proxy request op {other:?}"
            )))
        }
    })
}

fn encode_value(value: &ProxyValue) -> Value {
    let mut map = Map::new();
    match value {
        ProxyValue::Bytes(payload) => {
            map.insert("result".into(), Value::from("bytes"));
            map.insert("payload".into(), b64(payload));
        }
        ProxyValue::Text(text) => {
            map.insert("result".into(), Value::from("text"));
            map.insert("text".into(), Value::from(text.clone()));
        }
        ProxyValue::Flag(flag) => {
            map.insert("result".into(), Value::from("flag"));
            map.insert("flag".into(), Value::from(*flag));
        }
        ProxyValue::Stream {
            handle,
            stream_id,
            peer,
            incarnation,
        } => {
            map.insert("result".into(), Value::from("stream"));
            map.insert("handle".into(), Value::from(handle.to_string()));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
            map.insert("peer".into(), Value::from(peer.to_string()));
            map.insert("incarnation".into(), Value::from(incarnation.to_string()));
        }
    }
    Value::Object(map)
}

fn decode_value(value: &Value) -> Result<ProxyValue> {
    Ok(match str_field(value, "result")? {
        "bytes" => ProxyValue::Bytes(unb64(value, "payload")?),
        "text" => ProxyValue::Text(str_field(value, "text")?.to_string()),
        "flag" => ProxyValue::Flag(bool_field(value, "flag")?),
        "stream" => ProxyValue::Stream {
            handle: u64_field(value, "handle")?,
            stream_id: u64_field(value, "stream_id")?,
            // Absent or unreadable identity is a decode failure, not a
            // stream with no peer: a proxied handle that cannot name
            // its peer cannot filter by it.
            peer: u64_field(value, "peer")?,
            incarnation: u64_field(value, "incarnation")?,
        },
        other => {
            return Err(LeafError::ControlPlane(format!(
                "unknown proxy reply result {other:?}"
            )))
        }
    })
}

/// Encode a [`ProxyFailure`].
///
/// A [`ProxyFailure::Typed`] goes as a machine tag plus its fields,
/// never as its `Display` text: a follower must reconstruct the same
/// typed value, and re-parsing a sentence across a version boundary
/// is how a taxonomy turns into string matching. A
/// [`ProxyFailure::Reported`] has no fields left to encode — it is
/// already text — so it goes as `reported`, which is a tag the
/// decoder can tell apart from every typed one.
fn encode_failure(failure: &ProxyFailure) -> Value {
    match failure {
        ProxyFailure::Typed(error) => encode_error(error),
        ProxyFailure::Reported(text) => {
            let mut map = Map::new();
            map.insert("kind".into(), Value::from("reported"));
            map.insert("message".into(), Value::from(text.clone()));
            Value::Object(map)
        }
    }
}

fn decode_failure(value: &Value) -> Result<ProxyFailure> {
    if str_field(value, "kind")? == "reported" {
        return Ok(ProxyFailure::Reported(
            str_field(value, "message")?.to_string(),
        ));
    }
    Ok(ProxyFailure::Typed(decode_error(value)?))
}

fn encode_error(error: &LeafError) -> Value {
    let mut map = Map::new();
    match error {
        LeafError::Wire(detail) => {
            map.insert("kind".into(), Value::from("wire"));
            map.insert("detail".into(), Value::from(detail.clone()));
        }
        LeafError::Replay => {
            map.insert("kind".into(), Value::from("replay"));
        }
        LeafError::Session(detail) => {
            map.insert("kind".into(), Value::from("session"));
            map.insert("detail".into(), Value::from(detail.clone()));
        }
        LeafError::ControlPlane(detail) => {
            map.insert("kind".into(), Value::from("control_plane"));
            map.insert("detail".into(), Value::from(detail.clone()));
        }
        LeafError::Identity(detail) => {
            map.insert("kind".into(), Value::from("identity"));
            map.insert("detail".into(), Value::from(detail.clone()));
        }
        LeafError::Backpressure {
            stream_id,
            needed,
            remaining,
        } => {
            map.insert("kind".into(), Value::from("backpressure"));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
            map.insert("needed".into(), Value::from(*needed));
            map.insert("remaining".into(), Value::from(*remaining));
        }
        LeafError::ReliableWindowFull {
            stream_id,
            needed,
            remaining,
        } => {
            map.insert("kind".into(), Value::from("reliable_window_full"));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
            map.insert("needed".into(), Value::from(*needed as u64));
            map.insert("remaining".into(), Value::from(*remaining as u64));
        }
        LeafError::IceServerConflictsWithPeer {
            entry,
            peer_rtc_addr,
        } => {
            map.insert("kind".into(), Value::from("ice_server_conflicts_with_peer"));
            map.insert("entry".into(), Value::from(entry.clone()));
            map.insert("peer_rtc_addr".into(), Value::from(peer_rtc_addr.clone()));
        }
        LeafError::NotLeader { presented, current } => {
            map.insert("kind".into(), Value::from("not_leader"));
            map.insert("presented".into(), Value::from(presented.to_string()));
            map.insert(
                "current".into(),
                match current {
                    Some(current) => Value::from(current.to_string()),
                    None => Value::Null,
                },
            );
        }
        LeafError::Rtc(rtc) => {
            map.insert("kind".into(), Value::from("rtc"));
            match rtc {
                RtcError::IceTimeout => {
                    map.insert("rtc".into(), Value::from("ice_timeout"));
                }
                RtcError::UdpBlocked(evidence) => {
                    map.insert("rtc".into(), Value::from("udp_blocked"));
                    map.insert("bootstrap_ok".into(), Value::from(evidence.bootstrap_ok));
                    map.insert(
                        "stun_probe_failed".into(),
                        Value::from(evidence.stun_probe_failed),
                    );
                    map.insert("probed".into(), Value::from(evidence.probed.clone()));
                }
                RtcError::ChannelClosed(detail) => {
                    map.insert("rtc".into(), Value::from("channel_closed"));
                    map.insert("detail".into(), Value::from(detail.clone()));
                }
                RtcError::Unsupported(detail) => {
                    map.insert("rtc".into(), Value::from("unsupported"));
                    map.insert("detail".into(), Value::from(detail.clone()));
                }
            }
        }
        LeafError::Rpc(rpc) => {
            map.insert("kind".into(), Value::from("rpc"));
            match rpc {
                RpcError::Refused { status, message } => {
                    map.insert("rpc".into(), Value::from("refused"));
                    map.insert("status".into(), Value::from(*status));
                    map.insert("message".into(), Value::from(message.clone()));
                }
                RpcError::Timeout => {
                    map.insert("rpc".into(), Value::from("timeout"));
                }
                RpcError::SessionLost => {
                    map.insert("rpc".into(), Value::from("session_lost"));
                }
                RpcError::LeaderLost { generation } => {
                    map.insert("rpc".into(), Value::from("leader_lost"));
                    map.insert("generation".into(), Value::from(generation.to_string()));
                }
                RpcError::Indeterminate { deadline_ms } => {
                    map.insert("rpc".into(), Value::from("indeterminate"));
                    map.insert("deadline_ms".into(), Value::from(*deadline_ms));
                }
                RpcError::Malformed(detail) => {
                    map.insert("rpc".into(), Value::from("malformed"));
                    map.insert("detail".into(), Value::from(detail.clone()));
                }
            }
        }
    }
    Value::Object(map)
}

fn decode_error(value: &Value) -> Result<LeafError> {
    let detail = |key: &str| str_field(value, key).map(str::to_string);
    Ok(match str_field(value, "kind")? {
        "wire" => LeafError::Wire(detail("detail")?),
        "replay" => LeafError::Replay,
        "session" => LeafError::Session(detail("detail")?),
        "control_plane" => LeafError::ControlPlane(detail("detail")?),
        "identity" => LeafError::Identity(detail("detail")?),
        "not_leader" => LeafError::NotLeader {
            presented: u64_field(value, "presented")?,
            current: match field(value, "current")? {
                Value::Null => None,
                _ => Some(u64_field(value, "current")?),
            },
        },
        "rtc" => LeafError::Rtc(match str_field(value, "rtc")? {
            "ice_timeout" => RtcError::IceTimeout,
            "udp_blocked" => {
                let bootstrap_ok = bool_field(value, "bootstrap_ok")?;
                let stun_probe_failed = bool_field(value, "stun_probe_failed")?;
                let probed = detail("probed")?;
                // Reconstructed through the constructor, so the
                // "both observations or nothing" rule holds on this
                // side of the channel too: a peer that sent
                // `udp_blocked` without the evidence does not get to
                // assert it here.
                match UdpBlockedEvidence::new(bootstrap_ok, stun_probe_failed, probed) {
                    Some(evidence) => RtcError::udp_blocked(evidence),
                    None => RtcError::IceTimeout,
                }
            }
            "channel_closed" => RtcError::ChannelClosed(detail("detail")?),
            "unsupported" => RtcError::Unsupported(detail("detail")?),
            other => {
                return Err(LeafError::ControlPlane(format!(
                    "unknown rtc failure {other:?}"
                )))
            }
        }),
        "rpc" => LeafError::Rpc(match str_field(value, "rpc")? {
            "refused" => RpcError::Refused {
                status: u64_field(value, "status")?.try_into().map_err(|_| {
                    LeafError::ControlPlane("proxy rpc status does not fit a u16".into())
                })?,
                message: detail("message")?,
            },
            "timeout" => RpcError::Timeout,
            "session_lost" => RpcError::SessionLost,
            "leader_lost" => RpcError::LeaderLost {
                generation: u64_field(value, "generation")?,
            },
            "indeterminate" => RpcError::Indeterminate {
                deadline_ms: u64_field(value, "deadline_ms")?.try_into().map_err(|_| {
                    LeafError::ControlPlane("proxy rpc deadline_ms does not fit a u32".into())
                })?,
            },
            "malformed" => RpcError::Malformed(detail("detail")?),
            other => {
                return Err(LeafError::ControlPlane(format!(
                    "unknown rpc failure {other:?}"
                )))
            }
        }),
        "backpressure" => LeafError::Backpressure {
            stream_id: u64_field(value, "stream_id")?,
            needed: u64_field(value, "needed")?.try_into().map_err(|_| {
                LeafError::ControlPlane("proxy backpressure needed does not fit a u32".into())
            })?,
            remaining: u64_field(value, "remaining")?.try_into().map_err(|_| {
                LeafError::ControlPlane("proxy backpressure remaining does not fit a u32".into())
            })?,
        },
        "reliable_window_full" => LeafError::ReliableWindowFull {
            stream_id: u64_field(value, "stream_id")?,
            needed: u64_field(value, "needed")? as usize,
            remaining: u64_field(value, "remaining")? as usize,
        },
        "ice_server_conflicts_with_peer" => LeafError::IceServerConflictsWithPeer {
            entry: detail("entry")?,
            peer_rtc_addr: detail("peer_rtc_addr")?,
        },
        other => {
            return Err(LeafError::ControlPlane(format!(
                "unknown proxy error kind {other:?}"
            )))
        }
    })
}

fn field<'v>(value: &'v Value, key: &str) -> Result<&'v Value> {
    value
        .get(key)
        .ok_or_else(|| LeafError::ControlPlane(format!("proxy message has no {key}")))
}

fn str_field<'v>(value: &'v Value, key: &str) -> Result<&'v str> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| LeafError::ControlPlane(format!("proxy field {key} is not a string")))
}

fn bool_field(value: &Value, key: &str) -> Result<bool> {
    field(value, key)?
        .as_bool()
        .ok_or_else(|| LeafError::ControlPlane(format!("proxy field {key} is not a bool")))
}

/// A `u64` from a decimal string, or from a small JSON number where
/// the field is bounded (`status`, `deadline_ms`).
fn u64_field(value: &Value, key: &str) -> Result<u64> {
    let raw = field(value, key)?;
    if let Some(text) = raw.as_str() {
        return text.parse().map_err(|_| {
            LeafError::ControlPlane(format!("proxy field {key} = {text:?} is not a u64"))
        });
    }
    raw.as_u64()
        .ok_or_else(|| LeafError::ControlPlane(format!("proxy field {key} is not a u64")))
}

fn string_list(value: &Value, key: &str) -> Result<Vec<String>> {
    let array = field(value, key)?
        .as_array()
        .ok_or_else(|| LeafError::ControlPlane(format!("proxy field {key} is not an array")))?;
    array
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                LeafError::ControlPlane(format!("proxy field {key} holds a non-string"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UdpBlockedEvidence;

    /// A backend that records what it was asked and answers on
    /// command, so the lifecycle can be driven without a node.
    struct TestBackend {
        node_id: u64,
        seen: Rc<std::cell::RefCell<Vec<LeaderRequest>>>,
        /// Repliers held open, so a test can decide when — or
        /// whether — a request is ever answered.
        held: Rc<std::cell::RefCell<Vec<Replier>>>,
        answer_immediately: bool,
    }

    impl TestBackend {
        fn new(node_id: u64) -> Self {
            Self {
                node_id,
                seen: Rc::new(std::cell::RefCell::new(Vec::new())),
                held: Rc::new(std::cell::RefCell::new(Vec::new())),
                answer_immediately: true,
            }
        }

        fn holding(node_id: u64) -> Self {
            Self {
                answer_immediately: false,
                ..Self::new(node_id)
            }
        }
    }

    impl LeaderBackend for TestBackend {
        fn node_id(&self) -> u64 {
            self.node_id
        }

        fn perform(&mut self, _from: ProxySide, request: LeaderRequest, reply: Replier) {
            self.seen.borrow_mut().push(request.clone());
            if self.answer_immediately {
                match request {
                    LeaderRequest::Query { .. } | LeaderRequest::Counters => {
                        reply.text("[]".into())
                    }
                    LeaderRequest::StreamOpen { .. } => {
                        reply.stream(7, 0x0102_0304_0506_0708, 0x00aa, 3)
                    }
                    LeaderRequest::Call { .. } => reply.bytes(Bytes::from_static(b"pong")),
                    _ => reply.bytes(Bytes::new()),
                }
            } else {
                self.held.borrow_mut().push(reply);
            }
        }

        /// The native double's retirement: let go of every held
        /// replier, which is what a real backend's cancellation does
        /// to the operations it spawned. Each dropped replier settles
        /// its caller through `Replier::Drop`, and the count is how
        /// many that was.
        fn shutdown(&mut self, _generation: u64) -> usize {
            let held = core::mem::take(&mut *self.held.borrow_mut());
            let count = held.len();
            drop(held);
            count
        }
    }

    fn envelope(generation: u64, from: ProxySide, body: ProxyBody) -> String {
        ProxyEnvelope {
            generation,
            from,
            body,
        }
        .to_json()
    }

    // ───────────────────────────── the fence ─────────────────────────────

    /// The gate is monotonic and never re-admits. That single property
    /// is the whole of stale-leader fencing: a resumed tab's message
    /// carries a generation below the highest seen, and there is no
    /// path back.
    #[test]
    fn the_gate_refuses_every_generation_below_the_highest_it_has_seen() {
        let mut gate = GenerationGate::new();
        assert_eq!(gate.highest(), 0);
        assert_eq!(
            gate.admit(4).expect("first"),
            Admission::NewLeader { previous: 0 }
        );
        assert_eq!(gate.admit(4).expect("same"), Admission::Current);
        assert_eq!(
            gate.admit(5).expect("next"),
            Admission::NewLeader { previous: 4 }
        );

        let refused = gate
            .admit(4)
            .expect_err("a stale generation must be refused");
        assert_eq!(
            refused,
            LeafError::NotLeader {
                presented: 4,
                current: Some(5)
            }
        );
        assert_eq!(gate.highest(), 5, "a refusal must not move the gate");
        assert!(gate.admit(0).is_err(), "and zero is not a reset");
        assert_eq!(gate.highest(), 5);
    }

    /// The whole point of the machine-tagged encoding: a follower gets
    /// back the value the leader constructed, not a sentence to parse.
    #[test]
    fn every_typed_failure_survives_the_channel_unchanged() {
        let evidence = UdpBlockedEvidence::new(true, true, "203.0.113.7:9").expect("both hold");
        for original in [
            LeafError::Wire("framing".into()),
            LeafError::Replay,
            LeafError::Session("no session".into()),
            LeafError::ControlPlane("no endpoint".into()),
            LeafError::Identity("no CSPRNG".into()),
            LeafError::NotLeader {
                presented: 7,
                current: Some(9),
            },
            LeafError::NotLeader {
                presented: 7,
                current: None,
            },
            LeafError::Rtc(RtcError::IceTimeout),
            LeafError::Rtc(RtcError::udp_blocked(evidence.clone())),
            LeafError::Rtc(RtcError::ChannelClosed("closed".into())),
            LeafError::Rtc(RtcError::Unsupported("no RTCPeerConnection".into())),
            LeafError::Rpc(RpcError::Refused {
                status: 404,
                message: "no such service".into(),
            }),
            LeafError::Rpc(RpcError::Timeout),
            LeafError::Rpc(RpcError::SessionLost),
            LeafError::Rpc(RpcError::LeaderLost { generation: 42 }),
            LeafError::Rpc(RpcError::Malformed("bad frame".into())),
        ] {
            let body = ProxyBody::Failed {
                correlation: 1,
                failure: ProxyFailure::Typed(original.clone()),
            };
            let text = envelope(3, ProxySide::Leader, body);
            let back = ProxyEnvelope::from_json(&text).expect("round trip");
            let ProxyBody::Failed { failure, .. } = back.body else {
                panic!("not a failure");
            };
            assert_eq!(
                failure.typed(),
                Some(&original),
                "{original} did not survive the channel"
            );
            assert_eq!(failure.message(), original.to_string());
        }

        // The carried-text case keeps its text and claims no type,
        // so nothing downstream can mistake it for one.
        let reported = ProxyFailure::Reported("rpc: refused (404): nope".into());
        let text = envelope(
            1,
            ProxySide::Leader,
            ProxyBody::Failed {
                correlation: 2,
                failure: reported.clone(),
            },
        );
        let back = ProxyEnvelope::from_json(&text).expect("round trip");
        let ProxyBody::Failed { failure, .. } = back.body else {
            panic!("not a failure");
        };
        assert_eq!(failure, reported);
        assert_eq!(failure.typed(), None);
    }

    /// `UdpBlocked` cannot be asserted without the evidence, and the
    /// channel is not a way around that: a sender that claims it
    /// without both observations gets the weaker, honest type.
    #[test]
    fn udp_blocked_without_both_observations_arrives_as_an_ice_timeout() {
        let forged = serde_json::json!({
            "v": PROXY_VERSION.to_string(),
            "generation": "1",
            "from": "leader",
            "kind": "failed",
            "correlation": "1",
            "error": {
                "kind": "rtc",
                "rtc": "udp_blocked",
                "bootstrap_ok": true,
                "stun_probe_failed": false,
                "probed": "203.0.113.7:9"
            }
        })
        .to_string();
        let back = ProxyEnvelope::from_json(&forged).expect("decodes");
        let ProxyBody::Failed { failure, .. } = back.body else {
            panic!("not a failure");
        };
        assert_eq!(
            failure.typed(),
            Some(&LeafError::Rtc(RtcError::IceTimeout)),
            "a UDP-blocked claim without both observations must not be honoured"
        );
    }

    /// Every `u64` crosses as text. A generation rounded by
    /// `JSON.parse` is a fence that stops fencing above 2^53, which is
    /// the failure mode that would not show up until it mattered.
    #[test]
    fn every_u64_crosses_the_channel_as_a_decimal_string() {
        let big = u64::MAX - 1;
        let text = envelope(
            big,
            ProxySide::Follower(big),
            ProxyBody::Request {
                correlation: big,
                request: Box::new(LeaderRequest::StreamSend {
                    handle: big,
                    payload: Bytes::from_static(b"x"),
                }),
            },
        );
        assert!(
            text.contains(&format!("\"{big}\"")),
            "a u64 must be quoted: {text}"
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
        for key in ["generation", "follower", "correlation"] {
            assert!(
                parsed[key].is_string(),
                "{key} crossed as a number, which rounds: {text}"
            );
        }
        let back = ProxyEnvelope::from_json(&text).expect("round trip");
        assert_eq!(back.generation, big);
        assert_eq!(back.from, ProxySide::Follower(big));
    }

    /// Every body shape survives, including the ones only one side
    /// ever sends. A body that encoded but did not decode would be a
    /// silently dropped message.
    #[test]
    fn every_body_shape_round_trips() {
        let bodies = vec![
            ProxyBody::Attach {
                subscriptions: vec!["a".into(), "b".into()],
                capabilities: vec!["cap".into()],
            },
            ProxyBody::Detach,
            ProxyBody::Request {
                correlation: 9,
                request: Box::new(LeaderRequest::Call {
                    service: "svc".into(),
                    payload: Bytes::from_static(b"body"),
                    timeout_ms: Some(1500),
                }),
            },
            ProxyBody::Request {
                correlation: 10,
                request: Box::new(LeaderRequest::Call {
                    service: "svc".into(),
                    payload: Bytes::new(),
                    timeout_ms: None,
                }),
            },
            ProxyBody::Request {
                correlation: 11,
                request: Box::new(LeaderRequest::Subscribe {
                    channel: "chan".into(),
                }),
            },
            ProxyBody::Request {
                correlation: 12,
                request: Box::new(LeaderRequest::Publish {
                    channel: "chan".into(),
                    payload: Bytes::from_static(b"p"),
                }),
            },
            ProxyBody::Request {
                correlation: 13,
                request: Box::new(LeaderRequest::Announce {
                    capabilities: vec!["cap".into()],
                }),
            },
            ProxyBody::Request {
                correlation: 14,
                request: Box::new(LeaderRequest::Query {
                    capability: "cap".into(),
                }),
            },
            ProxyBody::Request {
                correlation: 15,
                request: Box::new(LeaderRequest::StreamOpen {
                    label: "app".into(),
                    reliability: Reliability::FireAndForget,
                    stream_id: Some(77),
                    channel_hash: Some(0xBEEF),
                    // A peer-addressed proxied stream: the field has
                    // to survive the round trip or a follower's
                    // stream silently addresses the anchor instead.
                    peer: Some(0xDEAD_BEEF_0000_0001),
                }),
            },
            ProxyBody::Request {
                correlation: 16,
                request: Box::new(LeaderRequest::StreamOpen {
                    label: "app".into(),
                    reliability: Reliability::Reliable,
                    stream_id: None,
                    channel_hash: None,
                    peer: None,
                }),
            },
            ProxyBody::Request {
                correlation: 17,
                request: Box::new(LeaderRequest::StreamClose { handle: 5 }),
            },
            ProxyBody::Request {
                correlation: 18,
                request: Box::new(LeaderRequest::Signal {
                    peer: 0xAAAA,
                    dialog: 7,
                    kind: "offer".into(),
                    payload: Bytes::from_static(b"v=0"),
                }),
            },
            ProxyBody::Request {
                correlation: 23,
                request: Box::new(LeaderRequest::PeerOffer { peer: 0xAB }),
            },
            ProxyBody::Request {
                correlation: 24,
                request: Box::new(LeaderRequest::PeerAcceptOffer { peer: 0xAC }),
            },
            ProxyBody::Request {
                correlation: 25,
                // The dialog has to survive the round trip: a proxied
                // poll that lost it would be applied to whichever
                // attempt is live when the leader got to it.
                request: Box::new(LeaderRequest::PeerCandidate {
                    peer: 0xAD,
                    dialog: 0x5109,
                }),
            },
            ProxyBody::Request {
                correlation: 26,
                request: Box::new(LeaderRequest::PeerHandshake {
                    peer: 0xAE,
                    dialog: 0x510A,
                }),
            },
            ProxyBody::Request {
                correlation: 19,
                request: Box::new(LeaderRequest::Counters),
            },
            ProxyBody::Leadership { node_id: 0x1234 },
            ProxyBody::Reply {
                correlation: 20,
                value: ProxyValue::Bytes(Bytes::from_static(b"reply")),
            },
            ProxyBody::Reply {
                correlation: 21,
                value: ProxyValue::Text("[]".into()),
            },
            ProxyBody::Reply {
                correlation: 22,
                value: ProxyValue::Stream {
                    handle: 4,
                    stream_id: 31,
                    peer: 0x00bb,
                    incarnation: 9,
                },
            },
            ProxyBody::Restored {
                channel: "chan".into(),
            },
            ProxyBody::Event {
                json: "{\"type\":\"connected\"}".into(),
            },
            ProxyBody::LeaderLost { lost: 4 },
        ];
        for body in bodies {
            let text = envelope(2, ProxySide::Leader, body.clone());
            let back = ProxyEnvelope::from_json(&text).expect("round trip");
            assert_eq!(back.body, body);
            assert_eq!(back.generation, 2, "every message carries the generation");
        }
    }

    /// A build that does not know the protocol refuses rather than
    /// guesses: two tabs on different deploys is an ordinary state.
    #[test]
    fn an_unknown_protocol_version_is_refused() {
        let current = format!("\"v\":\"{PROXY_VERSION}\"");
        let future = PROXY_VERSION + 1;
        let text = envelope(1, ProxySide::Leader, ProxyBody::Detach)
            .replace(&current, &format!("\"v\":\"{future}\""));
        let error = ProxyEnvelope::from_json(&text).expect_err("a future envelope must be refused");
        assert!(
            error.to_string().contains(&format!("version {future}")),
            "{error}"
        );
        assert!(ProxyEnvelope::from_json("not json").is_err());
        // A stale peer's envelope is refused the same way — that is what
        // each bump is for (v3: the pre-LEAF-13 watcher; v2: pre-LEAF-4/6).
        assert!(ProxyEnvelope::from_json("{\"v\":\"3\"}").is_err());
        assert!(ProxyEnvelope::from_json("{\"v\":\"2\"}").is_err());
    }

    /// The last-consumer arithmetic, which is the whole reason a
    /// release is not a subscribe run backwards.
    #[test]
    fn a_release_keeps_the_channel_while_another_follower_still_wants_it() {
        let mut registry = FollowerRegistry::new();
        registry.attach(1, ["shared".to_string(), "mine".to_string()], []);
        registry.attach(2, ["shared".to_string()], []);

        assert!(
            registry.release(1, "shared"),
            "follower 2 still declares it, so the membership stays"
        );
        assert_eq!(
            registry.subscription_union(),
            vec!["mine".to_string(), "shared".to_string()],
            "the released claim is gone from the union's owner, not from the union"
        );

        assert!(
            !registry.release(2, "shared"),
            "that was the last consumer, so the membership is the leader's to give up"
        );
        assert_eq!(
            registry.subscription_union(),
            vec!["mine".to_string()],
            "and it leaves the restoration list, so a successor does not bring it back"
        );

        assert!(
            !registry.release(1, "never-declared"),
            "releasing something nobody declared is not somebody else's claim"
        );
    }

    /// The last-consumer question, over the two sets that answer it.
    ///
    /// Native on purpose: the session layer that holds the first set
    /// is `wasm32`-only, and an arithmetic witnessed only there is
    /// witnessed only on a runner.
    #[test]
    fn a_channel_is_still_wanted_while_either_set_holds_it() {
        let mine: BTreeSet<String> = ["mine".to_string(), "both".to_string()]
            .into_iter()
            .collect();
        let theirs = vec!["theirs".to_string(), "both".to_string()];

        assert!(
            membership_still_wanted(&mine, &theirs, "mine"),
            "this tab still declares it, so the membership stays — the leader \
             must not cancel its own page's delivery because a follower left"
        );
        assert!(
            membership_still_wanted(&mine, &theirs, "theirs"),
            "a follower still declares it"
        );
        assert!(membership_still_wanted(&mine, &theirs, "both"));
        assert!(
            !membership_still_wanted(&mine, &theirs, "nobody"),
            "neither set holds it, so this was the last consumer"
        );

        let empty = BTreeSet::new();
        assert!(!membership_still_wanted(&empty, &[], "anything"));
    }
    // ────────────────────────── the follower registry ────────────────────

    /// Restoration is the union, not the last writer: two followers
    /// wanting two channels both get theirs back. Capability intent
    /// travels the same way, for the same reason.
    #[test]
    fn restoration_is_the_union_of_every_followers_declaration() {
        let mut registry = FollowerRegistry::new();
        assert!(registry.is_empty());
        registry.attach(
            1,
            ["alpha".to_string(), "beta".to_string()],
            ["cap-a".to_string()],
        );
        registry.attach(2, ["beta".to_string(), "gamma".to_string()], []);
        registry.declare(2, "delta");
        registry.declare_capabilities(2, &["cap-b".to_string()]);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.subscription_union(),
            vec!["alpha", "beta", "delta", "gamma"],
            "deduplicated and ordered, so the restoration sequence is deterministic"
        );
        assert_eq!(registry.capability_union(), vec!["cap-a", "cap-b"]);

        registry.detach(2);
        assert_eq!(registry.subscription_union(), vec!["alpha", "beta"]);
        assert_eq!(registry.capability_union(), vec!["cap-a"]);
        // Re-attach replaces a declaration rather than merging into
        // it: a follower that dropped a channel must not have it kept
        // alive forever.
        registry.attach(1, ["alpha".to_string()], []);
        assert_eq!(registry.subscription_union(), vec!["alpha"]);
        assert!(registry.capability_union().is_empty());
        // The same rule for capabilities declared after attaching: a
        // second `announce()` publishes a document, so it replaces.
        registry.declare_capabilities(1, &["cap-c".to_string()]);
        registry.declare_capabilities(1, &["cap-d".to_string()]);
        assert_eq!(registry.capability_union(), vec!["cap-d"]);
    }

    // ─────────────────────────── the leader's half ───────────────────────

    /// The claim is "the generation is stamped on **every** message".
    /// This holds it to account message by message, and the counter
    /// makes removing the stamp a visible change rather than a silent
    /// one.
    #[test]
    fn the_leader_stamps_its_generation_on_every_message_it_sends() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0x99), transport.clone(), 7);

        server.announce_leadership();
        server.announce_restored("chan");
        server.broadcast_event("{\"type\":\"connected\"}");
        server
            .on_message(&envelope(
                7,
                ProxySide::Follower(1),
                ProxyBody::Request {
                    correlation: 1,
                    request: Box::new(LeaderRequest::Query {
                        capability: "cap".into(),
                    }),
                },
            ))
            .expect("served");
        // Stand-down's own broadcast is stamped too — and it is the
        // one message the fence does not gate, because it is what
        // publishes the fence.
        server.retire(None);

        let posted = transport.envelopes();
        assert_eq!(posted.len(), 5);
        for message in &posted {
            assert_eq!(message.generation, 7, "an unstamped message: {message:?}");
            assert_eq!(message.from, ProxySide::Leader);
        }
        assert_eq!(
            server.stamped(),
            5,
            "the stamp counter must account for every message"
        );
    }

    /// A follower request stamped with a generation that is not the
    /// leader's is refused, typed, and never performed. This is the
    /// leader-side half of the fence — the half that stops a resumed
    /// tab's *follower* work from landing.
    #[test]
    fn a_request_from_another_generation_is_refused_and_never_performed() {
        let transport = Rc::new(RecordingTransport::new());
        let backend = TestBackend::new(0x99);
        let seen = backend.seen.clone();
        let mut server = ProxyServer::new(backend, transport.clone(), 5);
        transport.clear();

        for stale in [4, 6] {
            let error = server
                .on_message(&envelope(
                    stale,
                    ProxySide::Follower(1),
                    ProxyBody::Request {
                        correlation: 1,
                        request: Box::new(LeaderRequest::Call {
                            service: "svc".into(),
                            payload: Bytes::new(),
                            timeout_ms: None,
                        }),
                    },
                ))
                .expect_err("a foreign generation must be refused");
            assert_eq!(
                error,
                LeafError::NotLeader {
                    presented: stale,
                    current: Some(5)
                }
            );
        }
        assert!(
            seen.borrow().is_empty(),
            "a refused request must never reach the node"
        );

        let answers = transport.envelopes();
        assert_eq!(answers.len(), 2, "each refusal is answered");
        for answer in &answers {
            let ProxyBody::Failed { failure, .. } = &answer.body else {
                panic!("a refusal must be a typed failure, got {answer:?}");
            };
            assert!(matches!(
                failure.typed(),
                Some(LeafError::NotLeader {
                    current: Some(5),
                    ..
                })
            ));
        }
        // A follower ahead of us means we are the stale one.
        assert_eq!(server.superseded(), Some(6));
    }

    /// `Attach` is the fence's one exemption, and it has to be: a tab
    /// that just opened has no generation, and this is the message
    /// that gets it one.
    #[test]
    fn attach_is_served_without_a_generation_and_answered_with_leadership() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0xABCD), transport.clone(), 3);
        transport.clear();

        server
            .on_message(&envelope(
                0,
                ProxySide::Follower(11),
                ProxyBody::Attach {
                    subscriptions: vec!["chan".into()],
                    capabilities: vec![],
                },
            ))
            .expect("attach must be served at generation zero");

        assert_eq!(server.followers(), 1);
        assert_eq!(server.restoration(), vec!["chan"]);
        let posted = transport.envelopes();
        assert_eq!(posted.len(), 1);
        assert_eq!(
            posted[0].body,
            ProxyBody::Leadership { node_id: 0xABCD },
            "an attach must be answered by identifying the leader"
        );
        assert_eq!(posted[0].generation, 3);
    }

    /// A stood-down leader answers with a refusal instead of serving
    /// or going silent. Silence would look like latency; serving would
    /// be the stale-leader action itself.
    #[test]
    fn a_stood_down_leader_refuses_instead_of_serving() {
        let transport = Rc::new(RecordingTransport::new());
        let backend = TestBackend::new(0x99);
        let seen = backend.seen.clone();
        let mut server = ProxyServer::new(backend, transport.clone(), 2);
        server.close();
        assert!(server.is_closed());
        transport.clear();

        let error = server
            .on_message(&envelope(
                2,
                ProxySide::Follower(1),
                ProxyBody::Request {
                    correlation: 8,
                    request: Box::new(LeaderRequest::Counters),
                },
            ))
            .expect_err("a closed leader must refuse");
        assert!(matches!(error, LeafError::NotLeader { presented: 2, .. }));
        assert!(seen.borrow().is_empty());
        let posted = transport.envelopes();
        assert_eq!(posted.len(), 1);
        assert!(matches!(
            &posted[0].body,
            ProxyBody::Failed { correlation: 8, .. }
        ));
    }

    /// A replier that is dropped without an answer must still settle
    /// the follower's promise. A promise that never settles is worse
    /// than a failure: it is indistinguishable from a slow leader.
    #[test]
    fn a_forgotten_request_is_answered_rather_than_left_pending() {
        let transport = Rc::new(RecordingTransport::new());
        let backend = TestBackend::holding(0x99);
        let held = backend.held.clone();
        let mut server = ProxyServer::new(backend, transport.clone(), 1);
        transport.clear();

        server
            .on_message(&envelope(
                1,
                ProxySide::Follower(1),
                ProxyBody::Request {
                    correlation: 4,
                    request: Box::new(LeaderRequest::Counters),
                },
            ))
            .expect("served");
        assert!(transport.envelopes().is_empty(), "not answered yet");

        // The leader forgets it — the tab is going away, the future
        // was dropped, whatever the reason.
        held.borrow_mut().clear();

        let posted = transport.envelopes();
        assert_eq!(posted.len(), 1);
        let ProxyBody::Failed {
            correlation,
            failure,
        } = &posted[0].body
        else {
            panic!("expected a failure, got {:?}", posted[0].body);
        };
        assert_eq!(*correlation, 4);
        assert_eq!(
            failure.typed(),
            Some(&LeafError::Rpc(RpcError::SessionLost))
        );
    }

    /// The leader's own operations go through the same backend, and
    /// their answers go to the caller — not onto the channel every
    /// other tab on the origin is listening to.
    #[test]
    fn a_leader_local_request_is_answered_without_broadcasting() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0x99), transport.clone(), 4);
        transport.clear();

        let (tx, mut rx) = oneshot::channel();
        let reply = Replier::local(tx, server.lease());
        server.backend_mut().perform(
            ProxySide::Leader,
            LeaderRequest::Call {
                service: "svc".into(),
                payload: Bytes::new(),
                timeout_ms: None,
            },
            reply,
        );

        assert!(
            transport.raw().is_empty(),
            "a local answer must not be broadcast: {:?}",
            transport.raw()
        );
        let answered = rx.try_recv().expect("not cancelled").expect("answered");
        assert_eq!(answered, Ok(ProxyValue::Bytes(Bytes::from_static(b"pong"))));
    }

    // ────────────────────────── the follower's half ──────────────────────

    /// D2 row 1, the follower's side: leadership moving fails every
    /// in-flight call with `RpcError::LeaderLost` carrying the
    /// generation that owned it, and nothing is re-issued.
    #[test]
    fn a_leader_change_fails_the_old_generations_calls_typed_and_retries_nothing() {
        let transport = Rc::new(RecordingTransport::new());
        let mut client = ProxyClient::new(transport.clone(), 42, vec!["chan".into()], vec![], 100);

        client
            .on_message(&envelope(
                1,
                ProxySide::Leader,
                ProxyBody::Leadership { node_id: 7 },
            ))
            .expect("first leader");
        assert_eq!(client.generation(), 1);
        assert_eq!(client.leader_node(), Some(7));

        let mut first = client.request(LeaderRequest::Counters);
        let mut second = client.request(LeaderRequest::Query {
            capability: "cap".into(),
        });
        assert_eq!(client.pending(), 2);
        transport.clear();

        // A new leader announces itself.
        let event = client
            .on_message(&envelope(
                2,
                ProxySide::Leader,
                ProxyBody::Leadership { node_id: 8 },
            ))
            .expect("second leader")
            .expect("an event");
        assert_eq!(
            event,
            FollowerEvent::Leader {
                generation: 2,
                node_id: 8,
                previous: 1
            }
        );

        for pending in [&mut first, &mut second] {
            let outcome = pending.try_recv().expect("not cancelled").expect("settled");
            assert_eq!(
                outcome.expect_err("must fail").typed(),
                Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 })),
                "a call the old leader owned must fail against the generation that owned it"
            );
        }
        assert_eq!(client.pending(), 0, "nothing may be silently re-issued");

        // And it re-declares, so the new leader can restore.
        let posted = transport.envelopes();
        assert!(
            posted.iter().any(|message| matches!(
                &message.body,
                ProxyBody::Attach { subscriptions, .. }
                    if subscriptions == &vec!["chan".to_string()]
            )),
            "a surviving follower must re-declare its subscriptions: {posted:?}"
        );
        assert!(
            posted.iter().all(|message| message.generation == 2),
            "and stamp them with the new generation"
        );
    }

    /// THE stale-leader witness, in its pure form: a tab that was
    /// suspended still believes it holds generation *n*, and every
    /// message it emits is refused by a follower that has seen *n+1*.
    #[test]
    fn a_resumed_leaders_messages_are_fenced_and_change_nothing() {
        let transport = Rc::new(RecordingTransport::new());
        let mut client = ProxyClient::new(transport.clone(), 42, vec![], vec![], 100);
        client
            .on_message(&envelope(
                9,
                ProxySide::Leader,
                ProxyBody::Leadership { node_id: 8 },
            ))
            .expect("current leader");
        let mut pending = client.request(LeaderRequest::Counters);

        // The resumed tab, still stamping the generation it held.
        for body in [
            ProxyBody::Leadership { node_id: 7 },
            ProxyBody::Event {
                json: "{\"type\":\"connected\"}".into(),
            },
            ProxyBody::Reply {
                correlation: 100,
                value: ProxyValue::Text("stale".into()),
            },
            ProxyBody::Restored {
                channel: "chan".into(),
            },
        ] {
            let event = client
                .on_message(&envelope(8, ProxySide::Leader, body))
                .expect("decoded")
                .expect("an event");
            assert_eq!(
                event,
                FollowerEvent::Fenced {
                    presented: 8,
                    current: 9
                },
                "a message from a superseded generation must be refused"
            );
        }

        assert_eq!(client.generation(), 9, "the fence does not move backwards");
        assert_eq!(client.leader_node(), Some(8), "and the leader is unchanged");
        assert_eq!(
            client.pending(),
            1,
            "a stale reply must not settle a live correlation"
        );
        assert!(
            pending.try_recv().expect("not cancelled").is_none(),
            "the future must still be waiting"
        );
    }

    /// An orderly stand-down: the leader says whose work just died,
    /// and the follower fails exactly that.
    #[test]
    fn an_announced_leader_loss_fails_the_pending_work_it_names() {
        let transport = Rc::new(RecordingTransport::new());
        let mut client = ProxyClient::new(transport, 42, vec![], vec![], 100);
        client
            .on_message(&envelope(
                3,
                ProxySide::Leader,
                ProxyBody::Leadership { node_id: 7 },
            ))
            .expect("leader");
        let mut pending = client.request(LeaderRequest::Counters);

        let event = client
            .on_message(&envelope(
                3,
                ProxySide::Leader,
                ProxyBody::LeaderLost { lost: 3 },
            ))
            .expect("decoded")
            .expect("an event");
        assert_eq!(event, FollowerEvent::LeaderLost { lost: 3, failed: 1 });
        let outcome = pending.try_recv().expect("not cancelled").expect("settled");
        assert_eq!(
            outcome.expect_err("must fail").typed(),
            Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 3 }))
        );
    }

    /// Nothing is queued behind a leader that does not exist. A queue
    /// would turn "there is no node" into unbounded latency, and D2's
    /// rule is that the caller is told.
    #[test]
    fn a_request_with_no_leader_is_refused_immediately() {
        let transport = Rc::new(RecordingTransport::new());
        let mut client = ProxyClient::new(transport.clone(), 42, vec![], vec![], 100);

        let mut pending = client.request(LeaderRequest::Counters);
        let outcome = pending.try_recv().expect("not cancelled").expect("settled");
        assert_eq!(
            outcome.expect_err("must fail").typed(),
            Some(&LeafError::NotLeader {
                presented: 0,
                current: None
            })
        );
        assert_eq!(client.pending(), 0);
        assert!(
            transport.raw().is_empty(),
            "and nothing is posted for a leader that is not there"
        );
    }

    /// The round trip that makes the proxy an API rather than a
    /// message bus: a follower's request reaches the node and its
    /// answer reaches the follower's future.
    #[test]
    fn a_followers_request_reaches_the_node_and_its_answer_comes_back() {
        // One transport each, wired by hand — a `BroadcastChannel`
        // does not echo to its own sender either.
        let to_leader = Rc::new(RecordingTransport::new());
        let to_follower = Rc::new(RecordingTransport::new());
        let backend = TestBackend::new(0x5151);
        let seen = backend.seen.clone();
        let mut server = ProxyServer::new(backend, to_follower.clone(), 6);
        let mut client = ProxyClient::new(to_leader.clone(), 77, vec!["chan".into()], vec![], 500);

        client.attach();
        for text in to_leader.raw() {
            server.on_message(&text).expect("attach");
        }
        to_leader.clear();
        for text in to_follower.raw() {
            client.on_message(&text).expect("leadership");
        }
        to_follower.clear();
        assert_eq!(client.generation(), 6);

        let mut pending = client.request(LeaderRequest::Call {
            service: "echo".into(),
            payload: Bytes::from_static(b"ping"),
            timeout_ms: Some(250),
        });
        for text in to_leader.raw() {
            server.on_message(&text).expect("served");
        }
        assert_eq!(
            seen.borrow().len(),
            1,
            "the request must have reached the node"
        );
        for text in to_follower.raw() {
            client.on_message(&text).expect("reply");
        }
        let outcome = pending.try_recv().expect("not cancelled").expect("settled");
        assert_eq!(outcome, Ok(ProxyValue::Bytes(Bytes::from_static(b"pong"))));

        // And a subscribe declares itself, so a future leader can
        // restore it.
        let mut sub = client.request(LeaderRequest::Subscribe {
            channel: "other".into(),
        });
        for text in to_leader.raw() {
            server.on_message(&text).expect("served");
        }
        assert_eq!(
            server.restoration(),
            vec!["chan", "other"],
            "a channel a follower subscribed to must join the restoration set"
        );
        for text in to_follower.raw() {
            client.on_message(&text).expect("reply");
        }
        assert!(sub.try_recv().expect("not cancelled").is_some());
        assert!(client.subscriptions().contains(&"other".to_string()));
    }

    /// The lock and the channel are one scope. Two names would let a
    /// tab hold the lock while talking on a channel nobody hears.
    #[test]
    fn the_scope_names_the_identity_on_the_origin() {
        assert_eq!(
            scope_name("https://app.example", "0123456789abcdef0123456789abcdef"),
            "net-mesh/https://app.example/0123456789abcdef0123456789abcdef"
        );
        assert_ne!(
            scope_name("https://a.example", "ff"),
            scope_name("https://b.example", "ff"),
            "two origins must not share a lock"
        );
    }

    // ───────────── the accept envelope (LEAF-3 / LEAF-6) ─────────────

    /// LEAF-3: the producer's accept envelope and the consumer's
    /// decoder agree — the exact bytes the accept arm emits parse to
    /// exactly the one handle the loop dispatches once. Pre-fix,
    /// `decode_admitted` ran `serde_json::from_slice` on the WHOLE
    /// `0x03 ‖ JSON` payload, so the producer's own envelope decoded
    /// to NOTHING and every well-formed accept was dropped at the
    /// accept hop (the handler never dispatched, the caller timed
    /// out).
    #[test]
    fn a_well_formed_accept_envelope_decodes_to_exactly_one_call_handle() {
        let mut doc = Map::new();
        doc.insert("call".into(), Value::from("42"));
        // The producer's exact construction: `envelope(ORG_ENVELOPE_ADMITTED, json)`.
        let wire = super::envelope(
            ORG_ENVELOPE_ADMITTED,
            Value::Object(doc).to_string().as_bytes(),
        );
        assert_eq!(
            decode_admitted(&wire),
            Some(42),
            "the emitted accept must parse — pre-fix it decoded to None"
        );
    }

    /// LEAF-3's inverse: the ONLY accepts that parsed before the fix
    /// were FORGED bare-JSON ones (no tag to skip). A forgery now
    /// decodes to nothing — dispatch is the well-formed path's alone.
    #[test]
    fn a_forged_bare_json_accept_does_not_decode() {
        assert_eq!(
            decode_admitted(br#"{"call":"42","caller":{"entity":"ff"}}"#),
            None,
            "pre-fix exactly this forged shape parsed; a well-formed one did not"
        );
    }

    /// LEAF-6, codec half: a tampered envelope claim is never part of
    /// what the decoder produces. The projection reaching handlers is
    /// resolved from the leader's own serve state through
    /// `OrgServeCaller` — the accept doc's `caller` field, present or
    /// forged, is ignored.
    #[test]
    fn an_accept_decoder_reads_the_handle_and_never_a_caller_claim() {
        let mut doc = Map::new();
        doc.insert("call".into(), Value::from("42"));
        doc.insert("caller".into(), Value::from("{\"entity\":\"deadbeef\"}"));
        let wire = super::envelope(
            ORG_ENVELOPE_ADMITTED,
            Value::Object(doc).to_string().as_bytes(),
        );
        assert_eq!(
            decode_admitted(&wire),
            Some(42),
            "a tampered claim neither breaks the parse nor becomes the projection"
        );
    }

    // ─────────────────── at-most-once (LEAF-12) ───────────────────

    /// LEAF-12: a replayed request envelope — the same bytes posted
    /// twice at the channel — performs its verb exactly once. Pre-fix
    /// every delivery re-executed, so a replayed `OrgSend` published
    /// a second item (and a replayed `OrgServeSend` a second response
    /// item) at the provider.
    #[test]
    fn a_replayed_org_send_envelope_is_performed_exactly_once() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0x99), transport.clone(), 7);
        let seen = server.backend_mut().seen.clone();
        let request = envelope(
            7,
            ProxySide::Follower(1),
            ProxyBody::Request {
                correlation: 500,
                request: Box::new(LeaderRequest::OrgSend {
                    call: 9,
                    payload: Bytes::from_static(b"item"),
                }),
            },
        );
        server
            .on_message(&request)
            .expect("the first delivery serves");
        server
            .on_message(&request)
            .expect("the replay is dropped, not refused");

        assert_eq!(
            seen.borrow().len(),
            1,
            "the replayed envelope re-executed the send — items publish exactly once"
        );
        assert_eq!(
            server.replayed(),
            1,
            "and the replay was counted as seen, not re-performed"
        );
    }

    /// The same at-most-once for the served-response direction: a
    /// replayed `OrgServeSend` publishes exactly one response item.
    #[test]
    fn a_replayed_org_serve_send_envelope_is_performed_exactly_once() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0x99), transport.clone(), 7);
        let seen = server.backend_mut().seen.clone();
        let request = envelope(
            7,
            ProxySide::Follower(1),
            ProxyBody::Request {
                correlation: 501,
                request: Box::new(LeaderRequest::OrgServeSend {
                    call: 3,
                    payload: Bytes::from_static(b"response"),
                }),
            },
        );
        server.on_message(&request).expect("first");
        server.on_message(&request).expect("replay");
        assert_eq!(seen.borrow().len(), 1, "the response item published once");
    }

    /// The window is a replay guard, not a gap: a follower's next
    /// (fresh) correlation still performs, and an out-of-order
    /// delivery of a fresh one is not mistaken for a replay.
    #[test]
    fn a_fresh_correlation_is_not_swallowed_by_the_replay_window() {
        let transport = Rc::new(RecordingTransport::new());
        let mut server = ProxyServer::new(TestBackend::new(0x99), transport.clone(), 7);
        let seen = server.backend_mut().seen.clone();
        for correlation in [502, 503, 502, 503] {
            let request = envelope(
                7,
                ProxySide::Follower(1),
                ProxyBody::Request {
                    correlation,
                    request: Box::new(LeaderRequest::OrgNext { call: 9 }),
                },
            );
            server.on_message(&request).expect("served or dropped");
        }
        assert_eq!(
            seen.borrow().len(),
            2,
            "two distinct requests perform once each; their replays perform nothing"
        );
    }

    // ───────── permanent refusals reach the caller (LEAF-14) ─────────

    /// LEAF-14: only a generation/session move is transient. Pre-fix
    /// the re-declare loop retried EVERY registration failure at 50ms
    /// forever — an `AlreadyServed` refusal was misdiagnosed as
    /// latency and the page never learned its service was not live.
    #[test]
    fn a_permanent_serve_registration_refusal_is_never_transient() {
        let permanent = [
            ProxyFailure::Typed(LeafError::Session("serve \"svc\": AlreadyServed".into())),
            ProxyFailure::Reported("unknown org call shape \"x\"".into()),
            ProxyFailure::Typed(LeafError::Session("the session is closed".into())),
        ];
        for failure in &permanent {
            assert!(
                !registration_refusal_is_transient(failure),
                "{failure:?} is permanent and must reach the caller typed, not retry"
            );
        }
    }

    /// The transient half: a generation move under the request
    /// re-declares (the `Attach` discipline), and nothing else does.
    #[test]
    fn only_a_generation_move_retries_the_registration() {
        let transient = [
            ProxyFailure::Typed(LeafError::NotLeader {
                presented: 1,
                current: Some(2),
            }),
            ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost { generation: 1 })),
            ProxyFailure::Typed(LeafError::Rpc(RpcError::SessionLost)),
        ];
        for failure in &transient {
            assert!(
                registration_refusal_is_transient(failure),
                "{failure:?} clears on its own and re-declares"
            );
        }
    }

    // ─────────── the sender-bound relay (LEAF-5/4/13) ───────────

    use std::cell::RefCell;

    use crate::identity::EntityKeypair;
    use crate::org::cert::{OrgId, OrgKeypair, OrgMembershipCert};
    use crate::org::entity::EntityId;
    use crate::org::grant::{CapabilityAuthorityId, DispatcherScope, OrgDispatcherGrant};
    use crate::org::proof::RpcCallShape;
    use crate::org::replay::AdmissionReplayGuard;
    use crate::org::revocation::RevocationFacts;
    use crate::rpc_serve::{
        OpenOutcome, ServeAccess, ServeAdmission, ServeCall, ServeOptions, ServePeer, ServeRegistry,
    };
    use crate::rpc_stream::{
        attach_signed_admission, CallHandle, CallPin, OrgCallIntent, StreamCallRegistry, StreamOpen,
    };
    use crate::rpc_wire::RpcRequestPayload;

    const RELAY_SERVICE: &str = "svc.relay";
    const RELAY_NOW_SECS: u64 = 1_700_000_000;
    const RELAY_NOW_NS: u64 = 1_700_000_000_000_000_000;
    const RELAY_PEER: crate::control_plane::NodeId = 0xBEEF_0000_0002;

    /// The fixture world the relay witnesses mint their handles in —
    /// one org root and one caller entity, every proof minted through
    /// `crate::org` exactly as the serve lifecycle's harness does.
    struct RelayWorld {
        org: OrgKeypair,
        caller_kp: Rc<EntityKeypair>,
        caller_entity: EntityId,
        provider_entity: EntityId,
        owner_org: OrgId,
        binding: [u8; 32],
        facts: RevocationFacts,
        replay: AdmissionReplayGuard,
    }

    impl RelayWorld {
        fn new() -> Self {
            let org = OrgKeypair::from_bytes([0x42; 32]);
            let caller_kp = EntityKeypair::from_secret([0x24; 32]);
            let caller_entity = EntityId::from_bytes(*caller_kp.entity_id());
            Self {
                owner_org: org.org_id(),
                org,
                caller_kp: Rc::new(caller_kp),
                caller_entity,
                provider_entity: EntityId::from_bytes([0x77; 32]),
                binding: [0xAB; 32],
                facts: RevocationFacts::default(),
                replay: AdmissionReplayGuard::with_defaults(),
            }
        }

        fn capability() -> CapabilityAuthorityId {
            CapabilityAuthorityId::for_tag(&format!("nrpc:{RELAY_SERVICE}"))
        }

        fn intent(&self) -> OrgCallIntent {
            OrgCallIntent {
                keypair: Rc::clone(&self.caller_kp),
                membership: OrgMembershipCert::issue_at(
                    &self.org,
                    self.caller_entity.clone(),
                    1,
                    RELAY_NOW_SECS,
                    RELAY_NOW_SECS + 3_600,
                    0x1111_2222_3333_4444,
                ),
                dispatcher_grant: OrgDispatcherGrant::issue_at(
                    &self.org,
                    self.caller_entity.clone(),
                    DispatcherScope::Exact(Self::capability()),
                    RELAY_NOW_SECS,
                    RELAY_NOW_SECS + 3_600,
                    0x5555_6666_7777_8888,
                ),
                capability_grant: None,
                acting_org: self.owner_org,
                provider_org: self.owner_org,
                provider: self.provider_entity.clone(),
                capability: Self::capability(),
                ttl_secs: 30,
            }
        }

        fn serve_peer(&self) -> ServePeer {
            ServePeer {
                peer: RELAY_PEER,
                incarnation: 7,
                caller: self.caller_entity.clone(),
                session_binding: Some(self.binding),
            }
        }

        fn admission(&self) -> ServeAdmission<'_> {
            ServeAdmission {
                provider: &self.provider_entity,
                facts: &self.facts,
                replay: &self.replay,
            }
        }
    }

    /// One caller-side call handle: a LAZY CS open, so nothing is put
    /// on any wire and the relay sees exactly the handle the real
    /// `OrgCall` arm installs.
    fn relay_call_handle(world: &RelayWorld, seed: u64) -> CallHandle {
        let mut calls = StreamCallRegistry::new(world.caller_entity.origin_hash(), seed);
        calls
            .open_client_streaming(
                CallPin {
                    peer: RELAY_PEER,
                    incarnation: 7,
                    provider: world.provider_entity.clone(),
                    request_route: 1,
                    reply_route: 2,
                    carrier_stream_id: 3,
                },
                RELAY_SERVICE,
                StreamOpen::default(),
                world.intent(),
                Some(world.binding),
                RELAY_NOW_NS,
            )
            .expect("the lazy open mints a handle")
    }

    /// One real admitted [`ServeCall`] — minted through the serve
    /// registry's own admission, never a hand-rolled double.
    fn relay_serve_call(world: &RelayWorld, call_id: u64) -> ServeCall {
        relay_serve_call_in(world, call_id).1
    }

    /// [`relay_serve_call`], keeping the registry that owns the call so a
    /// test can drive it to a terminal.
    fn relay_serve_call_in(world: &RelayWorld, call_id: u64) -> (ServeRegistry, ServeCall) {
        let mut serves = ServeRegistry::new(0x5EED);
        let captured: Rc<RefCell<Option<ServeCall>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&captured);
        serves
            .serve(
                RELAY_SERVICE,
                ServeOptions {
                    shape: RpcCallShape::Unary,
                    access: ServeAccess::SameOrg,
                    provider_owner_org: world.owner_org,
                    skew_secs: 0,
                    default_live_ns: 300 * 1_000_000_000,
                    max_live_ns: 3600 * 1_000_000_000,
                    policy: None,
                },
                Rc::new(move |call| *sink.borrow_mut() = Some(call)),
            )
            .expect("serve");
        let mut request = RpcRequestPayload {
            service: RELAY_SERVICE.to_string(),
            deadline_ns: 0,
            flags: 0,
            headers: Vec::new(),
            body: Bytes::new(),
        };
        attach_signed_admission(
            &mut request,
            &world.intent(),
            call_id,
            RELAY_SERVICE,
            RpcCallShape::Unary,
            Some(world.binding),
            RELAY_NOW_NS,
        )
        .expect("mint");
        let outcome = serves.on_request(
            &world.serve_peer(),
            RELAY_SERVICE,
            call_id,
            request,
            RELAY_NOW_NS,
            &world.admission(),
        );
        assert_eq!(outcome, OpenOutcome::Admitted, "the fixture admits");
        let call = captured.borrow_mut().take();
        (serves, call.expect("the handler hook ran"))
    }

    /// LEAF-5: a call handle resolves only inside the namespace of
    /// the sender that claimed it. Read (`OrgNext`), inject
    /// (`OrgSend`) and cancel (`OrgCancel`) all resolve through
    /// `stream_call` — a foreign sender on the exact colliding id
    /// resolves NOTHING, and the victim's entry is untouched.
    #[test]
    fn a_follower_cannot_read_inject_or_cancel_another_followers_call() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        let attacker = ProxySide::Follower(2);
        let call = 777;
        relay
            .install_call(owner, call, relay_call_handle(&world, 0x1111))
            .expect("the owner installs its call");

        assert!(relay.stream_call(owner, call).is_some());
        assert_eq!(
            relay.stream_call(attacker, call),
            None,
            "a guessed/colliding id must read no call — pre-fix the id alone resolved it"
        );
        assert_eq!(relay.stream_call(ProxySide::Leader, call), None);
        assert!(relay.call_live(call), "and the victim's entry stands");
    }

    /// LEAF-5's clobber half: a colliding `OrgCall` insert must leave
    /// the live entry intact. Pre-fix the insert overwrote it — and
    /// dropping the victim's `CallHandle` is the victim call's
    /// exactly-one CANCEL.
    #[test]
    fn a_colliding_call_insert_leaves_the_live_entry_intact() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        let attacker = ProxySide::Follower(2);
        let call = 778;
        relay
            .install_call(owner, call, relay_call_handle(&world, 0x1111))
            .expect("the victim's insert");
        let before = relay.stream_call(owner, call).expect("the victim resolves");

        let Err((failure, returned)) =
            relay.install_call(attacker, call, relay_call_handle(&world, 0x2222))
        else {
            panic!("a colliding insert must be refused, never applied");
        };
        assert!(
            matches!(failure, ProxyFailure::Typed(LeafError::Session(_))),
            "the collision is typed: {failure:?}"
        );
        drop(returned);
        assert_eq!(
            relay.stream_call(owner, call),
            Some(before),
            "the victim keeps its own handle — a clobber would have cancelled its call"
        );
    }

    /// LEAF-13: the call entry goes WITH its terminal. Pre-fix the
    /// map held one `CallHandle`-bearing entry per call it ever
    /// carried (unbounded growth), and a re-delivered terminal found
    /// the entry and re-applied it.
    #[test]
    fn a_call_entry_is_released_with_its_terminal_and_never_re_applied() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        let call = 779;
        relay
            .install_call(owner, call, relay_call_handle(&world, 0x1111))
            .expect("install");

        assert!(
            relay.release_call(owner, call),
            "terminal delivery releases"
        );
        assert!(!relay.call_live(call), "the entry is gone");
        assert_eq!(
            relay.stream_call(owner, call),
            None,
            "a re-delivered terminal finds nothing left to re-apply"
        );
        assert!(
            !relay.release_call(owner, call),
            "and a second release changes nothing"
        );
    }

    /// LEAF-4: unregistration acts on the registration the leader
    /// RECORDED for the requesting follower — never a request's
    /// claimed name, and never another follower's registration. Pre-fix
    /// the `OrgServeUnregister` arm unshared whatever name the request
    /// claimed: one follower's request killed another follower's
    /// service and retired its live calls.
    #[test]
    fn an_unregister_touches_only_the_registrations_the_sender_own() {
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        let attacker = ProxySide::Follower(2);
        assert_eq!(relay.claim_registration(owner, 7, "svc.a"), Ok(true));

        assert!(
            relay.claim_registration(attacker, 7, "svc.b").is_err(),
            "a foreign registration id is refused, never re-bound"
        );
        assert!(
            !relay.drop_registration(attacker, 7),
            "an attacker's unregister affects nothing"
        );
        assert_eq!(
            relay.registration_service(owner, 7),
            Some("svc.a".to_string()),
            "the owner's registration — and the name it recorded — stand intact"
        );
        assert_eq!(relay.registration_service(attacker, 7), None);
    }

    /// The recorded-name half of LEAF-4: the leader records the name
    /// at registration and unregistration acts on THAT — a request's
    /// own service claim is no longer even carried, and an owner
    /// re-binding its live handle to another name is refused rather
    /// than re-pointed.
    #[test]
    fn an_unregister_removes_the_recorded_name_and_only_for_its_owner() {
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        assert_eq!(relay.claim_registration(owner, 7, "svc.a"), Ok(true));
        assert!(
            relay.claim_registration(owner, 7, "svc.other").is_err(),
            "a live registration keeps the name it recorded"
        );
        assert_eq!(
            relay.registration_service(owner, 7),
            Some("svc.a".to_string())
        );
        assert!(relay.drop_registration(owner, 7), "the owner unregisters");
        assert_eq!(
            relay.registration_service(owner, 7),
            None,
            "and exactly that registration went"
        );
    }

    /// LEAF-5's served half: a served call resolves only for the
    /// follower whose registration it was admitted to — read
    /// (`OrgServeRequest`), inject (`OrgServeSend`) and
    /// finish/retire resolve nothing for anyone else, on the exact
    /// id. The accept queue is the owner's alone.
    #[test]
    fn a_served_call_resolves_only_for_its_owning_follower() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        let attacker = ProxySide::Follower(2);
        relay
            .claim_registration(owner, 7, RELAY_SERVICE)
            .expect("claim");
        let bridge = match relay.install_serve(owner, 7, relay_serve_call(&world, 0x2000)) {
            Ok(bridge) => bridge,
            Err(_) => panic!("the admitted call parks under its owner"),
        };

        assert!(relay.serve_call(owner, bridge).is_some());
        assert!(
            relay.serve_call(attacker, bridge).is_none(),
            "a guessed or foreign id resolves no served call — pre-fix the id alone did"
        );
        assert!(
            relay.accept_admitted(attacker, 7).is_err(),
            "and no other follower pops this registration's accepts"
        );
        assert_eq!(relay.accept_admitted(owner, 7), Ok(Some(bridge)));
    }

    /// LEAF-5: bridge handles are CSPRNG-seeded, not a counter from
    /// 1 — a sequential id on a channel every tab can read is a
    /// guessable name for a live served call (pre-fix `next_serve_call`
    /// counted up from zero).
    #[test]
    fn a_bridge_handle_is_not_the_first_guess() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        relay
            .claim_registration(ProxySide::Follower(1), 7, RELAY_SERVICE)
            .expect("claim");
        let bridge = match relay.install_serve(
            ProxySide::Follower(1),
            7,
            relay_serve_call(&world, 0x2000),
        ) {
            Ok(bridge) => bridge,
            Err(_) => panic!("the admitted call parks under its owner"),
        };
        assert_ne!(
            bridge, 1,
            "pre-fix the first bridge handle was the guessable 1"
        );
    }

    /// §23 audit: bridge handles are drawn per call, not counted up from
    /// one random seed — one observed accept envelope must not predict
    /// the next live handle.
    #[test]
    fn consecutive_bridge_handles_are_not_sequential() {
        let world = RelayWorld::new();
        let mut relay = OrgRelay::default();
        let owner = ProxySide::Follower(1);
        relay
            .claim_registration(owner, 7, RELAY_SERVICE)
            .expect("claim");
        let mut install = |id| match relay.install_serve(owner, 7, relay_serve_call(&world, id)) {
            Ok(bridge) => bridge,
            Err(_) => panic!("the admitted call parks under its owner"),
        };
        let first = install(0x2000);
        let second = install(0x2001);
        assert_ne!(
            second,
            first.wrapping_add(1),
            "pre-fix the second handle was the first plus one"
        );
    }

    /// LEAF-13, the served-call half (§23 audit): a served call is
    /// released once its terminal has committed — by completion or by
    /// retirement — and only by its owner. Pre-audit `serve_calls` was
    /// pruned only by `drop_registration`/`clear`, so a long-lived
    /// registration kept one handle-holding entry per call it ever
    /// served.
    #[test]
    fn a_settled_served_call_is_released_by_its_owner_only() {
        let world = RelayWorld::new();
        let owner = ProxySide::Follower(1);
        let attacker = ProxySide::Follower(2);
        let mut relay = OrgRelay::default();
        relay
            .claim_registration(owner, 7, RELAY_SERVICE)
            .expect("claim");

        // Completion.
        let (mut serves, call) = relay_serve_call_in(&world, 0x2000);
        let Ok(bridge) = relay.install_serve(owner, 7, call.clone()) else {
            panic!("the admitted call parks under its owner");
        };
        assert!(
            !relay.release_serve(owner, bridge),
            "a live call is never released"
        );
        call.finish(crate::rpc_wire::StreamHandlerResult::Ok);
        serves.advance(RELAY_NOW_NS);
        assert!(call.settled(), "the unary terminal committed");
        assert!(
            !relay.release_serve(attacker, bridge),
            "another sender cannot release it"
        );
        assert!(relay.release_serve(owner, bridge));
        assert!(relay.serve_call(owner, bridge).is_none());
        assert!(
            !relay.release_serve(owner, bridge),
            "a second release changes nothing"
        );

        // Retirement.
        let (mut serves, call) = relay_serve_call_in(&world, 0x2001);
        let Ok(bridge) = relay.install_serve(owner, 7, call.clone()) else {
            panic!("the admitted call parks under its owner");
        };
        serves.fail_all(crate::rpc_stream::RetireReason::NodeClosed);
        assert!(call.settled(), "a retirement settles the call");
        assert!(relay.release_serve(owner, bridge));
        assert_eq!(relay.serve_call_count(), 0, "nothing accumulates");
    }
}
