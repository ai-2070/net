//! The browser half of §8 leader election: the Web Lock, the
//! `BroadcastChannel` carrier, and the session JavaScript holds.
//!
//! [`crate::leader`] owns the lifecycle — generations, fencing, the
//! follower registry, both ends of the proxy — and touches no browser
//! API, which is why it is tested natively. This module is the part
//! that can only run in a tab.
//!
//! # Why the Web Lock goes through `js_sys::Reflect`
//!
//! `web-sys`'s `LockManager` is gated behind
//! `--cfg=web_sys_unstable_apis`, which is a crate-wide `RUSTFLAGS`
//! every consumer and every CI job would have to carry to build the
//! leaf at all. The API itself is not unstable in the browsers Stage 5
//! targets (Chromium since 69, Firefox since 96); only the binding is.
//! So the call is made through `Reflect` on `navigator.locks`, which
//! needs no flag, and the shape is asserted rather than assumed: a
//! context with no `navigator.locks` is a **typed refusal**, not a
//! fallback to running two nodes on one identity.
//!
//! # `BroadcastChannel`, and not `MessagePort`
//!
//! §8 names both. `BroadcastChannel` is the one that needs no
//! rendezvous: a `MessagePort` pair has to be handed over by something,
//! and the only same-origin cross-tab primitive that could hand it
//! over is `BroadcastChannel` itself. So the channel *is* the carrier;
//! a port would be an optimisation over a channel that is already
//! carrying one JSON message per operation.
//!
//! # The composition
//!
//! `Lifecycle` is the whole session minus the node: it contends for
//! the lock, bumps the generation, serves followers, promotes itself
//! when the lock comes free, and restores subscriptions. The node
//! arrives through a `BackendFactory`, which is what lets the
//! lifecycle be driven in a browser test with no anchor in reach.
//! [`MeshSession`] is `Lifecycle` with the real
//! `crate::wasm::LeafNode` wired into that factory, and is the type
//! `@net-mesh/browser` talks to.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_channel::oneshot;
use js_sys::{Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{BroadcastChannel, MessageEvent};

use crate::bootstrap::gloo_timer_sleep;
use crate::error::{LeafError, Result, RpcError};
use crate::identity::IdentitySecrets;
use crate::leader::{
    scope_name, FollowerEvent, GenerationLease, LeaderBackend, LeaderRequest, ProxyClient,
    ProxyFailure, ProxyOutcome, ProxyServer, ProxyTransport, ProxyValue, Replier,
};
use crate::rpc::DEFAULT_CALL_TIMEOUT_MS;
use crate::storage::{IdentityVault, DEFAULT_DB_NAME};
use crate::stream::Reliability;
use crate::stream_ownership::{answer_stream_request, StreamBackend, StreamOwnership};

/// How often a leader revalidates its generation against the store.
///
/// Coarse on purpose. The in-memory [`GenerationLease`] is the
/// authority at the effect boundary because it costs a `Cell` read;
/// this is the only check that can see the one takeover no in-memory
/// state witnesses — a tab frozen while holding its lock, whose
/// successor bumped the IndexedDB counter while it was not running —
/// and an IndexedDB transaction in front of every send would be a
/// worse fence than none, because it would be an await.
const LEASE_REVALIDATION_MS: i32 = 1_000;

/// What a follower adds to the caller's timeout before giving up on
/// its own clock.
///
/// The caller's deadline belongs to the *operation*; the proxy round
/// trip is not part of it. Expiring at exactly `timeout_ms` would
/// refuse replies that were already on the channel, which would turn
/// a working call into an indeterminate one for no reason.
const PROXY_DEADLINE_GRACE_MS: u32 = 250;

/// The most inbound proxy messages held while a leader bootstraps.
///
/// A ceiling rather than an unbounded queue: the window is one
/// `connect()`, and anything that fills this is a flood rather than a
/// startup. Overflow is reported rather than silently forgotten.
const MAX_QUEUED_INBOUND: usize = 64;

/// How long a tab whose re-bootstrap failed waits before asking for
/// the lock again.
///
/// It has just released the only lock there is, so an immediate
/// re-request is granted immediately: without a pause a persistently
/// failing bootstrap would spin on acquisition rather than retry it.
/// Retrying at all is deliberate — the origin has no node after a
/// failed promotion, and a tab that gave up would leave the page with
/// a follower attached to nothing.
const PROMOTION_RETRY_MS: i32 = 250;

// ────────────────────────────── the Web Lock ───────────────────────────

/// A held Web Lock.
///
/// The API hands out no release handle: a lock is held for exactly as
/// long as the promise the callback returned stays pending. So this
/// **is** that promise's `resolve`, and dropping it lets go.
#[derive(Debug)]
pub struct WebLock {
    release: Option<Function>,
    name: String,
}

impl WebLock {
    /// The lock's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Let go. Idempotent.
    pub fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

impl Drop for WebLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// `navigator.locks`, or a typed refusal.
fn lock_manager() -> Result<JsValue> {
    let navigator = window()?.navigator();
    let locks = Reflect::get(navigator.as_ref(), &JsValue::from_str("locks"))
        .map_err(|e| js_err("reading navigator.locks", &e))?;
    if locks.is_undefined() || locks.is_null() {
        // Not a fallback. Without a lock there is no way to keep two
        // tabs from presenting one identity from two sessions, which
        // is the eviction §8 exists to prevent.
        return Err(LeafError::Identity(
            "this context has no Web Locks API, so one-node-per-origin cannot be \
             enforced; the leaf refuses to run rather than let two tabs evict \
             each other under the identity-rebind rules"
                .into(),
        ));
    }
    Ok(locks)
}

/// Request `name` exclusively.
///
/// With `if_available` the promise settles immediately: `Some` when the
/// lock was free, `None` when someone else holds it. Without it, the
/// request **queues** and settles when the current holder lets go —
/// which is the promotion path, and the reason a follower needs no
/// polling and no heartbeat.
pub async fn request_web_lock(name: &str, if_available: bool) -> Result<Option<WebLock>> {
    let locks = lock_manager()?;
    let request = Reflect::get(&locks, &JsValue::from_str("request"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .ok_or_else(|| LeafError::Identity("navigator.locks.request is not callable".into()))?;

    let (tx, rx) = oneshot::channel::<Result<Option<Function>>>();
    let slot = Rc::new(RefCell::new(Some(tx)));

    let granted_slot = slot.clone();
    let granted = Closure::once_into_js(move |lock: JsValue| -> JsValue {
        if lock.is_null() || lock.is_undefined() {
            // `ifAvailable` and somebody else has it.
            if let Some(tx) = granted_slot.borrow_mut().take() {
                let _ = tx.send(Ok(None));
            }
            return js_sys::Promise::resolve(&JsValue::UNDEFINED).into();
        }
        // The lock lives as long as this promise is pending. Its
        // `resolve` is the only release handle there is.
        let mut release = None;
        let held = js_sys::Promise::new(&mut |resolve, _reject| {
            release = Some(resolve);
        });
        if let Some(tx) = granted_slot.borrow_mut().take() {
            let _ = tx.send(Ok(release));
        }
        held.into()
    });

    let options = Object::new();
    set(&options, "mode", &JsValue::from_str("exclusive"))?;
    if if_available {
        set(&options, "ifAvailable", &JsValue::TRUE)?;
    }

    let outer = request
        .call3(&locks, &JsValue::from_str(name), &options, &granted)
        .map_err(|e| js_err("navigator.locks.request", &e))?;

    // The outer promise rejects if the request itself was refused
    // (unsupported mode, an aborted signal). Without watching it, that
    // rejection would present as a request that simply never settles.
    let failed_slot = slot;
    spawn_local(async move {
        if let Ok(promise) = outer.dyn_into::<js_sys::Promise>() {
            if let Err(error) = JsFuture::from(promise).await {
                if let Some(tx) = failed_slot.borrow_mut().take() {
                    let _ = tx.send(Err(js_err("the Web Lock request was refused", &error)));
                }
            }
        }
    });

    let release = rx.await.unwrap_or_else(|_| {
        Err(LeafError::Identity(
            "the Web Lock request was dropped before it settled".into(),
        ))
    })?;
    Ok(release.map(|release| WebLock {
        release: Some(release),
        name: name.to_string(),
    }))
}

// ──────────────────────────── the proxy carrier ────────────────────────

/// A `BroadcastChannel` as the proxy's carrier.
struct BroadcastTransport {
    channel: BroadcastChannel,
}

impl ProxyTransport for BroadcastTransport {
    fn post(&self, message: &str) {
        if let Err(error) = self.channel.post_message(&JsValue::from_str(message)) {
            // A closed channel is the ordinary consequence of a tab
            // going away mid-post; it is not worth failing an
            // operation over, but it is worth being visible.
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "net-mesh-leaf: proxy post failed: {}",
                js_err("post_message", &error)
            )));
        }
    }
}

// ─────────────────────────────── the session ───────────────────────────

/// Whether this tab runs the node or talks to the tab that does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// This tab holds the lock and runs the node.
    Leader,
    /// Another tab holds the lock; this one proxies to it.
    Follower,
}

impl Role {
    /// The spelling the JavaScript boundary uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Follower => "follower",
        }
    }
}

/// The future a `BackendFactory` returns.
pub type BackendFuture = Pin<Box<dyn Future<Output = Result<Box<dyn LeaderBackend>>>>>;

/// How a leader gets a node.
///
/// Called with the connect options (already carrying the identity),
/// the sink every node event must be handed to, and the
/// [`GenerationLease`] the node's outbound effects are admitted
/// under. A factory rather than a concrete type because the lifecycle
/// has to be drivable in a browser with no anchor in reach: the
/// leaf's own wasm tests supply a backend with no network behind it
/// and exercise the same promotion, restoration and fencing code the
/// real one runs.
///
/// The lease is an argument rather than something the backend reads
/// off the server later, because the backend exists **before** the
/// server does: it is connected inside this factory, and the
/// operations it spawns from that moment on have to be admitted under
/// the same fence the server's replies are.
pub type BackendFactory = Rc<dyn Fn(JsValue, EventSink, GenerationLease) -> BackendFuture>;

/// Where a leader's node events go.
pub type EventSink = Rc<dyn Fn(&str)>;

struct SessionState {
    role: Role,
    generation: u64,
    scope: String,
    fingerprint: String,
    connect_opts: JsValue,
    /// What this tab wants announced: its constructor's list, replaced
    /// by whatever its last successful `announce()` asked for. D2
    /// promises the *current* announcement is re-published on
    /// takeover, and a tab that restored its opening options would
    /// revert every runtime `announce()` at the next handoff.
    capabilities: Vec<String>,
    /// What this tab, while leader, has actually announced — its own
    /// intent unioned with its followers'. Kept so reconciliation is
    /// idempotent and an attach that changes nothing costs no
    /// announcement.
    announced: Vec<String>,
    /// The channels this tab itself asked for.
    declared: BTreeSet<String>,
    /// The channels the current leader has actually subscribed.
    subscribed: BTreeSet<String>,
    /// Channels the leader told us it re-established.
    restored: Vec<String>,
    listeners: Vec<Function>,
    lock: Option<WebLock>,
    interruption_ms: Option<f64>,
    closed: bool,
}

/// A promotion in flight, and the origin's lock it was granted.
///
/// Owned by [`Shared`] rather than by `take_leadership`'s own frame,
/// and that ownership is the whole of it: a bootstrap parked inside
/// its factory holds this origin's lock, so a `close()` that could
/// not reach it could neither cancel the bootstrap nor let go of the
/// lock. A legitimate queued successor then waited on a session the
/// page had already closed, while the parked connect went on
/// attaching, offering and enrolling for it.
///
/// Both halves are here for orderable reasons. `spawn_local` offers
/// no cancellation, so the only handle there is is a `oneshot` the
/// bootstrap polls — dropping `cancel` wakes it. And the lock is let
/// go by the *woken* frame, after it has retired whatever it
/// installed, never by the closing frame over a bootstrap that is
/// still live: the lock coming free is what promotes a successor, and
/// a successor must not overlap its predecessor's connect.
struct Bootstrap {
    /// The origin's lock, until the promotion installs it or lets go.
    lock: Option<WebLock>,
    /// Dropping this wakes the bootstrap, which then abandons.
    cancel: Option<oneshot::Sender<()>>,
}

