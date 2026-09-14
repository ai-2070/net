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

use std::collections::{BTreeSet, HashMap};
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
pub const PROXY_VERSION: u64 = 1;

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

// ──────────────────────────── follower registry ────────────────────────

/// Which followers are attached and what each one declared.
///
/// The leader keeps this so a *new* leader can re-establish the
/// subscriptions its followers depend on (D2 "Restoration"). It is
/// the union, not the last writer: two followers wanting two channels
/// both get theirs.
#[derive(Debug, Clone, Default)]
pub struct FollowerRegistry {
    followers: HashMap<u64, BTreeSet<String>>,
}

impl FollowerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach `follower` with the subscriptions it declared. Re-attach
    /// replaces the declaration, which is what a follower that
    /// survived a leader change sends.
    pub fn attach(&mut self, follower: u64, subscriptions: impl IntoIterator<Item = String>) {
        self.followers
            .insert(follower, subscriptions.into_iter().collect());
    }

    /// Record a channel a follower subscribed to after attaching.
    pub fn declare(&mut self, follower: u64, channel: &str) {
        self.followers
            .entry(follower)
            .or_default()
            .insert(channel.to_string());
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
            union.extend(declared.iter().cloned());
        }
        union.into_iter().collect()
    }
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
    },
    /// Send on an open stream.
    StreamSend {
        /// The stream.
        stream_id: u64,
        /// The payload.
        payload: Bytes,
    },
    /// Close an open stream.
    StreamClose {
        /// The stream.
        stream_id: u64,
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
    /// A stream was opened, with the wire id it got.
    Stream {
        /// The wire stream id.
        stream_id: u64,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    },
    /// Follower → leader: "I am going away."
    Detach,
    /// Follower → leader: do this, and answer under `correlation`.
    Request {
        /// The follower's correlation id.
        correlation: u64,
        /// What to do.
        request: LeaderRequest,
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
            ProxyBody::Attach { subscriptions } => {
                map.insert("kind".into(), Value::from("attach"));
                map.insert("subscriptions".into(), strings(subscriptions));
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
            },
            "detach" => ProxyBody::Detach,
            "request" => ProxyBody::Request {
                correlation: u64_field(&value, "correlation")?,
                request: decode_request(field(&value, "request")?)?,
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
    generation: u64,
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

    /// Answer with an opened stream's wire id.
    pub fn stream(mut self, stream_id: u64) {
        self.send(ProxyBody::Reply {
            correlation: self.correlation,
            value: ProxyValue::Stream { stream_id },
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
    pub fn local(answer: oneshot::Sender<ProxyOutcome>, generation: u64) -> Self {
        Self {
            sink: Some(ReplySink::Local(answer)),
            generation,
            correlation: 0,
        }
    }

    fn send(&mut self, body: ProxyBody) {
        let Some(sink) = self.sink.take() else {
            return;
        };
        match sink {
            ReplySink::Channel(transport) => transport.post(
                &ProxyEnvelope {
                    generation: self.generation,
                    from: ProxySide::Leader,
                    body,
                }
                .to_json(),
            ),
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

impl Drop for Replier {
    /// A replier dropped without an answer is a lost correlation, and
    /// a follower's promise that never settles is worse than a
    /// failure: it looks like latency forever. So dropping answers.
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

    /// Perform one request, answering through `reply`.
    ///
    /// The replier is taken by value and consumed by whichever
    /// answer it gets, so a double answer is a compile error and a
    /// forgotten one is caught by its `Drop`. An implementation that
    /// needs to await something moves the replier into the future.
    fn perform(&mut self, request: LeaderRequest, reply: Replier);
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

    fn perform(&mut self, request: LeaderRequest, reply: Replier) {
        (**self).perform(request, reply);
    }
}

/// The leader's half of the proxy.
pub struct ProxyServer<B: LeaderBackend> {
    backend: B,
    transport: Rc<dyn ProxyTransport>,
    generation: u64,
    followers: FollowerRegistry,
    stamped: u64,
    superseded: Option<u64>,
    closed: bool,
}

impl<B: LeaderBackend> ProxyServer<B> {
    /// A server for `generation`, posting on `transport`.
    pub fn new(backend: B, transport: Rc<dyn ProxyTransport>, generation: u64) -> Self {
        Self {
            backend,
            transport,
            generation,
            followers: FollowerRegistry::new(),
            stamped: 0,
            superseded: None,
            closed: false,
        }
    }

    /// The generation this leader holds.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
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

    /// Tell every tab that `lost`'s in-flight work is dead.
    pub fn announce_leader_lost(&mut self, lost: u64) {
        self.post(ProxyBody::LeaderLost { lost });
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
            if envelope.generation > self.generation {
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
            ProxyBody::Attach { subscriptions } => {
                self.followers.attach(follower, subscriptions);
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
                if envelope.generation > self.generation {
                    self.superseded = Some(envelope.generation);
                }
                if envelope.generation != self.generation {
                    let error = LeafError::NotLeader {
                        presented: envelope.generation,
                        current: Some(self.generation),
                    };
                    self.replier(correlation)
                        .fail(ProxyFailure::Typed(error.clone()));
                    return Err(error);
                }
                if let LeaderRequest::Subscribe { channel } = &request {
                    self.followers.declare(follower, channel);
                }
                let reply = self.replier(correlation);
                self.backend.perform(request, reply);
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
            generation: self.generation,
            correlation,
        }
    }

    fn post(&mut self, body: ProxyBody) {
        self.stamped += 1;
        self.transport.post(
            &ProxyEnvelope {
                generation: self.generation,
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
    pending: HashMap<u64, oneshot::Sender<ProxyOutcome>>,
    next_correlation: u64,
}

impl ProxyClient {
    /// A client for `follower`, declaring `subscriptions`.
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
        correlation_seed: u64,
    ) -> Self {
        Self {
            transport,
            follower,
            gate: GenerationGate::new(),
            leader_node: None,
            subscriptions,
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

    /// Declare the channels this follower depends on and ask whoever
    /// holds the lock to identify itself.
    pub fn attach(&mut self) {
        let subscriptions = self.subscriptions.clone();
        self.post(ProxyBody::Attach { subscriptions });
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
        let (tx, rx) = oneshot::channel();
        let generation = self.gate.highest();
        if generation == 0 {
            let _ = tx.send(Err(ProxyFailure::Typed(LeafError::NotLeader {
                presented: 0,
                current: None,
            })));
            return rx;
        }
        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);
        if let LeaderRequest::Subscribe { channel } = &request {
            if !self.subscriptions.iter().any(|c| c == channel) {
                self.subscriptions.push(channel.clone());
            }
        }
        self.pending.insert(correlation, tx);
        self.post(ProxyBody::Request {
            correlation,
            request,
        });
        rx
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
                // Re-declare: a leader that just took over has no
                // record of this follower.
                if let Admission::NewLeader { previous } = admission {
                    if previous != 0 {
                        self.attach();
                    }
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
        }
        LeaderRequest::StreamSend { stream_id, payload } => {
            map.insert("op".into(), Value::from("stream_send"));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
            map.insert("payload".into(), b64(payload));
        }
        LeaderRequest::StreamClose { stream_id } => {
            map.insert("op".into(), Value::from("stream_close"));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
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
        LeaderRequest::Counters => {
            map.insert("op".into(), Value::from("counters"));
        }
        LeaderRequest::Enroll => {
            map.insert("op".into(), Value::from("enroll"));
        }
        LeaderRequest::IsEnrolled => {
            map.insert("op".into(), Value::from("is_enrolled"));
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
        },
        "stream_send" => LeaderRequest::StreamSend {
            stream_id: u64_field(value, "stream_id")?,
            payload: unb64(value, "payload")?,
        },
        "stream_close" => LeaderRequest::StreamClose {
            stream_id: u64_field(value, "stream_id")?,
        },
        "signal" => LeaderRequest::Signal {
            peer: u64_field(value, "peer")?,
            dialog: u64_field(value, "dialog")?,
            kind: str_field(value, "signal_kind")?.to_string(),
            payload: unb64(value, "payload")?,
        },
        "counters" => LeaderRequest::Counters,
        "enroll" => LeaderRequest::Enroll,
        "is_enrolled" => LeaderRequest::IsEnrolled,
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
        ProxyValue::Stream { stream_id } => {
            map.insert("result".into(), Value::from("stream"));
            map.insert("stream_id".into(), Value::from(stream_id.to_string()));
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
            stream_id: u64_field(value, "stream_id")?,
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
            "malformed" => RpcError::Malformed(detail("detail")?),
            other => {
                return Err(LeafError::ControlPlane(format!(
                    "unknown rpc failure {other:?}"
                )))
            }
        }),
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
/// the field is bounded (`status`).
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

        fn perform(&mut self, request: LeaderRequest, reply: Replier) {
            self.seen.borrow_mut().push(request.clone());
            if self.answer_immediately {
                match request {
                    LeaderRequest::Query { .. } | LeaderRequest::Counters => {
                        reply.text("[]".into())
                    }
                    LeaderRequest::StreamOpen { .. } => reply.stream(0x0102_0304_0506_0708),
                    LeaderRequest::Call { .. } => reply.bytes(Bytes::from_static(b"pong")),
                    _ => reply.bytes(Bytes::new()),
                }
            } else {
                self.held.borrow_mut().push(reply);
            }
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
            "v": "1",
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
                request: LeaderRequest::StreamSend {
                    stream_id: big,
                    payload: Bytes::from_static(b"x"),
                },
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
            },
            ProxyBody::Detach,
            ProxyBody::Request {
                correlation: 9,
                request: LeaderRequest::Call {
                    service: "svc".into(),
                    payload: Bytes::from_static(b"body"),
                    timeout_ms: Some(1500),
                },
            },
            ProxyBody::Request {
                correlation: 10,
                request: LeaderRequest::Call {
                    service: "svc".into(),
                    payload: Bytes::new(),
                    timeout_ms: None,
                },
            },
            ProxyBody::Request {
                correlation: 11,
                request: LeaderRequest::Subscribe {
                    channel: "chan".into(),
                },
            },
            ProxyBody::Request {
                correlation: 12,
                request: LeaderRequest::Publish {
                    channel: "chan".into(),
                    payload: Bytes::from_static(b"p"),
                },
            },
            ProxyBody::Request {
                correlation: 13,
                request: LeaderRequest::Announce {
                    capabilities: vec!["cap".into()],
                },
            },
            ProxyBody::Request {
                correlation: 14,
                request: LeaderRequest::Query {
                    capability: "cap".into(),
                },
            },
            ProxyBody::Request {
                correlation: 15,
                request: LeaderRequest::StreamOpen {
                    label: "app".into(),
                    reliability: Reliability::FireAndForget,
                    stream_id: Some(77),
                    channel_hash: Some(0xBEEF),
                },
            },
            ProxyBody::Request {
                correlation: 16,
                request: LeaderRequest::StreamOpen {
                    label: "app".into(),
                    reliability: Reliability::Reliable,
                    stream_id: None,
                    channel_hash: None,
                },
            },
            ProxyBody::Request {
                correlation: 17,
                request: LeaderRequest::StreamClose { stream_id: 5 },
            },
            ProxyBody::Request {
                correlation: 18,
                request: LeaderRequest::Signal {
                    peer: 0xAAAA,
                    dialog: 7,
                    kind: "offer".into(),
                    payload: Bytes::from_static(b"v=0"),
                },
            },
            ProxyBody::Request {
                correlation: 19,
                request: LeaderRequest::Counters,
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
                value: ProxyValue::Stream { stream_id: 31 },
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
        let text =
            envelope(1, ProxySide::Leader, ProxyBody::Detach).replace("\"v\":\"1\"", "\"v\":\"2\"");
        let error = ProxyEnvelope::from_json(&text).expect_err("a v2 envelope must be refused");
        assert!(error.to_string().contains("version 2"), "{error}");
        assert!(ProxyEnvelope::from_json("not json").is_err());
        assert!(ProxyEnvelope::from_json("{\"v\":\"1\"}").is_err());
    }

    // ────────────────────────── the follower registry ────────────────────

    /// Restoration is the union, not the last writer: two followers
    /// wanting two channels both get theirs back.
    #[test]
    fn restoration_is_the_union_of_every_followers_declaration() {
        let mut registry = FollowerRegistry::new();
        assert!(registry.is_empty());
        registry.attach(1, ["alpha".to_string(), "beta".to_string()]);
        registry.attach(2, ["beta".to_string(), "gamma".to_string()]);
        registry.declare(2, "delta");
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.subscription_union(),
            vec!["alpha", "beta", "delta", "gamma"],
            "deduplicated and ordered, so the restoration sequence is deterministic"
        );

        registry.detach(2);
        assert_eq!(registry.subscription_union(), vec!["alpha", "beta"]);
        // Re-attach replaces a declaration rather than merging into
        // it: a follower that dropped a channel must not have it kept
        // alive forever.
        registry.attach(1, ["alpha".to_string()]);
        assert_eq!(registry.subscription_union(), vec!["alpha"]);
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
        server.announce_leader_lost(6);
        server
            .on_message(&envelope(
                7,
                ProxySide::Follower(1),
                ProxyBody::Request {
                    correlation: 1,
                    request: LeaderRequest::Query {
                        capability: "cap".into(),
                    },
                },
            ))
            .expect("served");

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
                        request: LeaderRequest::Call {
                            service: "svc".into(),
                            payload: Bytes::new(),
                            timeout_ms: None,
                        },
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
                    request: LeaderRequest::Counters,
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
                    request: LeaderRequest::Counters,
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
        let reply = Replier::local(tx, server.generation());
        server.backend_mut().perform(
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
        let mut client = ProxyClient::new(transport.clone(), 42, vec!["chan".into()], 100);

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
                ProxyBody::Attach { subscriptions } if subscriptions == &vec!["chan".to_string()]
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
        let mut client = ProxyClient::new(transport.clone(), 42, vec![], 100);
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
        let mut client = ProxyClient::new(transport, 42, vec![], 100);
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
        let mut client = ProxyClient::new(transport.clone(), 42, vec![], 100);

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
        let mut client = ProxyClient::new(to_leader.clone(), 77, vec!["chan".into()], 500);

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
}
