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

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use bytes::Bytes;
use futures_channel::oneshot;
use js_sys::{Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{BroadcastChannel, MessageEvent};

use crate::error::{LeafError, Result, RpcError};
use crate::identity::IdentitySecrets;
use crate::leader::{
    scope_name, FollowerEvent, LeaderBackend, LeaderRequest, ProxyClient, ProxyFailure,
    ProxyOutcome, ProxyServer, ProxyTransport, ProxyValue, Replier,
};
use crate::storage::{IdentityVault, DEFAULT_DB_NAME};
use crate::stream::Reliability;

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
/// Called with the connect options (already carrying the identity) and
/// the sink every node event must be handed to. A factory rather than a
/// concrete type because the lifecycle has to be drivable in a browser
/// with no anchor in reach: the leaf's own wasm tests supply a backend
/// with no network behind it and exercise the same promotion,
/// restoration and fencing code the real one runs.
pub type BackendFactory = Rc<dyn Fn(JsValue, EventSink) -> BackendFuture>;

/// Where a leader's node events go.
pub type EventSink = Rc<dyn Fn(&str)>;

struct SessionState {
    role: Role,
    generation: u64,
    scope: String,
    fingerprint: String,
    connect_opts: JsValue,
    capabilities: Vec<String>,
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

/// Everything the session shares between its callbacks.
///
/// Three separate `RefCell`s, not one: the node's event callback fires
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
            Some(lock) => take_leadership(&shared, lock, None).await?,
            None => {
                let seed = correlation_seed()?;
                let follower = follower_id()?;
                let declared: Vec<String> =
                    shared.state.borrow().declared.iter().cloned().collect();
                let mut client =
                    ProxyClient::new(shared.transport.clone(), follower, declared, seed);
                client.attach();
                *shared.client.borrow_mut() = Some(client);
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
        if let LeaderRequest::Subscribe { channel } = &request {
            self.shared
                .state
                .borrow_mut()
                .declared
                .insert(channel.clone());
        }

        let receiver = {
            let mut server = self.shared.server.borrow_mut();
            if let Some(server) = server.as_mut() {
                let (tx, rx) = oneshot::channel();
                let generation = server.generation();
                let reply = Replier::local(tx, generation);
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
                client.request(request)
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

    /// Register a listener for this session's event JSON.
    pub fn on_event(&self, callback: Function) {
        self.shared.state.borrow_mut().listeners.push(callback);
    }

    /// Stand down: fail this tab's in-flight work, tell the other
    /// tabs, and let go of the lock so a follower can promote.
    pub fn close(&self) {
        {
            let mut state = self.shared.state.borrow_mut();
            if state.closed {
                return;
            }
            state.closed = true;
        }
        let generation = self.generation();

        // D2 row 1, this tab's own half: nothing is retried.
        if let Some(client) = self.shared.client.borrow_mut().as_mut() {
            client.detach();
            client.fail_pending(ProxyFailure::Typed(LeafError::Rpc(RpcError::LeaderLost {
                generation,
            })));
        }
        if let Some(server) = self.shared.server.borrow_mut().as_mut() {
            // Tell the followers before the lock moves, so their
            // pending work fails against the generation that owned it
            // rather than against the successor's.
            server.announce_leader_lost(generation);
            server.close();
        }
        *self.shared.server.borrow_mut() = None;
        *self.shared.client.borrow_mut() = None;
        // Releasing the lock is what triggers a follower's promotion.
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
        if let Err(error) = take_leadership(&self.shared, lock, Some(previous)).await {
            report(&format!("promoting to leader of {scope}: {error}"));
        }
    }
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
///    followers re-declare their subscriptions.
/// 5. **Restore** the subscriptions and re-publish the announcement.
///    Streams are deliberately *not* resurrected: a stream is
///    session-scoped, and pretending otherwise would hide a real
///    interruption.
async fn take_leadership(shared: &Rc<Shared>, lock: WebLock, previous: Option<u64>) -> Result<()> {
    let started = now_ms();
    let generation = shared.vault.next_generation().await?;

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
    let backend = factory(connect_opts, sink).await?;

    let mut server = ProxyServer::new(backend, shared.transport.clone(), generation);
    server.announce_leadership();
    *shared.server.borrow_mut() = Some(server);
    {
        let mut state = shared.state.borrow_mut();
        state.role = Role::Leader;
        state.generation = generation;
        state.lock = Some(lock);
        state.subscribed.clear();
    }
    emit(
        shared,
        &format!("{{\"type\":\"leader_changed\",\"generation\":\"{generation}\"}}"),
    );

    reconcile(shared).await;

    let capabilities = shared.state.borrow().capabilities.clone();
    if !capabilities.is_empty() {
        let lifecycle = Lifecycle {
            shared: shared.clone(),
        };
        if let Err(failure) = lifecycle
            .request(LeaderRequest::Announce { capabilities })
            .await
        {
            report(&format!("re-publishing the announcement: {failure:?}"));
        }
    }

    if previous.is_some() {
        shared.state.borrow_mut().interruption_ms = Some(now_ms() - started);
    }
    Ok(())
}

/// Subscribe everything the leader owes — this tab's own channels plus
/// the union of its followers' declared ones — and tell the followers
/// which ones came back.
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
        let reconciling = Rc::clone(shared);
        spawn_local(async move { reconcile(&reconciling).await });
        flush_events(shared);
        return;
    }

    // Follower: gate it.
    let event = {
        let mut client = shared.client.borrow_mut();
        match client.as_mut() {
            Some(client) => client.on_message(text),
            None => return,
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

/// A leader that has seen a higher generation is not the leader.
///
/// It fails its own in-flight work against the generation that owned
/// it, drops the node, and stops serving. It does *not* try to reclaim
/// the lock: the successor holds it, and a tab that fought for it would
/// be the eviction §8 exists to prevent.
fn stand_down(shared: &Rc<Shared>, successor: u64) {
    let ours = shared.state.borrow().generation;
    report(&format!(
        "generation {ours} was superseded by {successor}; standing down"
    ));
    if let Some(server) = shared.server.borrow_mut().as_mut() {
        server.close();
    }
    *shared.server.borrow_mut() = None;
    {
        let mut state = shared.state.borrow_mut();
        state.role = Role::Follower;
        state.lock = None;
        state.subscribed.clear();
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
        if let Some(server) = shared.server.borrow_mut().as_mut() {
            server.broadcast_event(json);
        }
        emit(&shared, json);
    })
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
fn flush_events(shared: &Rc<Shared>) {
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

/// The real backend: the leaf node this tab runs.
struct NodeBackend {
    node: Rc<crate::wasm::LeafNode>,
    node_id: u64,
    streams: Rc<RefCell<HashMap<u64, crate::wasm::LeafStream>>>,
}

impl LeaderBackend for NodeBackend {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn perform(&mut self, request: LeaderRequest, reply: Replier) {
        let node = self.node.clone();
        match request {
            LeaderRequest::Call {
                service,
                payload,
                timeout_ms,
            } => spawn_local(async move {
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
            LeaderRequest::Subscribe { channel } => spawn_local(async move {
                match node.subscribe(channel).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Publish { channel, payload } => spawn_local(async move {
                match node.publish(channel, Uint8Array::from(&payload[..])).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Announce { capabilities } => spawn_local(async move {
                match node.announce(capabilities).await {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Query { capability } => spawn_local(async move {
                match node.query(capability).await {
                    Ok(json) => reply.text(json),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::Counters => reply.text(node.counters_json()),
            LeaderRequest::IsEnrolled => reply.flag(node.is_enrolled()),
            LeaderRequest::Enroll => spawn_local(async move {
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
            } => spawn_local(async move {
                match node
                    .signal(
                        format!("{peer:016x}"),
                        dialog as f64,
                        kind,
                        Uint8Array::from(&payload[..]),
                    )
                    .await
                {
                    Ok(()) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(reported(error)),
                }
            }),
            LeaderRequest::StreamOpen {
                label,
                reliability,
                stream_id,
                channel_hash,
            } => {
                let opts = Object::new();
                let spelling = match reliability {
                    Reliability::Reliable => "reliable",
                    Reliability::FireAndForget => "fireAndForget",
                };
                if set(&opts, "reliability", &JsValue::from_str(spelling))
                    .and_then(|()| set(&opts, "label", &JsValue::from_str(&label)))
                    .is_err()
                {
                    reply.fail(ProxyFailure::Typed(LeafError::Session(
                        "could not build the stream options".into(),
                    )));
                    return;
                }
                if let Some(id) = stream_id {
                    let _ = set(&opts, "streamId", &JsValue::from_str(&id.to_string()));
                }
                if let Some(hash) = channel_hash {
                    let _ = set(&opts, "channelHash", &JsValue::from_f64(f64::from(hash)));
                }
                match node.open_stream(opts.into()) {
                    Ok(stream) => {
                        let id = u64::from_str_radix(&stream.stream_id_hex(), 16).unwrap_or(0);
                        self.streams.borrow_mut().insert(id, stream);
                        reply.stream(id);
                    }
                    Err(error) => reply.fail(reported(error)),
                }
            }
            LeaderRequest::StreamSend { stream_id, payload } => {
                let streams = self.streams.borrow();
                match streams.get(&stream_id) {
                    Some(stream) => match stream.send(Uint8Array::from(&payload[..])) {
                        Ok(()) => reply.bytes(Bytes::new()),
                        Err(error) => reply.fail(reported(error)),
                    },
                    None => reply.fail(ProxyFailure::Typed(LeafError::Session(format!(
                        "no open stream {stream_id:#018x}"
                    )))),
                }
            }
            LeaderRequest::StreamClose { stream_id } => {
                if let Some(stream) = self.streams.borrow_mut().remove(&stream_id) {
                    stream.close();
                }
                reply.bytes(Bytes::new());
            }
        }
    }
}

/// The production factory: connect a real node and route its events.
fn node_factory() -> BackendFactory {
    Rc::new(|opts: JsValue, sink: EventSink| {
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
                streams: Rc::new(RefCell::new(HashMap::new())),
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
    pub async fn subscribe(&self, channel: String) -> Result<(), JsError> {
        self.lifecycle
            .request(LeaderRequest::Subscribe { channel })
            .await?;
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
    pub async fn announce(&self, capabilities: Vec<String>) -> Result<(), JsError> {
        self.lifecycle
            .request(LeaderRequest::Announce { capabilities })
            .await?;
        Ok(())
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
        let peer = u64::from_str_radix(peer_hex.trim_start_matches("0x"), 16)
            .map_err(|_| JsError::new("peer_hex must be hex"))?;
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
        let reliability = match optional_string(&opts, "reliability") {
            Some(spelling) => Reliability::parse(&spelling).ok_or_else(|| {
                JsError::new("reliability must be \"reliable\" or \"fireAndForget\"")
            })?,
            None => Reliability::Reliable,
        };
        let stream_id = match optional_string(&opts, "streamId") {
            Some(raw) => Some(parse_u64(&raw)?),
            None => None,
        };
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let channel_hash = optional_f64(&opts, "channelHash").map(|value| value as u16);

        let value = self
            .lifecycle
            .request(LeaderRequest::StreamOpen {
                label: optional_string(&opts, "label").unwrap_or_else(|| "app".to_string()),
                reliability,
                stream_id,
                channel_hash,
            })
            .await?;
        let ProxyValue::Stream { stream_id } = value else {
            return Err(JsError::new("open_stream did not answer with a stream"));
        };
        Ok(ProxyStream {
            lifecycle: self.lifecycle.clone(),
            stream_id,
            reliable: reliability.is_reliable(),
        })
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
#[wasm_bindgen]
pub struct ProxyStream {
    lifecycle: Lifecycle,
    stream_id: u64,
    reliable: bool,
}

#[wasm_bindgen]
impl ProxyStream {
    /// The stream id, 16 lowercase hex digits.
    pub fn stream_id_hex(&self) -> String {
        format!("{:016x}", self.stream_id)
    }

    /// Whether this stream retransmits.
    pub fn is_reliable(&self) -> bool {
        self.reliable
    }

    /// Send one payload.
    pub async fn send(&self, payload: Uint8Array) -> Result<(), JsError> {
        self.lifecycle
            .request(LeaderRequest::StreamSend {
                stream_id: self.stream_id,
                payload: Bytes::from(payload.to_vec()),
            })
            .await?;
        Ok(())
    }

    /// Listen for inbound payloads on this stream.
    ///
    /// Filtered from the session's event stream by stream id, which is
    /// the same mechanism a leader-local stream uses — so a follower's
    /// stream and a leader's deliver through one path.
    pub fn on_message(&self, callback: Function) {
        let wanted = format!("\"stream_id\":\"{}\"", self.stream_id);
        let filter = Closure::wrap(Box::new(move |json: JsValue| {
            if json
                .as_string()
                .is_some_and(|text| text.contains(&wanted) && text.contains("\"stream_data\""))
            {
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
    /// A page whose leader went away opens a new stream.
    pub fn close(&self) {
        let lifecycle = self.lifecycle.clone();
        let stream_id = self.stream_id;
        spawn_local(async move {
            let _ = lifecycle
                .request(LeaderRequest::StreamClose { stream_id })
                .await;
        });
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

fn optional_f64(opts: &JsValue, key: &str) -> Option<f64> {
    Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
}

fn string_array(opts: &JsValue, key: &str) -> Vec<String> {
    Reflect::get(opts, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Array>().ok())
        .map(|array| array.iter().filter_map(|item| item.as_string()).collect())
        .unwrap_or_default()
}

fn parse_u64(raw: &str) -> Result<u64, JsError> {
    let trimmed = raw.trim();
    let parsed = match trimmed.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => trimmed.parse(),
    };
    parsed.map_err(|_| JsError::new(&format!("{raw:?} is not a u64")))
}

impl From<ProxyFailure> for JsError {
    fn from(failure: ProxyFailure) -> Self {
        // The same message a local call would have thrown, so
        // `@net-mesh/browser` re-types a proxied failure and a direct
        // one through one parser.
        JsError::new(&failure.message())
    }
}