/// A future that ends when its cancellation sender is dropped.
///
/// `Poll::Ready(None)` is cancellation, and the check comes
/// **first**: a bootstrap whose factory completed in the same turn a
/// close cancelled it must not go on to publish a leader for a
/// session the page has closed.
struct Cancellable<F> {
    op: Pin<Box<F>>,
    cancelled: oneshot::Receiver<()>,
}

impl<F: Future> Future for Cancellable<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if Pin::new(&mut self.cancelled).poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        self.op.as_mut().poll(cx).map(Some)
    }
}

/// Everything the session shares between its callbacks.
///
/// Separate `RefCell`s, not one: the node's event callback fires
/// from inside a `perform` that already holds the server borrow, and a
/// single cell would make that a panic. Events additionally go through
/// a queue drained on a microtask, for the same reason
/// [`crate::wasm`]'s inbox exists — a callback must not re-enter a cell
/// its caller may hold.
struct Shared {
    state: RefCell<SessionState>,
    server: RefCell<Option<ProxyServer<Box<dyn LeaderBackend>>>>,
    client: RefCell<Option<ProxyClient>>,
    events: RefCell<Vec<String>>,
    /// Node events owed to the **followers**, held until no server
    /// borrow is outstanding.
    ///
    /// Separate from `events` because the two have different owners:
    /// `events` goes to this tab's listeners and touches nothing
    /// else, while a broadcast needs the server that a synchronous
    /// backend arm may already be holding. See [`event_sink`].
    broadcasts: RefCell<Vec<String>>,
    /// The fence in force for this tab while it is the leader.
    ///
    /// Held here as well as inside the server because stand-down needs
    /// it *after* the server is gone: the order is fence, then close
    /// the node, then release the lock, and a lease that lived only in
    /// the server would be dropped by step two.
    lease: RefCell<Option<GenerationLease>>,
    /// Proxy messages that arrived while this tab was bootstrapping as
    /// leader — after the lock was granted and before a server
    /// existed.
    ///
    /// Dropping them loses a follower's `Attach`, and a follower that
    /// attached into that window is taught the generation by the
    /// Leadership broadcast but re-declares only on a *subsequent*
    /// one: its subscriptions would simply never reach the node.
    queued: RefCell<Vec<String>>,
    /// How many queued messages were dropped for want of room, so the
    /// ceiling is observable rather than silent.
    overflowed: Cell<u64>,
    /// The promotion this tab has in flight, if any: see
    /// [`Bootstrap`]. Here so `close` can end one — the lock a
    /// parked bootstrap holds is the origin's only lock.
    bootstrap: RefCell<Option<Bootstrap>>,
    vault: Rc<IdentityVault>,
    transport: Rc<BroadcastTransport>,
    channel: BroadcastChannel,
    factory: BackendFactory,
}

/// One tab's participation in the origin's single node.
///
/// The whole of [`MeshSession`] except the choice of node: it contends
/// for the lock, stamps and fences the generation, serves followers,
/// promotes itself when the lock comes free, and restores the
/// subscriptions and the announcement.
#[derive(Clone)]
pub struct Lifecycle {
    shared: Rc<Shared>,
}

impl Lifecycle {
    /// Resolve the identity, contend for the lock, and come back as
    /// whichever role this tab got.
    ///
    /// `opts` is [`MeshSession::open`]'s options object: everything
    /// `LeafNode::connect` reads, plus `capabilities`,
    /// `subscriptions`, `dbName` and `lockScope`.
    pub async fn open(opts: JsValue, factory: BackendFactory) -> Result<Self> {
        let db_name =
            optional_string(&opts, "dbName").unwrap_or_else(|| DEFAULT_DB_NAME.to_string());
        let vault = Rc::new(IdentityVault::open(&db_name).await?);

        let origin = optional_string(&opts, "origin").ok_or_else(|| {
            LeafError::Identity(
                "origin is required: it is the trust boundary the identity is stored \
                 under, and the scope the lock is named after"
                    .into(),
            )
        })?;
        let (connect_opts, fingerprint) = identity_opts(&opts, &vault).await?;
        let scope = optional_string(&opts, "lockScope")
            .unwrap_or_else(|| scope_name(&origin, &fingerprint));

        let channel = BroadcastChannel::new(&scope)
            .map_err(|e| js_err("opening the proxy BroadcastChannel", &e))?;
        let transport = Rc::new(BroadcastTransport {
            channel: channel.clone(),
        });

        let declared: BTreeSet<String> = string_array(&opts, "subscriptions").into_iter().collect();
        let shared = Rc::new(Shared {
            state: RefCell::new(SessionState {
                role: Role::Follower,
                generation: 0,
                scope: scope.clone(),
                fingerprint,
                connect_opts,
                capabilities: string_array(&opts, "capabilities"),
                announced: Vec::new(),
                declared,
                subscribed: BTreeSet::new(),
                restored: Vec::new(),
                listeners: Vec::new(),
                lock: None,
                interruption_ms: None,
                closed: false,
            }),
            server: RefCell::new(None),
            client: RefCell::new(None),
            events: RefCell::new(Vec::new()),
            broadcasts: RefCell::new(Vec::new()),
            lease: RefCell::new(None),
            queued: RefCell::new(Vec::new()),
            overflowed: Cell::new(0),
            bootstrap: RefCell::new(None),
            vault,
            transport,
            channel,
            factory,
        });

        install_channel_listener(&shared);

        // One round of contention. `ifAvailable` so a follower learns
        // it is a follower immediately instead of blocking behind the
        // leader for the lifetime of the tab.
        match request_web_lock(&scope, true).await? {
            Some(lock) => bootstrap_leadership(&shared, lock, None).await?,
            None => {
                attach_as_follower(&shared)?;
                // Queue for the lock. This resolves only when the
                // leader lets go, which is the promotion trigger:
                // no polling, no heartbeat, no timeout to tune.
                let waiting = Self {
                    shared: shared.clone(),
                };
                spawn_local(async move { waiting.await_promotion().await });
            }
        }

        Ok(Self { shared })
    }

    /// This tab's role.
    pub fn role(&self) -> Role {
        self.shared.state.borrow().role
    }

    /// The generation in force for this tab.
    pub fn generation(&self) -> u64 {
        self.shared.state.borrow().generation
    }

    /// The identity fingerprint the lock and channel are named after.
    pub fn fingerprint(&self) -> String {
        self.shared.state.borrow().fingerprint.clone()
    }

    /// The lock and channel name.
    pub fn scope(&self) -> String {
        self.shared.state.borrow().scope.clone()
    }

    /// Milliseconds from the lock coming free to this tab having
    /// re-bootstrapped and restored — the measured interruption
    /// budget, `None` on a tab that was the first leader.
    pub fn interruption_ms(&self) -> Option<f64> {
        self.shared.state.borrow().interruption_ms
    }

    /// The node id of whoever runs the node.
    pub fn node_id(&self) -> Option<u64> {
        if let Some(server) = self.shared.server.borrow().as_ref() {
            return Some(server.node_id());
        }
        self.shared
            .client
            .borrow()
            .as_ref()
            .and_then(ProxyClient::leader_node)
    }

    /// The channels the leader has told this tab are live.
    pub fn restored(&self) -> Vec<String> {
        self.shared.state.borrow().restored.clone()
    }

    /// Perform one operation, wherever the node is.
    ///
    /// A leader answers it locally through the same
    /// [`LeaderBackend`] a follower's request goes through; a follower
    /// posts it, fenced, and waits. One path, so there is no operation
    /// that behaves differently depending on which tab you opened
    /// first.
    pub async fn request(&self, request: LeaderRequest) -> ProxyOutcome {
        if self.shared.state.borrow().closed {
            return Err(ProxyFailure::Typed(LeafError::Session(
                "the session is closed".into(),
            )));
        }
        // Read before the request is moved: a follower arms its own
        // clock over the caller's deadline.
        //
        // Over the **default** as well as an explicit one, which is
        // L4: `session.call(service, bytes)` passes `None`, the node
        // reads that as [`DEFAULT_CALL_TIMEOUT_MS`] — and applies it
        // only once the leader gets round to executing the request.
        // A leader frozen while holding its lock executes nothing, so
        // an ordinary call with no timeout had no deadline anywhere:
        // the caller's promise waited out the whole suspension. The
        // default is therefore armed here too, with the same
        // disposition an explicit one gets — `Indeterminate`, never a
        // retry, because a deadline on this tab cannot cancel work
        // that may already have been admitted on another.
        let deadline = match &request {
            LeaderRequest::Call { timeout_ms, .. } => Some(
                timeout_ms.unwrap_or(u32::try_from(DEFAULT_CALL_TIMEOUT_MS).unwrap_or(u32::MAX)),
            ),
            _ => None,
        };

        let receiver = {
            let mut server = self.shared.server.borrow_mut();
            if let Some(server) = server.as_mut() {
                let (tx, rx) = oneshot::channel();
                let reply = Replier::local(tx, server.lease());
                server.backend_mut().perform(request, reply);
                rx
            } else {
                drop(server);
                let mut client = self.shared.client.borrow_mut();
                let Some(client) = client.as_mut() else {
                    return Err(ProxyFailure::Typed(LeafError::NotLeader {
                        presented: 0,
                        current: None,
                    }));
                };
                let (correlation, rx) = client.issue(request);
                if let (Some(correlation), Some(ms)) = (correlation, deadline) {
                    // The timer only borrows the client after its own
                    // await, so this borrow is free to stand.
                    self.arm_deadline(correlation, ms);
                }
                rx
            }
        };
        flush_events(&self.shared);

        let outcome = receiver.await.unwrap_or_else(|_| {
            // The replier was dropped without answering and its own
            // `Drop` could not reach us either. Typed, not retried.
            Err(ProxyFailure::Typed(LeafError::Rpc(RpcError::SessionLost)))
        });
        flush_events(&self.shared);
        outcome
    }

    /// Record a channel as this tab's own declared intent, and
    /// subscribe it.
    ///
    /// Separate from [`Self::request`] because the declaration is an
    /// ownership claim and a `Subscribe` is not: reconciliation
    /// subscribes the *union* of this tab's channels and its
    /// followers', and recording that union inside `request` inserted
    /// every follower's channel into this tab's own `declared` set —
    /// so a per-tab declaration stopped being per-tab, and a
    /// follower's channel outlived the follower in whatever tab
    /// happened to hold the lock when it attached.
    pub async fn subscribe(&self, channel: String) -> ProxyOutcome {
        self.shared
            .state
            .borrow_mut()
            .declared
            .insert(channel.clone());
        self.request(LeaderRequest::Subscribe { channel }).await
    }

    /// Drop this tab's claim on `channel`, and give up the membership
    /// only if nobody else still wants it.
    ///
    /// The counterpart to [`Self::subscribe`], and deliberately not
    /// its mirror image. A subscribe is one tab declaring an interest
    /// and the leader holds ONE membership for the union of them, so a
    /// release is a claim being dropped and only the **last** claim
    /// gives the membership up. Unsubscribing on every release would
    /// cancel a sibling tab's delivery — one tab closing a store must
    /// not stop another tab that is still reading the same channel.
    ///
    /// On a leader the union is visible here: this tab's `declared`
    /// beside every follower's declarations. On a follower it is not,
    /// so the request goes to the leader, which parks it and answers
    /// from [`serve_releases`] once it has both sets.
    pub async fn unsubscribe(&self, channel: String) -> ProxyOutcome {
        let is_leader = {
            let mut state = self.shared.state.borrow_mut();
            state.declared.remove(&channel);
            state.role == Role::Leader
        };
        if !is_leader {
            return self.request(LeaderRequest::Unsubscribe { channel }).await;
        }
        if still_wanted(&self.shared, &channel) {
            // Somebody else is reading it. This tab's claim is gone,
            // which is what it asked for, and no frame goes out.
            return Ok(ProxyValue::Bytes(Bytes::new()));
        }
        let outcome = self
            .request(LeaderRequest::Unsubscribe {
                channel: channel.clone(),
            })
            .await;
        if outcome.is_ok() {
            // Reconciliation subscribes what is wanted and not yet
            // subscribed, so forgetting this is what lets a later
            // re-declaration actually re-subscribe.
            self.shared.state.borrow_mut().subscribed.remove(&channel);
        }
        outcome
    }

    /// Publish the announcement, and record it as this tab's current
    /// intent.
    ///
    /// The recording is the point. D2 promises the announcement is
    /// re-published on takeover, and a bare `request` would publish it
    /// once: a tab whose `announce()` succeeded and then became the
    /// leader would re-publish the list it was *constructed* with,
    /// silently dropping whatever the page announced at runtime.
    ///
    /// # A leader announces the origin's intent, not its own
    ///
    /// On a leader this goes through the same reconciliation a
    /// follower's attach does, and that is L3. `announce(C)`
    /// published `C` verbatim, so a leader with intent A beside a
    /// follower with intent B — a union the network had already been
    /// told — replaced A∪B with C, withdrawing a live follower's
    /// capability because *another* tab called `announce()`. The
    /// argument is this tab's intent; the document is the union; the
    /// caller is still told whether publishing it worked.
    ///
    /// A follower's own announcement is proxied as before: the leader
    /// records it against that follower and reconciles, so the union
    /// converges there rather than here.
    pub async fn announce(&self, capabilities: Vec<String>) -> ProxyOutcome {
        if self.shared.state.borrow().role != Role::Leader {
            let outcome = self
                .request(LeaderRequest::Announce {
                    capabilities: capabilities.clone(),
                })
                .await;
            if outcome.is_ok() {
                self.shared.state.borrow_mut().capabilities = capabilities;
            }
            return outcome;
        }
        // Recorded first, because the union is computed from it. Put
        // back on failure: a refused announcement must not leave this
        // tab claiming an intent the network was never told about,
        // which the next reconciliation would then publish as if it
        // had succeeded.
        let previous = core::mem::replace(
            &mut self.shared.state.borrow_mut().capabilities,
            capabilities,
        );
        let outcome = announce_union(&self.shared).await;
        if outcome.is_err() {
            self.shared.state.borrow_mut().capabilities = previous;
        }
        outcome
    }

    /// Arm this tab's own deadline over one follower request.
    ///
    /// The `timeout_ms` inside the request is the **leader's**: it
    /// starts when the leader performs the call and is expired by the
    /// leader's own tick. A leader that is frozen while holding its
    /// lock expires nothing, so without this the caller's promise
    /// waits out the whole suspension and then settles against an
    /// execution that began minutes late.
    ///
    /// What expiry produces is deliberately not `Timeout`: a deadline
    /// on this tab cannot cancel work already admitted on another, so
    /// the outcome is [`RpcError::Indeterminate`] and nothing is
    /// re-issued. A retry here would be a second execution of
    /// something that may have executed once already.
    fn arm_deadline(&self, correlation: u64, timeout_ms: u32) {
        let weak = Rc::downgrade(&self.shared);
        let wait =
            i32::try_from(timeout_ms.saturating_add(PROXY_DEADLINE_GRACE_MS)).unwrap_or(i32::MAX);
        spawn_local(async move {
            let _ = gloo_timer_sleep(wait).await;
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let mut client = shared.client.borrow_mut();
            if let Some(client) = client.as_mut() {
                client.expire(
                    correlation,
                    ProxyFailure::Typed(LeafError::Rpc(RpcError::Indeterminate {
                        deadline_ms: timeout_ms,
                    })),
                );
            }
        });
    }

    /// Open an application stream, bound to the generation that
    /// opened it.
    ///
    /// On the lifecycle rather than on [`MeshSession`] because the
    /// binding is lifecycle state: the handle has to know which
    /// generation's node its id belongs to, and only this layer knows
    /// that. `MeshSession::open_stream` is the `wasm_bindgen`
    /// spelling of it.
    pub async fn open_stream(&self, opts: &JsValue) -> Result<ProxyStream, JsError> {
        let options = crate::wasm::stream_options(opts)?;
        let reliable = options.reliability.is_reliable();
        // Read before the request, not after: a request that crosses a
        // handoff must produce a handle stamped with the generation it
        // was *issued* under, or the stamp would agree with the
        // successor it must refuse.
        let generation = self.generation();
        let value = self
            .request(LeaderRequest::StreamOpen {
                label: options.label,
                reliability: options.reliability,
                stream_id: options.stream_id,
                channel_hash: options.channel_hash,
                // Carried now rather than refused by name: the
                // request addresses the peer on the leader's node, so
                // a follower can put application bytes on a direct
                // leaf-to-leaf session the same way the leader tab
                // can.
                peer: options.peer,
            })
            .await?;
        let ProxyValue::Stream {
            handle,
            stream_id,
            peer,
            incarnation,
        } = value
        else {
            return Err(JsError::new("open_stream did not answer with a stream"));
        };
        Ok(ProxyStream {
            lifecycle: self.clone(),
            handle,
            stream_id,
            peer,
            incarnation,
            reliable,
            generation,
        })
    }

    /// Register a listener for this session's event JSON.
    pub fn on_event(&self, callback: Function) {
        self.shared.state.borrow_mut().listeners.push(callback);
    }

    /// Stand down: cancel a bootstrap that is still running, fence the
    /// generation, retire the node, tell the other tabs, and only then
    /// let go of the lock so a follower can promote.
    ///
    /// The order is the whole of it, and it is not this function's to
    /// improvise: [`ProxyServer::retire`] owns the fence, the node and
    /// the followers and has already finished all three when it
    /// returns, so the lock release below cannot get ahead of the
    /// fence.
    ///
    /// A promotion that has not finished is a fourth owner, and the
    /// one this function used to be unable to see: the lock it was
    /// granted lives in the shared bootstrap cell, not in `state.lock`, so
    /// there was no server to retire, no lease to revoke and no lock
    /// to release — the origin's lock stayed held until the factory
    /// completed on its own. It is cancelled here and releases its own
    /// lock once it has let go of what it was building, which is why
    /// this function does not reach into that cell for it.
    pub fn close(&self) {
        {
            let mut state = self.shared.state.borrow_mut();
            if state.closed {
                return;
            }
            state.closed = true;
        }

        // First, because everything below is about an *installed*
        // leader and this is the other kind. Dropping the sender is
        // the wake: the bootstrap is polled, sees its cancellation
        // before its factory, and abandons.
        if let Some(bootstrap) = self.shared.bootstrap.borrow_mut().as_mut() {
            bootstrap.cancel = None;
        }
        let generation = self.generation();

        // D2 row 1, this tab's own half: nothing is retried.
        if let Some(client) = self.shared.client.borrow_mut().as_mut() {
            client.detach();
            client.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                generation,
            })));
        }
        // Taken out of the cell *before* retiring it: retirement runs
        // the node's close path, which drains events into the sink,
        // and the sink reaches for this same borrow.
        let mut server = self.shared.server.borrow_mut().take();
        if let Some(server) = server.as_mut() {
            let retirement = server.retire(None);
            report(&format!(
                "generation {} stood down: {} pending call(s) failed",
                retirement.generation, retirement.pending_failed
            ));
        }
        drop(server);
        *self.shared.client.borrow_mut() = None;
        *self.shared.lease.borrow_mut() = None;
        self.shared.queued.borrow_mut().clear();
        // Releasing the lock is what triggers a follower's promotion,
        // and it is last: everything above has already happened.
        self.shared.state.borrow_mut().lock = None;
        self.shared.channel.close();
    }

    /// Queue for the lock and take leadership when it comes free.
    async fn await_promotion(self) {
        let scope = self.scope();
        let lock = match request_web_lock(&scope, false).await {
            Ok(Some(lock)) => lock,
            Ok(None) => return,
            Err(error) => {
                report(&format!("waiting for the {scope} lock: {error}"));
                return;
            }
        };
        if self.shared.state.borrow().closed {
            return;
        }
        let previous = self.generation();
        if let Err(error) = bootstrap_leadership(&self.shared, lock, Some(previous)).await {
            report(&format!("promoting to leader of {scope}: {error}"));
        }
    }
}

/// Run one promotion under [`Shared`]'s cancellation, and leave the
/// origin's lock somewhere a `close` can name whichever way it ends.
///
/// The lock and the cancellation handle are published **before** the
/// first await, so every moment between the grant and the
/// installation is a moment `close` can end. And the abandonment
/// below runs *after* the statement that owns the bootstrap's frame
/// has dropped it — an in-flight `connect`, the offer it was waiting
/// on and the node half it had built are gone before the lock they
/// were running under is let go, which is the order the installed
/// stand-down keeps and the one a "drop the lock and hope" fix would
/// break.
async fn bootstrap_leadership(
    shared: &Rc<Shared>,
    lock: WebLock,
    previous: Option<u64>,
) -> Result<()> {
    let (cancel, cancelled) = oneshot::channel();
    *shared.bootstrap.borrow_mut() = Some(Bootstrap {
        lock: Some(lock),
        cancel: Some(cancel),
    });
    let finished = Cancellable {
        op: Box::pin(take_leadership(shared, previous)),
        cancelled,
    }
    .await;
    match finished {
        Some(outcome) => {
            // However it disposed of the lock, the bootstrap is over.
            drop(shared.bootstrap.borrow_mut().take());
            outcome
        }
        None => {
            abandon_bootstrap(shared);
            Ok(())
        }
    }
}

/// End a cancelled bootstrap: let go of whatever it installed, and
/// only then of the origin's lock.
///
/// Reachable two ways, because cancellation can land on either side
/// of the installation: parked in the factory, where there is nothing
/// to retire and the lock is still the bootstrap's; or inside the
/// restoration that follows it, where the server is published and
/// `close` has already retired it. Both are idempotent, and both end
/// with the lock — a successor's promotion is triggered by that
/// release, so it is the last thing that happens here.
fn abandon_bootstrap(shared: &Rc<Shared>) {
    let mut server = shared.server.borrow_mut().take();
    if let Some(server) = server.as_mut() {
        let retirement = server.retire(None);
        report(&format!(
            "generation {} was cancelled mid-bootstrap: {} pending call(s) failed",
            retirement.generation, retirement.pending_failed
        ));
    }
    drop(server);
    *shared.lease.borrow_mut() = None;
    shared.queued.borrow_mut().clear();
    shared.state.borrow_mut().lock = None;
    drop(shared.bootstrap.borrow_mut().take());
}

/// Take the origin's lock out of the bootstrap that was granted it.
fn claim_bootstrap_lock(shared: &Shared) -> Option<WebLock> {
    shared
        .bootstrap
        .borrow_mut()
        .as_mut()
        .and_then(|bootstrap| bootstrap.lock.take())
}

/// Let go of it as a step, rather than as the consequence of a frame
/// being dropped.
fn release_bootstrap_lock(shared: &Shared) {
    drop(claim_bootstrap_lock(shared));
}

/// Give the lock back and queue for it again, after a promotion that
/// could not finish.
///
/// One helper for both ways a promotion fails — the generation
/// transaction and the backend factory — because the consequence is
/// identical and the recovery must be: the origin has no node, so
/// *somebody* has to bring one up. A tab that only logged this would
/// be a working follower attached to a leader that is already gone.
///
/// `generation` is the one this attempt was allocated, or zero when
/// the allocation itself is what failed — exact, because the counter
/// starts at one, so zero can only mean "none was allocated".
fn fall_back_to_follower(
    shared: &Rc<Shared>,
    previous: Option<u64>,
    generation: u64,
    error: &LeafError,
) -> Result<()> {
    // The lock goes back with this frame, before anything else: the
    // attempt is over, and a queued successor is entitled to it.
    release_bootstrap_lock(shared);
    let Some(previous) = previous else {
        // The first leader's failure is the caller's: `open()` rejects
        // and there is no predecessor to have been following.
        return Ok(());
    };
    if shared.state.borrow().closed {
        return Ok(());
    }
    // Whatever the old leader owed this tab is owed by nobody now.
    // Typed and named, not dropped: a sender that merely went away
    // settles as `SessionLost`, which is a weaker answer to "which
    // leader did I lose" than the one this path knows.
    if let Some(client) = shared.client.borrow_mut().as_mut() {
        client.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
            generation: previous,
        })));
    }
    *shared.client.borrow_mut() = None;
    emit(
        shared,
        &format!(
            "{{\"type\":\"promotion_failed\",\"generation\":\"{generation}\",\"detail\":{}}}",
            json_string(&error.to_string())
        ),
    );
    attach_as_follower(shared)?;
    // Queued again, after a pause. Queued because the origin now has
    // no node and somebody has to bring one up. After a pause because
    // this tab has just released the only lock there is, so an
    // immediate re-request is granted immediately: without the delay a
    // persistently failing bootstrap would spin on acquisition instead
    // of retrying it.
    let waiting = Lifecycle {
        shared: shared.clone(),
    };
    spawn_local(async move {
        let _ = gloo_timer_sleep(PROMOTION_RETRY_MS).await;
        if waiting.shared.state.borrow().closed {
            return;
        }
        waiting.await_promotion().await;
    });
    Ok(())
}

/// Become the leader: bump the generation, re-bootstrap, restore.
///
/// The order is the specification's, and each step depends on the one
/// before it:
///
/// 1. **Generation first**, read-increment-written in one IndexedDB
///    transaction. Everything after it is stamped, so nothing may
///    happen before the stamp exists.
/// 2. **Fail the predecessor's work** before announcing, so no caller
///    can see a reply from the old generation arrive against a future
///    issued in the new one.
/// 3. **Re-bootstrap with the same identity** — a rebind through the
///    existing address-independent binding, not a new path.
/// 4. **Announce leadership**, which is also what makes surviving
///    followers re-declare their subscriptions, and replay whatever
///    arrived while step three was in flight.
/// 5. **Restore** the subscriptions and re-publish the announcement.
///    Streams are deliberately *not* resurrected: a stream is
///    session-scoped, and pretending otherwise would hide a real
///    interruption.
///
/// # Three awaits, and what may have happened across them
///
/// Steps one, three and five all suspend, and a tab can be closed
/// while any of them is in flight. The old shape checked `closed`
/// once, before the acquisition, and then installed the server and
/// the lock unconditionally — so a session closed during the connect
/// ended up holding the origin's lock while refusing every operation,
/// and no other tab could take over for the lifetime of the page. So
/// intent is re-read after **each** await, and the lock and the
/// server are published together or not at all.
///
/// That is a refusal to publish, and it is not the same thing as
/// cancellation: the awaits above still run to completion. The
/// cancellation is [`bootstrap_leadership`]'s, which owns this future
/// and the lock it was granted.
async fn take_leadership(shared: &Rc<Shared>, previous: Option<u64>) -> Result<()> {
    let started = now_ms();
    if shared.state.borrow().closed {
        return Ok(());
    }
    let generation = match shared.vault.next_generation().await {
        Ok(generation) => generation,
        Err(error) => {
            // R13 made a transaction that could not commit an honest
            // failure; this is the recovery that honesty needs. The
            // old shape propagated straight out of here — above the
            // retry path below — so the sole surviving tab kept the
            // lock with no node, no client and no acquisition queued,
            // stranded by the very abort that was correctly reported.
            fall_back_to_follower(shared, previous, 0, &error)?;
            return Err(error);
        }
    };
    if shared.state.borrow().closed {
        // The generation is spent, which costs nothing — it is a
        // monotonic counter, not a lease on a resource. Returning
        // without claiming the lock is the point: it is still the
        // bootstrap's, and the bootstrap is about to end and let go.
        return Ok(());
    }

    if let Some(previous) = previous {
        if let Some(client) = shared.client.borrow_mut().as_mut() {
            client.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                generation: previous,
            })));
        }
    }
    *shared.client.borrow_mut() = None;

    let connect_opts = shared.state.borrow().connect_opts.clone();
    let sink = event_sink(shared);
    let factory = shared.factory.clone();
    let lease = GenerationLease::new(generation);
    let backend = match factory(connect_opts, sink, lease.clone()).await {
        Ok(backend) => backend,
        Err(error) => {
            fall_back_to_follower(shared, previous, generation, &error)?;
            return Err(error);
        }
    };

    let mut server = ProxyServer::with_lease(backend, shared.transport.clone(), lease.clone());
    if shared.state.borrow().closed {
        // Checked at publish time, not before the connect. The node
        // exists and nobody asked for it any more, so it is retired
        // through the same path a stand-down uses and the lock is
        // released by this frame.
        server.retire(None);
        drop(server);
        release_bootstrap_lock(shared);
        return Ok(());
    }

    *shared.lease.borrow_mut() = Some(lease.clone());
    *shared.server.borrow_mut() = Some(server);
    {
        let mut state = shared.state.borrow_mut();
        state.role = Role::Leader;
        state.generation = generation;
        state.lock = claim_bootstrap_lock(shared);
        state.subscribed.clear();
        state.announced.clear();
    }
    if let Some(server) = shared.server.borrow_mut().as_mut() {
        server.announce_leadership();
    }
    // Replayed *after* the Leadership broadcast, because that is the
    // message a queued `Attach` is waiting to be answered by: the
    // follower learns the generation, and its declaration is already
    // in the registry reconciliation is about to read.
    replay_queued(shared);
    emit(
        shared,
        &format!("{{\"type\":\"leader_changed\",\"generation\":\"{generation}\"}}"),
    );

    // The storage half of the fence, given the outbound caller D2
    // always claimed it had.
    let guarding = Rc::downgrade(shared);
    let guarded = lease.clone();
    spawn_local(async move { guard_lease(guarding, guarded).await });

    reconcile(shared).await;

    if previous.is_some() {
        shared.state.borrow_mut().interruption_ms = Some(now_ms() - started);
    }
    Ok(())
}

/// Become a functioning follower: a client with this tab's current
/// declarations, attached.
///
/// One helper rather than two copies, because the second copy is the
/// one that gets forgotten: this runs on the ordinary follower path
/// *and* on the path where a promotion's re-bootstrap failed, and the
/// declarations it carries have to be the same both times.
fn attach_as_follower(shared: &Rc<Shared>) -> Result<()> {
    let seed = correlation_seed()?;
    let follower = follower_id()?;
    let (declared, capabilities) = {
        let state = shared.state.borrow();
        (
            state.declared.iter().cloned().collect::<Vec<String>>(),
            state.capabilities.clone(),
        )
    };
    let mut client = ProxyClient::new(
        shared.transport.clone(),
        follower,
        declared,
        capabilities,
        seed,
    );
    client.attach();
    *shared.client.borrow_mut() = Some(client);
    Ok(())
}

/// Hand the freshly installed server every message that arrived while
/// this tab had no server to give them to.
fn replay_queued(shared: &Rc<Shared>) {
    let queued = core::mem::take(&mut *shared.queued.borrow_mut());
    for text in queued {
        let served = {
            let mut server = shared.server.borrow_mut();
            server.as_mut().map(|server| server.on_message(&text))
        };
        if let Some(Err(error)) = served {
            report(&format!("replaying a queued proxy message: {error}"));
        }
    }
}

/// Revalidate a leader's generation against the store, and stand down
/// if it has moved.
///
/// This is the outbound caller `IdentityVault::fence` never had, and
/// the case it exists for is the one no in-memory check can see: a tab
/// frozen while holding its lock. Its lease still reads live, its
/// `ProxyServer` has seen no higher generation — it saw nothing at all
/// — and the only thing that moved is the record in IndexedDB, written
/// by whichever tab acquired the lock in the meantime. So the leader
/// asks the store, on a timer.
///
/// A storage *failure* is not a takeover and does not stand anything
/// down: a database that cannot be read says nothing about who holds
/// the lock, and treating it as a loss would be a way to evict a
/// healthy leader.
async fn guard_lease(shared: Weak<Shared>, lease: GenerationLease) {
    loop {
        let _ = gloo_timer_sleep(LEASE_REVALIDATION_MS).await;
        if !lease.is_live() {
            return;
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if shared.state.borrow().closed {
            return;
        }
        match shared.vault.fence(lease.generation()).await {
            Ok(()) => {}
            Err(LeafError::NotLeader { current, .. }) => {
                if !lease.is_live() {
                    return;
                }
                let successor = current.unwrap_or_else(|| lease.generation().saturating_add(1));
                report(&format!(
                    "the store records generation {successor}; this tab holds {}",
                    lease.generation()
                ));
                stand_down(&shared, successor);
                return;
            }
            Err(error) => report(&format!("revalidating the leader generation: {error}")),
        }
    }
}

/// Subscribe everything the leader owes — this tab's own channels plus
/// the union of its followers' declared ones — re-publish the
/// announcement the same way, and tell the followers what came back.
///
/// Reconciliation rather than a one-shot restore list, because a
/// follower can attach at any moment, including after the new leader
/// has already re-subscribed. Running it on every attach converges;
/// a single pass at promotion time would miss every follower that had
/// not re-declared yet.
async fn reconcile(shared: &Rc<Shared>) {
    let wanted: Vec<String> = {
        let server = shared.server.borrow();
        let Some(server) = server.as_ref() else {
            return;
        };
        let state = shared.state.borrow();
        let mut wanted: BTreeSet<String> = state.declared.clone();
        wanted.extend(server.restoration());
        wanted
            .into_iter()
            .filter(|channel| !state.subscribed.contains(channel))
            .collect()
    };

    for channel in wanted {
        let lifecycle = Lifecycle {
            shared: shared.clone(),
        };
        match lifecycle
            .request(LeaderRequest::Subscribe {
                channel: channel.clone(),
            })
            .await
        {
            Ok(_) => {
                shared.state.borrow_mut().subscribed.insert(channel.clone());
                if let Some(server) = shared.server.borrow_mut().as_mut() {
                    server.announce_restored(&channel);
                }
                emit(
                    shared,
                    &format!(
                        "{{\"type\":\"subscription_restored\",\"channel\":{}}}",
                        json_string(&channel)
                    ),
                );
            }
            Err(failure) => report(&format!(
                "restoring the {channel} subscription: {}",
                failure.message()
            )),
        }
    }

    if let Err(failure) = announce_union(shared).await {
        report(&format!(
            "re-publishing the announcement: {}",
            failure.message()
        ));
    }
}

/// Publish the announcement this origin currently wants, and report
/// whether it worked.
///
/// The union of this tab's intent and every attached follower's,
/// which is the only definition that survives a handoff: D2 promises
/// the announcement comes back, and the announcement is not the
/// property of whichever tab happens to hold the lock. Idempotent —
/// an attach that declares nothing new costs no network operation.
///
/// # An empty union is a withdrawal, not a no-op
///
/// The early return used to cover an empty `wanted` as well, so the
/// last capability-declaring follower detaching left its tag in the
/// published document indefinitely: reconciliation ran, computed
/// nothing, and returned. A tag the origin no longer offers is worse
/// than no tag — a querying peer picks this node and the call is
/// refused — so an empty union is published like any other change.
/// The equality check still covers the ordinary case of *nothing to
/// announce and nothing announced*, which costs no operation.
async fn announce_union(shared: &Rc<Shared>) -> ProxyOutcome {
    // The parked follower declarations are taken with the same
    // snapshot the union is computed from, deliberately: a
    // declaration recorded after this line is not in `wanted`, and
    // settling it here would report a publication that does not
    // carry it. Its own reconciliation is already scheduled.
    let (wanted, declarations) = {
        let mut server = shared.server.borrow_mut();
        let Some(server) = server.as_mut() else {
            // Not the leader: there is no union to publish, and the
            // follower path does not come through here.
            return Err(ProxyFailure::Typed(LeafError::NotLeader {
                presented: shared.state.borrow().generation,
                current: None,
            }));
        };
        let state = shared.state.borrow();
        let mut union: BTreeSet<String> = state.capabilities.iter().cloned().collect();
        union.extend(server.capability_restoration());
        let declarations = server.take_pending_announcements();
        (union.into_iter().collect::<Vec<String>>(), declarations)
    };
    if shared.state.borrow().announced == wanted {
        settle_declarations(declarations, &Ok(ProxyValue::Bytes(Bytes::new())));
        return Ok(ProxyValue::Bytes(Bytes::new()));
    }
    let lifecycle = Lifecycle {
        shared: shared.clone(),
    };
    let outcome = lifecycle
        .request(LeaderRequest::Announce {
            capabilities: wanted.clone(),
        })
        .await;
    if outcome.is_ok() {
        shared.state.borrow_mut().announced = wanted;
    }
    settle_declarations(declarations, &outcome);
    outcome
}

/// Whether anyone still wants `channel`: this tab's own declaration,
/// or any attached follower's.
///
/// The two sets together are the whole question, and the reason the
/// decision cannot live in [`crate::leader::ProxyServer`], which holds
/// only the second one.
fn still_wanted(shared: &Rc<Shared>, channel: &str) -> bool {
    let own_wants = shared.state.borrow().declared.clone();
    let followers = shared
        .server
        .borrow()
        .as_ref()
        .map(ProxyServer::restoration)
        .unwrap_or_default();
    crate::leader::membership_still_wanted(&own_wants, &followers, channel)
}

/// Answer the releases the server parked, each against the union.
///
/// A release whose channel somebody else still declares succeeds
/// without a frame; the last one gives the membership up. Every parked
/// replier is settled: dropping one answers its caller typed, which is
/// the correct failure but not the answer it is owed.
async fn serve_releases(shared: &Rc<Shared>) {
    let parked = {
        let mut server = shared.server.borrow_mut();
        match server.as_mut() {
            Some(server) => server.take_pending_releases(),
            None => return,
        }
    };
    for (channel, replier) in parked {
        if still_wanted(shared, &channel) {
            replier.bytes(Bytes::new());
            continue;
        }
        let lifecycle = Lifecycle {
            shared: shared.clone(),
        };
        let outcome = lifecycle
            .request(LeaderRequest::Unsubscribe {
                channel: channel.clone(),
            })
            .await;
        if outcome.is_ok() {
            shared.state.borrow_mut().subscribed.remove(&channel);
        }
        settle_declarations(vec![replier], &outcome);
    }
}

/// Answer the followers whose `announce()` was parked on this
/// publication, with the outcome it actually had.
///
/// A follower's declaration is recorded by the server and published
/// here as part of the union, so its caller's promise settles on the
/// union publication rather than on a document that was never
/// published. Held repliers that are never settled are not silently
/// lost either: dropping one answers it typed.
fn settle_declarations(declarations: Vec<Replier>, outcome: &ProxyOutcome) {
    for replier in declarations {
        match outcome {
            Ok(_) => replier.bytes(Bytes::new()),
            Err(failure) => replier.fail(failure.clone()),
        }
    }
}

/// Install the one `BroadcastChannel` listener.
fn install_channel_listener(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    let listener = Closure::wrap(Box::new(move |event: MessageEvent| {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let Some(text) = event.data().as_string() else {
            return;
        };
        dispatch(&shared, &text);
    }) as Box<dyn FnMut(MessageEvent)>);
    shared
        .channel
        .set_onmessage(Some(listener.as_ref().unchecked_ref()));
    // The channel outlives every borrow of it; the closure must too.
    listener.forget();
}

/// Route one inbound proxy message.
fn dispatch(shared: &Rc<Shared>, text: &str) {
    if shared.state.borrow().closed {
        return;
    }

    // Leader: serve it. The borrow ends before anything async runs,
    // because `perform` only ever spawns.
    let served = {
        let mut server = shared.server.borrow_mut();
        server.as_mut().map(|server| server.on_message(text))
    };
    if let Some(outcome) = served {
        if let Err(error) = outcome {
            // A refusal, already answered to the follower. Visible,
            // not fatal: a stale follower is exactly what the fence
            // is for.
            report(&format!("proxy refusal: {error}"));
        }
        let superseded = shared
            .server
            .borrow()
            .as_ref()
            .and_then(ProxyServer::superseded);
        if let Some(generation) = superseded {
            stand_down(shared, generation);
            return;
        }
        // Releases before reconciliation, and in their own task:
        // `serve_releases` may perform a wire unsubscribe, and
        // reconciliation subscribes what is still wanted. Running the
        // release first means the two cannot argue about a channel
        // this message just gave up.
        let releasing = Rc::clone(shared);
        let reconciling = Rc::clone(shared);
        spawn_local(async move {
            serve_releases(&releasing).await;
            reconcile(&reconciling).await;
        });
        flush_events(shared);
        return;
    }

    // Follower: gate it.
    let event = {
        let mut client = shared.client.borrow_mut();
        match client.as_mut() {
            Some(client) => client.on_message(text),
            None => {
                // Neither half exists: this tab holds the lock and is
                // still awaiting its node. Dropping the message here
                // is what lost a follower's `Attach` — the follower is
                // taught the generation by the Leadership broadcast
                // but re-declares only on a *subsequent* one, so its
                // subscriptions never reached the node at all.
                drop(client);
                queue_inbound(shared, text);
                return;
            }
        }
    };
    match event {
        Ok(Some(event)) => handle_follower_event(shared, event),
        Ok(None) => {}
        Err(error) => report(&format!("proxy message refused: {error}")),
    }
    flush_events(shared);
}

/// Act on what the follower half learned.
fn handle_follower_event(shared: &Rc<Shared>, event: FollowerEvent) {
    match event {
        FollowerEvent::Leader { generation, .. } => {
            shared.state.borrow_mut().generation = generation;
            emit(
                shared,
                &format!("{{\"type\":\"leader_changed\",\"generation\":\"{generation}\"}}"),
            );
        }
        FollowerEvent::Restored { channel } => {
            shared.state.borrow_mut().restored.push(channel.clone());
            emit(
                shared,
                &format!(
                    "{{\"type\":\"subscription_restored\",\"channel\":{}}}",
                    json_string(&channel)
                ),
            );
        }
        FollowerEvent::Event { json } => emit(shared, &json),
        FollowerEvent::Fenced { presented, current } => {
            // The fence fired. Surfaced rather than swallowed: a page
            // debugging a resumed tab needs to see the refusal.
            emit(
                shared,
                &format!(
                    "{{\"type\":\"generation_fenced\",\"presented\":\"{presented}\",\
                      \"current\":\"{current}\"}}"
                ),
            );
        }
        FollowerEvent::LeaderLost { lost, failed } => emit(
            shared,
            &format!(
                "{{\"type\":\"leader_lost\",\"generation\":\"{lost}\",\
                  \"failed\":\"{failed}\"}}"
            ),
        ),
    }
}

/// Hold one inbound message until this tab has a server for it.
fn queue_inbound(shared: &Rc<Shared>, text: &str) {
    let mut queued = shared.queued.borrow_mut();
    if queued.len() >= MAX_QUEUED_INBOUND {
        drop(queued);
        let dropped = shared.overflowed.get().saturating_add(1);
        shared.overflowed.set(dropped);
        report(&format!(
            "the bootstrap queue is full at {MAX_QUEUED_INBOUND}; {dropped} proxy \
             message(s) dropped"
        ));
        return;
    }
    queued.push(text.to_string());
}

/// A leader that has seen a higher generation is not the leader.
///
/// Same order as an orderly close, and for the same reason: the fence
/// goes up, the node is retired, the followers are told, and only then
/// is the lock let go. It does *not* try to reclaim the lock — the
/// successor holds it, and a tab that fought for it would be the
/// eviction §8 exists to prevent.
fn stand_down(shared: &Rc<Shared>, successor: u64) {
    let ours = shared.state.borrow().generation;
    report(&format!(
        "generation {ours} was superseded by {successor}; standing down"
    ));
    // Out of the cell first: retirement runs the node's close path,
    // which drains events through a sink that reaches for this borrow.
    let mut server = shared.server.borrow_mut().take();
    if let Some(server) = server.as_mut() {
        let retirement = server.retire(Some(successor));
        report(&format!(
            "generation {} was fenced; {} pending call(s) failed",
            retirement.generation, retirement.pending_failed
        ));
    }
    drop(server);
    *shared.lease.borrow_mut() = None;
    shared.queued.borrow_mut().clear();
    {
        let mut state = shared.state.borrow_mut();
        state.role = Role::Follower;
        state.subscribed.clear();
        state.announced.clear();
        // Last, as always.
        state.lock = None;
    }
    emit(
        shared,
        &format!(
            "{{\"type\":\"not_leader\",\"presented\":\"{ours}\",\"current\":\"{successor}\"}}"
        ),
    );
}

/// The sink a leader's node hands every event to.
fn event_sink(shared: &Rc<Shared>) -> EventSink {
    let weak = Rc::downgrade(shared);
    Rc::new(move |json: &str| {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        // **Queued, never broadcast from here.** This sink is called
        // by the node, and the node is pumped synchronously by
        // backend arms that run under `shared.server.borrow_mut()` —
        // `NodeBackend::StreamSend` invokes a real
        // `wasm::LeafStream::send`, which pumps, drives reliability
        // and dispatches whatever that produced before it returns. A
        // terminal event for *another* stream therefore arrives
        // inside the outer server borrow, and taking a second one
        // here was a panic. Discarding the event on a failed borrow
        // would trade the panic for a follower that never hears its
        // stream died, so the event is held and broadcast by
        // [`flush_broadcasts`] once the borrow is gone.
        shared.broadcasts.borrow_mut().push(json.to_string());
        emit(&shared, json);
    })
}

/// Broadcast every event owed to the followers, if the server is
/// free.
///
/// Leaves the queue untouched when it is not: the caller that holds
/// the server borrow is a synchronous frame, and [`emit`] has already
/// scheduled the drain that runs after it returns. Nothing is
/// dropped.
fn flush_broadcasts(shared: &Rc<Shared>) {
    if shared.broadcasts.borrow().is_empty() {
        return;
    }
    let Ok(mut server) = shared.server.try_borrow_mut() else {
        return;
    };
    let Some(server) = server.as_mut() else {
        // No server: this tab is not the leader any more, and a
        // stale leader's broadcast is exactly what the fence exists
        // to stop.
        shared.broadcasts.borrow_mut().clear();
        return;
    };
    for json in core::mem::take(&mut *shared.broadcasts.borrow_mut()) {
        server.broadcast_event(&json);
    }
}

/// Queue one event and schedule the drain.
///
/// Queued, not delivered: this runs inside a JS callback that may
/// already hold a borrow a listener would re-enter.
fn emit(shared: &Rc<Shared>, json: &str) {
    shared.events.borrow_mut().push(json.to_string());
    let shared = shared.clone();
    spawn_local(async move { flush_events(&shared) });
}

/// Deliver every queued event, holding no borrow while a listener runs.
///
/// The followers first: a node event is one fact, and the tab that
/// runs the node must not see it arbitrarily earlier than the tabs it
/// serves. This is also the drain [`event_sink`] depends on — the
/// broadcast it could not take the server for is taken here, once
/// every synchronous backend frame has returned.
fn flush_events(shared: &Rc<Shared>) {
    flush_broadcasts(shared);
    let queued = core::mem::take(&mut *shared.events.borrow_mut());
    if queued.is_empty() {
        return;
    }
    let listeners = shared.state.borrow().listeners.clone();
    for json in queued {
        let value = JsValue::from_str(&json);
        for listener in &listeners {
            let _ = listener.call1(&JsValue::NULL, &value);
        }
    }
}

// ─────────────────────────── the node's backend ────────────────────────

/// The operations one generation admitted, and the handle that drops
/// them.
///
/// Public because it is the seam a [`LeaderBackend`] implementation
/// needs in order to honour the fence at all: an implementation that
/// spawns its own futures with `spawn_local` has no way to let go of
/// them, and `shutdown` would be a promise it could not keep.
///
/// `spawn_local` gives no way to cancel a future from outside: the
/// executor owns it, and the only thing that ends it is the future
/// completing. So each spawned operation carries a
/// `oneshot::Receiver` whose sender lives here, and dropping the
/// sender *cancels* the receiver — which wakes the task, which then
/// finds its lease revoked and returns without running the rest of
/// the operation. The replier it was carrying is dropped with it and
/// settles its caller typed.
///
/// Without the wake this would not work: an operation parked on a
/// barrier that never fires is never polled again, so a lazily
/// checked fence would leave its caller's promise pending forever —
/// which is the failure D2 says is worse than a refusal.
#[derive(Default)]
pub struct OpRegistry {
    next: Cell<u64>,
    live: RefCell<HashMap<u64, oneshot::Sender<()>>>,
}

impl OpRegistry {
    /// Admit one operation and hand back its cancellation receiver.
    pub fn admit(&self) -> (u64, oneshot::Receiver<()>) {
        let id = self.next.get().wrapping_add(1);
        self.next.set(id);
        let (tx, rx) = oneshot::channel();
        self.live.borrow_mut().insert(id, tx);
        (id, rx)
    }

    /// Forget a finished operation, so a long-lived leader's registry
    /// does not grow one entry per call it ever made.
    fn finished(&self, id: u64) {
        self.live.borrow_mut().remove(&id);
    }

    /// Cancel every live operation. Returns how many.
    pub fn cancel_all(&self) -> usize {
        // Taken, then dropped outside the borrow: dropping a sender
        // wakes its task, and a woken task's first act is to drop its
        // `Fenced`, which reaches for this same cell.
        let live = core::mem::take(&mut *self.live.borrow_mut());
        let count = live.len();
        drop(live);
        count
    }
}

/// One spawned backend operation, bound to the generation that
/// admitted it.
struct Fenced {
    op: Pin<Box<dyn Future<Output = ()>>>,
    cancelled: oneshot::Receiver<()>,
    lease: GenerationLease,
    registry: Rc<OpRegistry>,
    id: u64,
}

impl Future for Fenced {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if !self.lease.is_live() {
            return Poll::Ready(());
        }
        if Pin::new(&mut self.cancelled).poll(cx).is_ready() {
            return Poll::Ready(());
        }
        self.op.as_mut().poll(cx)
    }
}

impl Drop for Fenced {
    fn drop(&mut self) {
        self.registry.finished(self.id);
    }
}

/// Spawn `op` under `lease`, cancellable through `registry`.
///
/// Every operation the leader's node performs goes through here, and
/// that is the point: an operation admitted under generation *n* must
/// not produce an effect after *n* has been fenced, and the only way
/// to guarantee that for work that is already in flight is for the
/// work itself to be droppable.
pub fn spawn_fenced(
    lease: &GenerationLease,
    registry: &Rc<OpRegistry>,
    op: impl Future<Output = ()> + 'static,
) {
    let (id, cancelled) = registry.admit();
    spawn_local(Fenced {
        op: Box::pin(op),
        cancelled,
        lease: lease.clone(),
        registry: registry.clone(),
        id,
    });
}

/// The real backend: the leaf node this tab runs.
struct NodeBackend {
    node: Rc<crate::wasm::LeafNode>,
    node_id: u64,
    /// The streams this backend owns, addressed by handle.
    ///
    /// [`StreamOwnership`] is the one place that addressing lives, and
    /// the browser witnesses' backend uses the same type — so a change
    /// to how an open is addressed is visible to them.
    streams: Rc<StreamOwnership<crate::wasm::LeafStream>>,
    /// The fence this backend's operations are admitted under — the
    /// same lease the server stamps its replies with.
    lease: GenerationLease,
    ops: Rc<OpRegistry>,
}

impl LeaderBackend for NodeBackend {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn shutdown(&mut self, generation: u64) -> usize {
        // The operations first: each one is holding the node, and a
        // cancelled one is what stops it reaching the transport after
        // the lock has moved.
        let cancelled = self.ops.cancel_all();
        // A stream is session-scoped. D2 is explicit that it is not
        // resurrected, so the handles go with the node rather than
        // becoming a table a successor could be addressed through.
        drop(self.streams.drain());
        let failed = self.node.retire(generation);
        if cancelled > 0 {
            report(&format!(
                "generation {generation} cancelled {cancelled} in-flight operation(s)"
            ));
        }
        failed
    }

    fn perform(&mut self, request: LeaderRequest, reply: Replier) {
        let node = self.node.clone();
        // Every arm that suspends is spawned under the fence: an
        // operation admitted by this generation must be droppable when
        // the generation is, and the synchronous arms below cannot
        // outlive the call anyway.
        let lease = self.lease.clone();
        let ops = self.ops.clone();
        match request {
            LeaderRequest::Call {
                service,
                payload,
                timeout_ms,
            } => spawn_fenced(&lease, &ops, async move {
                match node
                    .call(
                        service,
                        Uint8Array::from(&payload[..]),
                        timeout_ms.map(f64::from),
                    )
                    .await
                {
                    Ok(body) => reply.bytes(Bytes::from(body.to_vec())),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Subscribe { channel } => spawn_fenced(&lease, &ops, async move {
                match node.subscribe(channel).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Unsubscribe { channel } => spawn_fenced(&lease, &ops, async move {
                // Reached only when the server decided this was the
                // LAST consumer; a release with a sibling tab still
                // reading is answered there and never gets here.
                match node.unsubscribe(channel).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Publish { channel, payload } => spawn_fenced(&lease, &ops, async move {
                match node.publish(channel, Uint8Array::from(&payload[..])).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Announce { capabilities } => spawn_fenced(&lease, &ops, async move {
                match node.announce(capabilities).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Query { capability } => spawn_fenced(&lease, &ops, async move {
                match node.query(capability).await {
                    Ok(json) => reply.text(json),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Counters => reply.text(node.counters_json()),
            LeaderRequest::IsEnrolled => reply.flag(node.is_enrolled()),
            LeaderRequest::Enroll => spawn_fenced(&lease, &ops, async move {
                match node.enroll().await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Signal {
                peer,
                dialog,
                kind,
                payload,
            } => spawn_fenced(&lease, &ops, async move {
                match node
                    .signal(
                        format!("{peer:016x}"),
                        #[allow(clippy::cast_precision_loss)]
                        (dialog as f64),
                        kind,
                        Uint8Array::from(&payload[..]),
                    )
                    .await
                {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::PeerOffer { peer } => spawn_fenced(&lease, &ops, async move {
                match node.peer_offer(format!("{peer:016x}")).await {
                    Ok(json) => reply.text(json),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::PeerAcceptOffer { peer } => spawn_fenced(&lease, &ops, async move {
                match node.peer_accept_offer(format!("{peer:016x}")).await {
                    Ok(json) => reply.text(json),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::PeerCandidate { peer, dialog } => {
                spawn_fenced(&lease, &ops, async move {
                    // The dialog-NAMED form. `peer_candidate` would
                    // resolve whichever attempt happens to be live
                    // when this resumes, which for a request that
                    // crossed a channel is how a stale poll drives
                    // its own replacement.
                    match node
                        .peer_candidate_in(format!("{peer:016x}"), format!("{dialog:016x}"))
                        .await
                    {
                        Ok(json) => reply.text(json),
                        Err(error) => reply.fail(reported(error)),
                    }
                })
            }
            LeaderRequest::PeerHandshake { peer, dialog } => {
                spawn_fenced(&lease, &ops, async move {
                    match node
                        .peer_handshake_in(format!("{peer:016x}"), format!("{dialog:016x}"))
                        .await
                    {
                        Ok(dialog_hex) => reply.text(dialog_hex),
                        Err(error) => reply.fail(reported(error)),
                    }
                })
            }
            // Every stream request goes through the shared dispatch:
            // it owns which stream an operation reaches, what the open
            // reply says, and how a closed handle is refused. This
            // backend supplies only the three type-specific actions
            // (`StreamBackend`), so there is no reply construction here
            // to answer with the requested peer instead of the
            // resolved one.
            other @ (LeaderRequest::StreamOpen { .. }
            | LeaderRequest::StreamSend { .. }
            | LeaderRequest::StreamClose { .. }) => {
                // The match above admits only the three stream
                // variants, so the hand-back is unreachable here.
                // Asserted rather than ignored: a silent `let _`
                // would drop a live replier if that ever changed.
                // Evaluated unconditionally: a `debug_assert!`
                // around the CALL would remove the dispatch from a
                // release build entirely.
                let unhandled = answer_stream_request(&self.streams.clone(), self, other, reply);
                debug_assert!(
                    unhandled.is_none(),
                    "the stream arms prefilter, so nothing is handed back"
                );
                drop(unhandled);
            }
        }
    }
}

impl StreamBackend for NodeBackend {
    type Stream = crate::wasm::LeafStream;

    /// Build the direct surface's options and open. The peer is
    /// converted to the 16-hex spelling that surface accepts, because
    /// a decimal id addresses a different node rather than failing.
    fn open(
        &self,
        label: &str,
        reliability: Reliability,
        stream_id: Option<u64>,
        channel_hash: Option<u16>,
        peer: Option<u64>,
    ) -> Result<Self::Stream, ProxyFailure> {
        let opts = Object::new();
        let spelling = match reliability {
            Reliability::Reliable => "reliable",
            Reliability::FireAndForget => "fireAndForget",
        };
        if set(&opts, "reliability", &JsValue::from_str(spelling))
            .and_then(|()| set(&opts, "label", &JsValue::from_str(label)))
            .is_err()
        {
            return Err(ProxyFailure::Typed(LeafError::Session(
                "could not build the stream options".into(),
            )));
        }
        if let Some(id) = stream_id {
            let _ = set(&opts, "streamId", &JsValue::from_str(&id.to_string()));
        }
        if let Some(hash) = channel_hash {
            let _ = set(&opts, "channelHash", &JsValue::from_f64(f64::from(hash)));
        }
        if let Some(peer) = peer {
            let _ = set(&opts, "peer", &JsValue::from_str(&format!("{peer:016x}")));
        }
        self.node.open_stream(opts.into()).map_err(reported)
    }

    fn send(&self, stream: &Self::Stream, payload: &[u8]) -> Result<(), ProxyFailure> {
        stream.send(Uint8Array::from(payload)).map_err(reported)
    }

    fn close(&self, stream: Self::Stream) {
        stream.close();
    }
}

/// The production factory: connect a real node and route its events.
fn node_factory() -> BackendFactory {
    Rc::new(|opts: JsValue, sink: EventSink, lease: GenerationLease| {
        Box::pin(async move {
            let node =
                crate::wasm::LeafNode::connect(opts)
                    .await
                    .map_err(|error| match reported(error) {
                        ProxyFailure::Reported(text) => LeafError::Session(text),
                        ProxyFailure::Typed(error) => error,
                    })?;
            let node_id = u64::from_str_radix(&node.node_id_hex(), 16)
                .map_err(|_| LeafError::Identity("the node id was not hex".into()))?;
            let forward = Closure::wrap(Box::new(move |json: JsValue| {
                if let Some(text) = json.as_string() {
                    sink(&text);
                }
            }) as Box<dyn FnMut(JsValue)>);
            node.on_event(forward.as_ref().unchecked_ref::<Function>().clone());
            forward.forget();
            let backend: Box<dyn LeaderBackend> = Box::new(NodeBackend {
                node: Rc::new(node),
                node_id,
                streams: Rc::new(StreamOwnership::default()),
                lease,
                ops: Rc::new(OpRegistry::default()),
            });
            Ok(backend)
        }) as BackendFuture
    })
}

// ──────────────────────────── the JS surface ───────────────────────────

/// A Net node as a tab sees it, whether or not this tab runs it.
///
/// ```text
/// class MeshSession {
///   static open(opts): Promise<MeshSession>
///   role(): "leader" | "follower"
///   generation(): string
///   node_id_hex(): string | undefined
///   fingerprint(): string
///   scope(): string
///   interruption_ms(): number | undefined
///   call(service, payload, timeout_ms?): Promise<Uint8Array>
///   subscribe(channel): Promise<void>
///   publish(channel, payload): Promise<void>
///   announce(capabilities): Promise<void>
///   query(capability): Promise<string>
///   counters_json(): Promise<string>
///   signal(peer_hex, dialog, kind, payload): Promise<void>
///   open_stream(opts): Promise<ProxyStream>
///   on_event(cb): void
///   close(): void
/// }
/// ```
///
/// One honest difference from `crate::wasm::LeafNode`: `open_stream`
/// returns a promise, because on a follower the stream is opened by
/// another tab. Everything else has the same shape, and every
/// operation carries the generation.
#[wasm_bindgen]
pub struct MeshSession {
    lifecycle: Lifecycle,
}

#[wasm_bindgen]
impl MeshSession {
    /// Resolve the identity, contend for the origin's lock, and return
    /// this tab's session.
    ///
    /// `opts` is everything `LeafNode.connect` takes — `credentialB64`,
    /// `origin`, `bootstrapUrl?`, `iceServers?`, `entitySecretHex?`,
    /// `noiseSecretHex?` — plus `capabilities?`, `subscriptions?`,
    /// `dbName?` and `lockScope?`.
    ///
    /// With no `entitySecretHex` the identity comes from IndexedDB
    /// under a non-extractable AES-GCM key, generated there on first
    /// run. With one, the host holds custody and nothing is stored.
    /// Both paths produce the same value and take the same route in.
    pub async fn open(opts: JsValue) -> Result<MeshSession, JsError> {
        let lifecycle = Lifecycle::open(opts, node_factory()).await.map_err(js)?;
        Ok(Self { lifecycle })
    }

    /// `"leader"` or `"follower"`.
    pub fn role(&self) -> String {
        self.lifecycle.role().as_str().to_string()
    }

    /// The generation in force, as a decimal string.
    pub fn generation(&self) -> String {
        self.lifecycle.generation().to_string()
    }

    /// The node's id, 16 lowercase hex digits, once one is known.
    pub fn node_id_hex(&self) -> Option<String> {
        self.lifecycle.node_id().map(|id| format!("{id:016x}"))
    }

    /// The identity fingerprint the lock is named after.
    pub fn fingerprint(&self) -> String {
        self.lifecycle.fingerprint()
    }

    /// The lock and channel name.
    pub fn scope(&self) -> String {
        self.lifecycle.scope()
    }

    /// The measured interruption, in milliseconds, for a tab that was
    /// promoted. `undefined` on the first leader.
    pub fn interruption_ms(&self) -> Option<f64> {
        self.lifecycle.interruption_ms()
    }

    /// Call an nRPC service.
    pub async fn call(
        &self,
        service: String,
        payload: Uint8Array,
        timeout_ms: Option<f64>,
    ) -> Result<Uint8Array, JsError> {
        let outcome = self
            .lifecycle
            .request(LeaderRequest::Call {
                service,
                payload: Bytes::from(payload.to_vec()),
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                timeout_ms: timeout_ms.map(|ms| ms.max(0.0) as u32),
            })
            .await;
        match outcome? {
            ProxyValue::Bytes(body) => Ok(Uint8Array::from(&body[..])),
            other => Err(JsError::new(&format!(
                "a call answered with {other:?}, which is not a reply body"
            ))),
        }
    }

    /// Subscribe to a channel.
    ///
    /// Recorded as this tab's own declaration, which is what a
    /// successor restores and what an attach carries to a new leader.
    pub async fn subscribe(&self, channel: String) -> Result<(), JsError> {
        self.lifecycle.subscribe(channel).await?;
        Ok(())
    }

    /// Release this tab's claim on a channel.
    ///
    /// The membership itself is given up only when no other tab still
    /// declares the channel: the origin runs one node, and one tab
    /// closing a subscription is not every tab closing it.
    pub async fn unsubscribe(&self, channel: String) -> Result<(), JsError> {
        self.lifecycle.unsubscribe(channel).await?;
        Ok(())
    }

    /// Publish one payload to a channel.
    pub async fn publish(&self, channel: String, payload: Uint8Array) -> Result<(), JsError> {
        self.lifecycle
            .request(LeaderRequest::Publish {
                channel,
                payload: Bytes::from(payload.to_vec()),
            })
            .await?;
        Ok(())
    }

    /// Publish this node's capability announcement.
    ///
    /// Also records it as this tab's current intent, so a takeover
    /// re-publishes what the page last announced rather than what it
    /// opened with.
    pub async fn announce(&self, capabilities: Vec<String>) -> Result<(), JsError> {
        self.lifecycle.announce(capabilities).await?;
        Ok(())
    }

    /// Mint an offer and start an attempt with `peer_hex` (§9).
    ///
    /// The four peer methods below are the primitives a **shared**
    /// TypeScript driver calls. The drive loop and its classification
    /// live once, in the package, and are used by `BrowserNode` and
    /// by this session alike — a second implementation of that loop
    /// here would be two spellings of one contract.
    pub async fn peer_offer(&self, peer_hex: String) -> Result<String, JsError> {
        let peer = crate::wasm::parse_peer_id(&peer_hex)?;
        match self
            .lifecycle
            .request(LeaderRequest::PeerOffer { peer })
            .await?
        {
            ProxyValue::Text(json) => Ok(json),
            other => Err(JsError::new(&format!(
                "an offer answered with {other:?}, which is not a reading"
            ))),
        }
    }

    /// Answer the offer `peer_hex` filed.
    pub async fn peer_accept_offer(&self, peer_hex: String) -> Result<String, JsError> {
        let peer = crate::wasm::parse_peer_id(&peer_hex)?;
        match self
            .lifecycle
            .request(LeaderRequest::PeerAcceptOffer { peer })
            .await?
        {
            ProxyValue::Text(json) => Ok(json),
            other => Err(JsError::new(&format!(
                "an answer answered with {other:?}, which is not a reading"
            ))),
        }
    }

    /// Service one polling step of the attempt `dialog_hex`.
    ///
    /// The dialog is required, not optional. A proxied poll that
    /// named only the peer would be applied to whatever attempt is
    /// live when the leader gets to it, which is how a superseded
    /// request drives its replacement.
    pub async fn peer_candidate(
        &self,
        peer_hex: String,
        dialog_hex: String,
    ) -> Result<String, JsError> {
        let peer = crate::wasm::parse_peer_id(&peer_hex)?;
        let dialog = crate::wasm::parse_dialog_id(&dialog_hex)?;
        match self
            .lifecycle
            .request(LeaderRequest::PeerCandidate { peer, dialog })
            .await?
        {
            ProxyValue::Text(json) => Ok(json),
            other => Err(JsError::new(&format!(
                "a candidate step answered with {other:?}, which is not a reading"
            ))),
        }
    }

    /// Run the Noise handshake for the attempt `dialog_hex`.
    pub async fn peer_handshake(
        &self,
        peer_hex: String,
        dialog_hex: String,
    ) -> Result<String, JsError> {
        let peer = crate::wasm::parse_peer_id(&peer_hex)?;
        let dialog = crate::wasm::parse_dialog_id(&dialog_hex)?;
        match self
            .lifecycle
            .request(LeaderRequest::PeerHandshake { peer, dialog })
            .await?
        {
            ProxyValue::Text(dialog_hex) => Ok(dialog_hex),
            other => Err(JsError::new(&format!(
                "a handshake answered with {other:?}, which is not a dialog"
            ))),
        }
    }

    /// Find the nodes offering a capability.
    pub async fn query(&self, capability: String) -> Result<String, JsError> {
        match self
            .lifecycle
            .request(LeaderRequest::Query { capability })
            .await?
        {
            ProxyValue::Text(json) => Ok(json),
            other => Err(JsError::new(&format!(
                "a query answered with {other:?}, which is not a document"
            ))),
        }
    }

    /// Every leaf counter as JSON; `u64`s are decimal strings.
    pub async fn counters_json(&self) -> Result<String, JsError> {
        match self.lifecycle.request(LeaderRequest::Counters).await? {
            ProxyValue::Text(json) => Ok(json),
            other => Err(JsError::new(&format!(
                "counters answered with {other:?}, which is not a document"
            ))),
        }
    }

    /// Run the enrollment exchange if the node is not enrolled.
    ///
    /// Proxied, so a follower's `enroll()` works: one node per origin
    /// means one enrollment per origin, and the follower is asking
    /// the only session there is. Idempotent on the leader's side —
    /// an already-enrolled node answers immediately.
    pub async fn enroll(&self) -> Result<(), JsError> {
        self.lifecycle.request(LeaderRequest::Enroll).await?;
        Ok(())
    }

    /// Whether the node has completed enrollment.
    ///
    /// A promise, where [`crate::wasm::LeafNode::is_enrolled`] is
    /// synchronous: on a follower the answer lives in another tab.
    pub async fn is_enrolled(&self) -> Result<bool, JsError> {
        match self.lifecycle.request(LeaderRequest::IsEnrolled).await? {
            ProxyValue::Flag(flag) => Ok(flag),
            other => Err(JsError::new(&format!(
                "is_enrolled answered with {other:?}, which is not a flag"
            ))),
        }
    }

    /// Sign and send a session-independent signalling envelope.
    pub async fn signal(
        &self,
        peer_hex: String,
        dialog: f64,
        kind: String,
        payload: Uint8Array,
    ) -> Result<(), JsError> {
        // **One spelling, and the same reader as every other
        // page-facing surface.** This read bare hex of ANY length —
        // `"9"`, `"0x9"`, `"deadbeef"` all parsed — so a page on
        // `openSession()` could sign an envelope to a node id
        // spelling `connect()` refuses, i.e. the two surfaces
        // disagreed about what a node id IS. That is exactly the
        // drift the §9 refusal exists to prevent (PeerLoop's audit,
        // 2026-09-16). It also could not work: the leader re-encodes
        // canonical 16-hex on the way to
        // [`crate::wasm::LeafNode::signal`], which read DECIMAL.
        let peer = crate::wasm::parse_peer_id(&peer_hex)?;
        self.lifecycle
            .request(LeaderRequest::Signal {
                peer,
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                dialog: dialog.max(0.0) as u64,
                kind,
                payload: Bytes::from(payload.to_vec()),
            })
            .await?;
        Ok(())
    }

    /// Open an application stream, wherever the node is.
    pub async fn open_stream(&self, opts: JsValue) -> Result<ProxyStream, JsError> {
        self.lifecycle.open_stream(&opts).await
    }

    /// Register an event listener. One JSON string per event.
    ///
    /// Beyond [`crate::node::LeafEvent`]'s tags, a session emits
    /// `leader_changed`, `subscription_restored`, `leader_lost`,
    /// `generation_fenced` and `not_leader` — the lifecycle made
    /// observable, because a page that cannot see a leader change
    /// cannot explain one.
    pub fn on_event(&self, callback: Function) {
        self.lifecycle.on_event(callback);
    }

    /// Stand down and release the lock.
    pub fn close(&self) {
        self.lifecycle.close();
    }
}

/// One application stream, on whichever tab runs the node.
///
/// # Why it remembers its generation
///
/// A stream id names per-session state, and leadership moving means a
/// new node with a new and independently allocated stream table. A
/// handle that carried only the id would therefore address *whatever*
/// stream the successor happened to open with that id — so a retained
/// object's `send` could push a payload into a stranger's stream and
/// its `close` could tear that stream down. Carrying the opening
/// generation makes the handle refuse instead, and the refusal is
/// typed, because "the leader changed" is an answer a page can act on
/// and silence is not.
///
/// # Why it remembers a handle *and* an id
///
/// The same argument one level down. Two opens to two peers under one
/// label share a wire stream id, so the id does not name an open
/// either: addressing the backend by it let one handle's `send` reach
/// another peer's stream. The `handle` names the open; the
/// `stream_id` is what the application filters on; and
/// [`Self::peer_node_hex`] is what makes that filter a peer filter
/// rather than a peer-blind one.
#[wasm_bindgen]
pub struct ProxyStream {
    lifecycle: Lifecycle,
    /// The backend's owning handle for this open.
    handle: u64,
    stream_id: u64,
    /// The authenticated peer the leader's node resolved, not the one
    /// the caller asked for.
    peer: u64,
    incarnation: u64,
    reliable: bool,
    generation: u64,
}

#[wasm_bindgen]
impl ProxyStream {
    /// The stream id, 16 lowercase hex digits.
    pub fn stream_id_hex(&self) -> String {
        format!("{:016x}", self.stream_id)
    }

    /// The peer this stream is with, 16 lowercase hex digits.
    ///
    /// **Spelled exactly like [`crate::wasm::LeafStream::peer_node_hex`]**
    /// because a consumer must not need to know whether its stream is
    /// direct or proxied to filter by peer. Its absence here was the
    /// R4-10 cross-peer admixture, still open on every proxied handle
    /// — including the leader tab's own `MeshSession`, which wraps the
    /// same `ProxyStream`.
    pub fn peer_node_hex(&self) -> String {
        format!("{:016x}", self.peer)
    }

    /// The incarnation of the session this stream was opened on,
    /// decimal — the spelling the event carries.
    pub fn incarnation(&self) -> String {
        self.incarnation.to_string()
    }

    /// Whether this stream retransmits.
    pub fn is_reliable(&self) -> bool {
        self.reliable
    }

    /// The generation that opened this stream, as a decimal string.
    pub fn generation(&self) -> String {
        self.generation.to_string()
    }

    /// Send one payload.
    ///
    /// Refused, typed, once leadership has moved: see the type's own
    /// documentation for why this is not merely an optimisation.
    ///
    /// # What resolving means, and what it does not
    ///
    /// Three dispositions are distinct and only the first is this
    /// promise's:
    ///
    /// 1. **Admission.** Resolving says the node that owns the stream
    ///    accepted the bytes and enqueued them — a reliable stream has
    ///    taken ownership of them for retransmission, a
    ///    fire-and-forget one has queued one packet. That is the
    ///    contract, and it is the same one `LeafStream.send` has on
    ///    the direct surface.
    /// 2. **Transport refusal.** A DataChannel that refuses the
    ///    packet at the flush boundary is reported there
    ///    (`crate::wasm::Inner::flush`) and does *not* reject this
    ///    promise: the flush happens after admission, on the node's
    ///    own pump, and a reliable stream's retransmission is what
    ///    answers for it.
    /// 3. **Acknowledgement.** Nothing here waits for one. A reliable
    ///    stream's delivery is observable through its peer, not
    ///    through the resolution of this call; an unacknowledged
    ///    payload is retransmitted, and a stream that exhausts its
    ///    retries fails the stream rather than this send.
    ///
    /// A page that needs end-to-end confirmation asks for one in its
    /// own protocol — an nRPC call, or a reply on the stream. Making
    /// this promise wait for an ACK would turn every send into a round
    /// trip and still could not speak for a fire-and-forget stream.
    pub async fn send(&self, payload: Uint8Array) -> Result<(), JsError> {
        self.still_ours()?;
        self.lifecycle
            .request(LeaderRequest::StreamSend {
                // The owning handle, not the wire id: the id is shared
                // with any other peer's stream under the same label.
                handle: self.handle,
                payload: Bytes::from(payload.to_vec()),
            })
            .await?;
        Ok(())
    }

    /// Listen for inbound payloads on this stream.
    ///
    /// Filtered from the session's event stream by **both halves of
    /// the key** — the peer and the stream id — which is the same
    /// mechanism and the same event spelling a leader-local stream
    /// uses ([`crate::wasm::LeafStream::on_message`]), so a
    /// follower's stream and a leader's deliver through one path;
    /// **and** by the generation that opened it, so a stale handle's
    /// consumer stops receiving rather than starts receiving a
    /// successor's bytes.
    ///
    /// The peer half was missing here, and the id alone is not the
    /// identity of a stream: two peers under one label share it, so a
    /// proxied consumer received whichever peer's payload arrived.
    /// That is R4-10, on the proxied path.
    pub fn on_message(&self, callback: Function) {
        // The event's own spelling of each half: decimal, exactly as
        // `to_json` writes them.
        let wanted_peer = format!("\"peer_node\":\"{}\"", self.peer);
        let wanted_stream = format!("\"stream_id\":\"{}\"", self.stream_id);
        let lifecycle = self.lifecycle.clone();
        let generation = self.generation;
        let filter = Closure::wrap(Box::new(move |json: JsValue| {
            if lifecycle.generation() != generation {
                return;
            }
            if json.as_string().is_some_and(|text| {
                text.contains("\"stream_data\"")
                    && text.contains(&wanted_peer)
                    && text.contains(&wanted_stream)
            }) {
                let _ = callback.call1(&JsValue::NULL, &json);
            }
        }) as Box<dyn FnMut(JsValue)>);
        let function = filter.as_ref().unchecked_ref::<Function>().clone();
        filter.forget();
        self.lifecycle.on_event(function);
    }

    /// Stop using the stream.
    ///
    /// Not a resurrection point: D2 is explicit that a stream is
    /// session-scoped and is **not** restored across a leader change.
    /// A page whose leader went away opens a new stream — and this
    /// handle refuses to close anything on a successor, because the
    /// stream it named is already gone with the node that owned it.
    ///
    /// The generation is checked twice, and the second one is the one
    /// that matters. This method is synchronous and the request is
    /// not, so the admission check below happens in the caller's turn
    /// while the request is dispatched in a later one: a handoff in
    /// between would have stamped the close as *current* and torn
    /// down whatever stream the successor had opened with this id.
    /// The check at admission stays because it is the one that makes a
    /// stale close cost no task at all.
    pub fn close(&self) {
        if self.still_ours().is_err() {
            return;
        }
        let lifecycle = self.lifecycle.clone();
        let handle = self.handle;
        let generation = self.generation;
        spawn_local(async move {
            if lifecycle.generation() != generation {
                return;
            }
            let _ = lifecycle
                .request(LeaderRequest::StreamClose { handle })
                .await;
        });
    }

    /// Whether the generation that opened this stream is still the one
    /// in force.
    fn still_ours(&self) -> Result<(), JsError> {
        let current = self.lifecycle.generation();
        if current == self.generation {
            return Ok(());
        }
        Err(js(LeafError::NotLeader {
            presented: self.generation,
            current: Some(current),
        }))
    }
}

// ─────────────────────────────── plumbing ──────────────────────────────

/// Resolve the identity and return the connect options carrying it,
/// plus its fingerprint.
///
/// **One surface, two sources.** A host that supplied
/// `entitySecretHex` holds custody and nothing is stored; otherwise the
/// identity comes from IndexedDB, generated there on first run. Either
/// way the result is an [`IdentitySecrets`] and the options object a
/// node is connected with, so nothing downstream — promotion included
/// — has two cases to handle.
async fn identity_opts(opts: &JsValue, vault: &IdentityVault) -> Result<(JsValue, String)> {
    let (secrets, connect_opts) = match optional_string(opts, "entitySecretHex") {
        Some(entity_hex) => {
            let noise_hex = optional_string(opts, "noiseSecretHex");
            let secrets = IdentitySecrets::from_hex(&entity_hex, noise_hex.as_deref())?;
            // The custodial hex is already on the object the caller
            // passed, except for a generated Noise half.
            let connect_opts = clone_opts(opts)?;
            if noise_hex.is_none() {
                let (_, noise) = secrets.to_hex_pair();
                set(&connect_opts, "noiseSecretHex", &JsValue::from_str(&noise))?;
            }
            (secrets, connect_opts)
        }
        None => {
            let secrets = vault.load_or_create().await?;
            let connect_opts = clone_opts(opts)?;
            let (entity, noise) = secrets.to_hex_pair();
            set(
                &connect_opts,
                "entitySecretHex",
                &JsValue::from_str(&entity),
            )?;
            set(&connect_opts, "noiseSecretHex", &JsValue::from_str(&noise))?;
            (secrets, connect_opts)
        }
    };
    let fingerprint = secrets.into_identity().fingerprint();
    Ok((connect_opts.into(), fingerprint))
}

/// A shallow copy of the options object, so filling in the identity
/// does not mutate the caller's.
fn clone_opts(opts: &JsValue) -> Result<Object> {
    let out = Object::new();
    if let Some(source) = opts.dyn_ref::<Object>() {
        Object::assign(&out, source);
    }
    Ok(out)
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<()> {
    Reflect::set(object, &JsValue::from_str(key), value)
        .map(|_| ())
        .map_err(|e| js_err(&format!("setting {key}"), &e))
}

/// A fresh follower id. Random, not a counter: two tabs have no shared
/// counter to draw from, and a collision would merge two followers'
/// declared subscriptions into one registry entry.
fn follower_id() -> Result<u64> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| LeafError::Identity(format!("no CSPRNG available: {e}")))?;
    Ok(u64::from_le_bytes(bytes))
}

/// A fresh correlation seed, for the reason `CallTable::with_seed`
/// exists: a follower that reattaches after a leader change must not
/// reuse a predecessor's ids.
fn correlation_seed() -> Result<u64> {
    follower_id()
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}

fn window() -> Result<web_sys::Window> {
    web_sys::window().ok_or_else(|| {
        LeafError::Identity("no `window`: the leaf runs on the main thread (S0b)".into())
    })
}

/// A failure that crossed the `wasm_bindgen` boundary, carried as the
/// text it arrived as. See [`ProxyFailure`] for why it is not re-typed
/// here.
fn reported(error: JsError) -> ProxyFailure {
    let value: JsValue = error.into();
    let text = value
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"));
    ProxyFailure::Reported(text)
}

fn js_err(what: &str, error: &JsValue) -> LeafError {
    let detail = error
        .as_string()
        .or_else(|| {
            error
                .dyn_ref::<js_sys::Error>()
                .map(|error| String::from(error.message()))
        })
        .unwrap_or_else(|| format!("{error:?}"));
    LeafError::Identity(format!("{what}: {detail}"))
}

fn js(error: LeafError) -> JsError {
    JsError::new(&error.to_string())
}

fn report(message: &str) {
    web_sys::console::warn_1(&JsValue::from_str(&format!("net-mesh-leaf: {message}")));
}

fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

fn optional_string(opts: &JsValue, key: &str) -> Option<String> {
    Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
        .filter(|text| !text.is_empty())
}

fn string_array(opts: &JsValue, key: &str) -> Vec<String> {
    Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Array>().ok())
        .map(|array| array.iter().filter_map(|item| item.as_string()).collect())
        .unwrap_or_default()
}

impl From<ProxyFailure> for JsError {
    fn from(failure: ProxyFailure) -> Self {
        // The same message a local call would have thrown, so
        // `@net-mesh/browser` re-types a proxied failure and a direct
        // one through one parser.
        JsError::new(&failure.message())
    }
}
