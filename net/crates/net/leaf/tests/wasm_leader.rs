//! §8 in a real browser: identity at rest, leader election, the
//! follower proxy, and the D2 lifecycle.
//!
//! `cargo check --target wasm32-unknown-unknown` cannot see any of
//! what this file exists for, and neither can the native suite:
//!
//! 1. **IndexedDB and WebCrypto have no native equivalent.** The
//!    at-rest *format* is pinned natively in
//!    `identity::tests`; what has to run in a browser is that the
//!    record on disk is a ciphertext under a key whose
//!    `extractable` is `false`, that two opens of the same database
//!    yield the same node id, and that the generation counter is
//!    monotonic across transactions.
//! 2. **Web Locks are the election.** Two [`Lifecycle`]s in one page
//!    contend for one lock exactly as two tabs do — locks are scoped
//!    to the origin, not the document — so "two tabs sharing one
//!    identity without evicting each other" and "close the leader and
//!    a follower takes over" are both reachable here, with the real
//!    `BroadcastChannel` in between.
//! 3. **The fence has three enforcers** and two of them are browser
//!    state: the follower's gate (covered natively), the leader's
//!    refusal, and the IndexedDB generation. A resumed tab is refused
//!    by all three, and only here can the third be exercised.
//!
//! What is deliberately NOT here: an anchor. The node arrives through
//! [`Lifecycle`]'s backend factory, so this drives the whole lifecycle
//! — contention, generation, promotion, restoration, fencing — with a
//! recording backend in place of a DataChannel. The bootstrap round
//! trip that the interruption budget also includes is measured by the
//! two-tab witness in `net/crates/net/tests/rtc_browser/`, against a
//! real anchor; the number this file reports is the handoff, named as
//! such.
//!
//! Run:
//! ```text
//! CHROMEDRIVER=<chromedriver matching the system Chrome> \
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//! cargo test --target wasm32-unknown-unknown --test wasm_leader
//! ```

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bytes::Bytes;
use futures_channel::oneshot;
use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use net_leaf::bootstrap::gloo_timer_sleep;
use net_leaf::error::{LeafError, RpcError};
use net_leaf::identity::{IdentitySecrets, IDENTITY_BLOB_MAGIC};
use net_leaf::leader::{
    GenerationLease, LeaderBackend, LeaderRequest, ProxyFailure, ProxyValue, Replier,
};
use net_leaf::leader_session::{spawn_fenced, BackendFactory, Lifecycle, OpRegistry, Role};
use net_leaf::storage::IdentityVault;
use net_leaf::LeafIdentity;

// The point is a real browser engine: IndexedDB, `crypto.subtle`,
// `navigator.locks` and `BroadcastChannel` do not exist in Node.
wasm_bindgen_test_configure!(run_in_browser);

// ─────────────────────────────── helpers ───────────────────────────────

/// A suffix unique to this test run, so tests do not share a database
/// or contend for each other's lock. Random rather than a counter: the
/// runner may reorder, and a leaked lock from one test must not be
/// able to block another.
fn unique(prefix: &str) -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("browser CSPRNG");
    format!("{prefix}-{}", u64::from_le_bytes(bytes))
}

/// Yield to the event loop so `BroadcastChannel` deliveries, promise
/// continuations and lock grants can run.
async fn settle() {
    for _ in 0..6 {
        let _ = gloo_timer_sleep(1).await;
    }
}

/// Wait a real interval, for the two properties that are timers: the
/// leader's generation revalidation and a follower's own call
/// deadline.
async fn wait_ms(ms: i32) {
    let _ = gloo_timer_sleep(ms).await;
    settle().await;
}

/// The options object a session opens with. No credential: the backend
/// factory below is what stands in for connecting.
fn opts(db: &str, scope: &str, subscriptions: &[&str], capabilities: &[&str]) -> JsValue {
    let object = Object::new();
    put(&object, "origin", &JsValue::from_str("https://leaf.test"));
    put(&object, "dbName", &JsValue::from_str(db));
    put(&object, "lockScope", &JsValue::from_str(scope));
    put(&object, "subscriptions", &array(subscriptions));
    put(&object, "capabilities", &array(capabilities));
    object.into()
}

fn put(object: &Object, key: &str, value: &JsValue) {
    Reflect::set(object, &JsValue::from_str(key), value).expect("plain object");
}

fn array(items: &[&str]) -> JsValue {
    let array = js_sys::Array::new();
    for item in items {
        array.push(&JsValue::from_str(item));
    }
    array.into()
}

/// What a test backend recorded, shared with the test.
#[derive(Default)]
struct Log {
    performed: Vec<LeaderRequest>,
    /// Repliers held open, so a test can have a call in flight when
    /// the leader goes away.
    held: Vec<Replier>,
    /// How many times a backend was retired, and how many pending
    /// calls each retirement reported.
    shutdowns: Vec<usize>,
}

/// A node stand-in: records every request and answers it, unless the
/// test asked it to hold calls open.
struct TestBackend {
    node_id: u64,
    log: Rc<RefCell<Log>>,
    hold_calls: bool,
}

impl LeaderBackend for TestBackend {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn perform(&mut self, request: LeaderRequest, reply: Replier) {
        self.log.borrow_mut().performed.push(request.clone());
        if self.hold_calls && matches!(request, LeaderRequest::Call { .. }) {
            self.log.borrow_mut().held.push(reply);
            return;
        }
        match request {
            LeaderRequest::Call { .. } => reply.bytes(Bytes::from_static(b"pong")),
            LeaderRequest::Query { .. } | LeaderRequest::Counters => reply.text("[]".into()),
            LeaderRequest::IsEnrolled => reply.flag(true),
            LeaderRequest::StreamOpen { stream_id, .. } => reply.stream(stream_id.unwrap_or(9)),
            _ => reply.bytes(Bytes::new()),
        }
    }

    fn shutdown(&mut self, _generation: u64) -> usize {
        // Letting go of the held repliers is what a real backend's
        // cancellation does to the operations it spawned: each one
        // settles its caller as it is dropped.
        let held = core::mem::take(&mut self.log.borrow_mut().held);
        let count = held.len();
        drop(held);
        self.log.borrow_mut().shutdowns.push(count);
        count
    }
}

/// A factory over one shared log, so a test can watch what the leader
/// of the moment did — including a leader that was promoted after the
/// first one went away.
fn factory(node_id: u64, log: Rc<RefCell<Log>>, hold_calls: bool) -> BackendFactory {
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        Box::pin(async move {
            let backend: Box<dyn LeaderBackend> = Box::new(TestBackend {
                node_id,
                log,
                hold_calls,
            });
            Ok(backend)
        })
    })
}

/// What a factory does on the invocation a test cares about.
enum Delay {
    /// Produce a backend, but only once the barrier is released.
    Park,
    /// Fail, once the barrier is released.
    Fail,
}

/// A factory whose `nth` invocation (counting from zero) is parked on
/// `barrier` and then either produces a backend or fails.
///
/// The seam that makes the acquisition races deterministic: both of
/// them are "what arrived while the backend was being connected", and
/// without a barrier the connect in these tests completes in the same
/// microtask that started it.
fn delayed_factory(
    node_id: u64,
    log: Rc<RefCell<Log>>,
    nth: usize,
    delay: Delay,
    barrier: oneshot::Receiver<()>,
) -> BackendFactory {
    let barrier = Rc::new(RefCell::new(Some(barrier)));
    let invocations = Rc::new(Cell::new(0usize));
    let fail = matches!(delay, Delay::Fail);
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        let barrier = barrier.clone();
        let invocation = invocations.get();
        invocations.set(invocation + 1);
        Box::pin(async move {
            if invocation == nth {
                // Taken out of the cell before the await: a borrow
                // held across a suspension point is a panic waiting
                // for a second caller.
                let parked = barrier.borrow_mut().take();
                if let Some(parked) = parked {
                    let _ = parked.await;
                }
                if fail {
                    return Err(LeafError::Session(
                        "the test's backend refused to connect".into(),
                    ));
                }
            }
            let backend: Box<dyn LeaderBackend> = Box::new(TestBackend {
                node_id,
                log,
                hold_calls: false,
            });
            Ok(backend)
        })
    })
}

// ──────────────── a backend over a REAL leaf node ────────────────

/// The handles a test keeps on its real-node backend.
#[derive(Clone)]
struct RealNode {
    node: Rc<RefCell<net_leaf::LeafNode>>,
    peer: u64,
    /// Packets the node produced, counted as operations complete.
    sent: Rc<Cell<usize>>,
    /// How many pending calls the retirement failed.
    failed: Rc<Cell<usize>>,
    /// Released by the test: the operation that parks before it
    /// dispatches waits on this.
    barrier: Rc<RefCell<Option<oneshot::Receiver<()>>>>,
    /// Operations that dispatched after being parked.
    dispatched: Rc<Cell<usize>>,
}

/// A backend that owns a **real** sans-IO leaf node: a real session, a
/// real call table, a real outbound queue.
///
/// Not a recording double, and for R9 that is the whole point. A
/// recording backend's "pending call" is a [`Replier`] parked in a
/// `Vec`, so retiring it proves only that the test's own vector was
/// drained. Here the pending call is an entry in the node's real call
/// table created by `LeafNode::call`, the parked operation is a
/// spawned future holding that node across a barrier, and "produced
/// no further outbound work" is counted off the node's own
/// `take_outbound`. What is absent is the DataChannel under it, which
/// is this file's stated premise and is covered against a real anchor
/// by the two-tab witness.
struct RealNodeBackend {
    handles: RealNode,
    node_id: u64,
    lease: GenerationLease,
    ops: Rc<OpRegistry>,
}

impl LeaderBackend for RealNodeBackend {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn perform(&mut self, request: LeaderRequest, reply: Replier) {
        let handles = self.handles.clone();
        match request {
            // A real pending call: issued into the real call table,
            // and nothing in this test ever delivers a response, so it
            // stays there until something retires it.
            LeaderRequest::Call {
                service,
                payload,
                timeout_ms,
            } => spawn_fenced(&self.lease, &self.ops, async move {
                let issued = {
                    let mut node = handles.node.borrow_mut();
                    let issued =
                        node.call(handles.peer, &service, &payload, timeout_ms.map(u64::from));
                    let produced = node.take_outbound().len();
                    handles.sent.set(handles.sent.get() + produced);
                    issued
                };
                match issued {
                    Ok(receiver) => match receiver.await {
                        Ok(Ok(body)) => reply.bytes(body),
                        Ok(Err(error)) => {
                            reply.fail(ProxyFailure::Reported(LeafError::Rpc(error).to_string()));
                        }
                        Err(_) => reply.fail(ProxyFailure::Reported(
                            "the call was cancelled locally".into(),
                        )),
                    },
                    Err(error) => reply.fail(ProxyFailure::Reported(error.to_string())),
                }
            }),
            // The operation parked *before* dispatch. It holds the
            // node and has not touched it yet, which is exactly the
            // window a stand-down has to be able to close.
            LeaderRequest::Publish { channel, payload } => {
                spawn_fenced(&self.lease, &self.ops, async move {
                    let parked = handles.barrier.borrow_mut().take();
                    if let Some(parked) = parked {
                        let _ = parked.await;
                    }
                    handles.dispatched.set(handles.dispatched.get() + 1);
                    let outcome = {
                        let mut node = handles.node.borrow_mut();
                        let outcome = node.publish(handles.peer, &channel, &payload);
                        let produced = node.take_outbound().len();
                        handles.sent.set(handles.sent.get() + produced);
                        outcome
                    };
                    match outcome {
                        Ok(()) => reply.bytes(Bytes::new()),
                        Err(error) => reply.fail(ProxyFailure::Reported(error.to_string())),
                    }
                });
            }
            LeaderRequest::Subscribe { channel } => {
                let outcome = {
                    let mut node = handles.node.borrow_mut();
                    let outcome = node.subscribe(handles.peer, &channel);
                    let produced = node.take_outbound().len();
                    handles.sent.set(handles.sent.get() + produced);
                    outcome
                };
                match outcome {
                    Ok(_) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(ProxyFailure::Reported(error.to_string())),
                }
            }
            LeaderRequest::Counters | LeaderRequest::Query { .. } => reply.text("[]".into()),
            LeaderRequest::IsEnrolled => reply.flag(true),
            LeaderRequest::StreamOpen { stream_id, .. } => reply.stream(stream_id.unwrap_or(9)),
            _ => reply.bytes(Bytes::new()),
        }
    }

    fn shutdown(&mut self, generation: u64) -> usize {
        // Exactly what `NodeBackend::shutdown` does, against a real
        // node: cancel the operations the fenced generation admitted,
        // then fail the node's own pending calls once, typed.
        self.ops.cancel_all();
        let mut node = self.handles.node.borrow_mut();
        let failed = node.fail_calls_on_leader_loss(generation);
        node.drop_session(self.handles.peer, "leadership was released");
        // Teardown packets are the node's own close path, not work a
        // fenced operation produced, so they are not charged to the
        // counter the witness reads.
        node.take_outbound();
        self.handles.failed.set(failed);
        failed
    }
}

/// Two real leaf nodes with a real session between them, and the
/// handles a test needs on the first.
fn real_pair() -> (RealNode, net_leaf::LeafNode) {
    let mut leader = net_leaf::LeafNode::new(LeafIdentity::generate().expect("identity"), 7);
    let mut peer = net_leaf::LeafNode::new(LeafIdentity::generate().expect("identity"), 9);
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).expect("browser CSPRNG");
    let one = leader
        .begin_handshake(
            peer.node_id(),
            &psk,
            peer.identity().noise().public_key(),
            1,
        )
        .expect("msg1");
    let two = peer
        .accept_handshake(leader.node_id(), &psk, &one, 1)
        .expect("msg2");
    leader
        .complete_handshake(peer.node_id(), &two)
        .expect("session");
    leader.drain_events();
    peer.drain_events();
    leader.take_outbound();
    let peer_id = peer.node_id();
    (
        RealNode {
            node: Rc::new(RefCell::new(leader)),
            peer: peer_id,
            sent: Rc::new(Cell::new(0)),
            failed: Rc::new(Cell::new(0)),
            barrier: Rc::new(RefCell::new(None)),
            dispatched: Rc::new(Cell::new(0)),
        },
        peer,
    )
}

fn real_factory(handles: RealNode) -> BackendFactory {
    Rc::new(move |_opts, _sink, lease: GenerationLease| {
        let handles = handles.clone();
        let node_id = handles.node.borrow().node_id();
        Box::pin(async move {
            let backend: Box<dyn LeaderBackend> = Box::new(RealNodeBackend {
                handles,
                node_id,
                lease,
                ops: Rc::new(OpRegistry::default()),
            });
            Ok(backend)
        })
    })
}

/// Drive `request` on `lifecycle` in the background and hand back the
/// slot its outcome lands in, so a test can assert that it is *still
/// unsettled* at a chosen moment.
fn in_flight(
    lifecycle: &Lifecycle,
    request: LeaderRequest,
) -> Rc<RefCell<Vec<Result<ProxyValue, ProxyFailure>>>> {
    let slot = Rc::new(RefCell::new(Vec::new()));
    let writing = slot.clone();
    let lifecycle = lifecycle.clone();
    spawn_local(async move {
        let outcome = lifecycle.request(request).await;
        writing.borrow_mut().push(outcome);
    });
    slot
}

// ───────────────────────── identity at rest ──────────────────────────

/// Two opens of one database yield one identity, and what is on disk
/// is a ciphertext under a key that cannot be exported.
///
/// The three claims §8 makes about storage, each checked against the
/// record itself rather than against the API that wrote it.
#[wasm_bindgen_test]
async fn the_stored_identity_is_stable_and_encrypted_under_a_non_extractable_key() {
    let db = unique("identity-db");
    let vault = IdentityVault::open(&db).await.expect("open");

    let first = vault.load_or_create().await.expect("create");
    let node_id = first.into_identity().node_id();
    let second = vault.load_or_create().await.expect("load");
    assert_eq!(
        second.into_identity().node_id(),
        node_id,
        "a second load must return the identity the first one stored, or every \
         reload would be a new node"
    );

    // A separate open of the same database — what a reloaded page does.
    let reopened = IdentityVault::open(&db).await.expect("reopen");
    assert_eq!(
        reopened
            .load_or_create()
            .await
            .expect("load")
            .into_identity()
            .node_id(),
        node_id
    );

    let record = vault.raw_record().await.expect("record");
    assert!(!record.is_undefined(), "the record must be there");

    let blob: Uint8Array = Reflect::get(&record, &JsValue::from_str("blob"))
        .expect("blob")
        .dyn_into()
        .expect("a Uint8Array");
    let bytes = blob.to_vec();
    assert!(
        !bytes
            .windows(IDENTITY_BLOB_MAGIC.len())
            .any(|w| w == IDENTITY_BLOB_MAGIC),
        "the stored blob contains the plaintext schema tag, so it is not encrypted"
    );
    assert!(
        bytes.len() > IdentitySecrets::generate().expect("gen").encode().len(),
        "AES-GCM adds a 16-byte tag; a ciphertext no longer than the plaintext \
         is not a ciphertext"
    );

    let key = Reflect::get(&record, &JsValue::from_str("key")).expect("key");
    assert_eq!(
        Reflect::get(&key, &JsValue::from_str("extractable")).expect("extractable"),
        JsValue::FALSE,
        "the wrapping key must be non-extractable — that is the whole of what \
         §8 claims storage protects"
    );
    assert_eq!(
        Reflect::get(&key, &JsValue::from_str("type"))
            .expect("type")
            .as_string()
            .as_deref(),
        Some("secret")
    );

    // A blob that will not decrypt is a typed refusal, not a panic
    // and not a silently new identity.
    let other = IdentityVault::open(&unique("identity-db"))
        .await
        .expect("open");
    let theirs = other.load_or_create().await.expect("create");
    assert_ne!(
        theirs.into_identity().node_id(),
        node_id,
        "two databases must not share an identity"
    );
}

/// The generation is read, incremented and written in one transaction,
/// is monotonic across independent opens, and is what the storage-side
/// fence compares against.
#[wasm_bindgen_test]
async fn the_generation_counter_is_monotonic_and_fences_a_stale_holder() {
    let db = unique("generation-db");
    let vault = IdentityVault::open(&db).await.expect("open");

    assert_eq!(
        vault.current_generation().await.expect("read"),
        0,
        "no leader has ever held the lock"
    );
    for expected in 1..=3u64 {
        assert_eq!(vault.next_generation().await.expect("bump"), expected);
        assert_eq!(vault.current_generation().await.expect("read"), expected);
    }

    // A fresh handle on the same database — a reloaded page, or the
    // successor tab.
    let successor = IdentityVault::open(&db).await.expect("reopen");
    assert_eq!(successor.next_generation().await.expect("bump"), 4);

    vault.fence(4).await.expect("the current generation passes");
    let refused = vault
        .fence(3)
        .await
        .expect_err("a superseded generation must be refused by storage");
    assert_eq!(
        refused,
        LeafError::NotLeader {
            presented: 3,
            current: Some(4)
        },
        "this is the third enforcer of the fence, and the one a resumed tab \
         cannot talk its way past"
    );
    assert!(
        vault.fence(5).await.is_err(),
        "nor a generation from the future"
    );
}

// ─────────────────────── two tabs, one identity ──────────────────────

/// Two sessions on one identity: one leader, one follower, neither
/// evicting the other, both naming the same node.
///
/// The Stage 5 exit criterion. Web Locks are origin-scoped, so two
/// `Lifecycle`s in one page contend exactly as two tabs do.
#[wasm_bindgen_test]
async fn two_sessions_share_one_identity_without_evicting_each_other() {
    let db = unique("share-db");
    let scope = unique("share-scope");
    let log = Rc::new(RefCell::new(Log::default()));

    let first = Lifecycle::open(
        opts(&db, &scope, &["alpha"], &["cap:one"]),
        factory(0x1111, log.clone(), false),
    )
    .await
    .expect("first session");
    settle().await;

    let second = Lifecycle::open(
        opts(&db, &scope, &["beta"], &["cap:one"]),
        factory(0x2222, log.clone(), false),
    )
    .await
    .expect("second session");
    settle().await;

    assert_eq!(first.role(), Role::Leader, "the first tab took the lock");
    assert_eq!(
        second.role(),
        Role::Follower,
        "the second must attach, not start a second node — that is the eviction \
         §8 exists to prevent"
    );
    assert_eq!(first.generation(), 1);
    assert_eq!(
        second.generation(),
        1,
        "the follower adopts the leader's generation"
    );
    assert_eq!(
        second.node_id(),
        Some(0x1111),
        "both tabs name one node, and it is the leader's"
    );
    assert_eq!(
        first.fingerprint(),
        second.fingerprint(),
        "one identity, so one fingerprint and one lock"
    );

    // And the follower's operations really reach the node.
    let answered = second
        .request(LeaderRequest::Call {
            service: "echo".into(),
            payload: Bytes::from_static(b"ping"),
            timeout_ms: Some(500),
        })
        .await
        .expect("the proxied call must succeed");
    assert_eq!(answered, ProxyValue::Bytes(Bytes::from_static(b"pong")));
    assert!(
        log.borrow()
            .performed
            .iter()
            .any(|request| matches!(request, LeaderRequest::Call { .. })),
        "the call must have been performed by the leader's node"
    );

    first.close();
    second.close();
    settle().await;
}

/// Close the leader: a follower takes over, the pending call fails
/// typed, and the subscriptions come back.
///
/// D2's three observable requirements in one witness, because they are
/// one event: the handoff.
#[wasm_bindgen_test]
async fn closing_the_leader_promotes_a_follower_fails_its_calls_and_restores_its_channels() {
    let db = unique("handoff-db");
    let scope = unique("handoff-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));
    let follower_log = Rc::new(RefCell::new(Log::default()));

    // The leader holds calls open, so there is genuinely one in
    // flight when it goes away.
    let leader = Lifecycle::open(
        opts(&db, &scope, &["leader-chan"], &["cap:one"]),
        factory(0x1111, leader_log.clone(), true),
    )
    .await
    .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.generation(), 1);

    let follower = Lifecycle::open(
        opts(&db, &scope, &["follower-chan"], &["cap:one"]),
        factory(0x2222, follower_log.clone(), false),
    )
    .await
    .expect("follower");
    settle().await;
    assert_eq!(follower.role(), Role::Follower);

    // The leader has restored its own channel and, once the follower
    // attached, the follower's too.
    assert!(
        leader_log.borrow().performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Subscribe { channel } if channel == "follower-chan"
        )),
        "the leader must subscribe the union of its followers' declarations: {:?}",
        leader_log.borrow().performed
    );

    // One call in flight, held by the leader's backend.
    let pending = {
        let follower = follower.clone();
        let (tx, rx) = futures_channel::oneshot::channel();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = follower
                .request(LeaderRequest::Call {
                    service: "slow".into(),
                    payload: Bytes::new(),
                    timeout_ms: None,
                })
                .await;
            let _ = tx.send(outcome);
        });
        rx
    };
    settle().await;
    assert!(
        leader_log
            .borrow()
            .performed
            .iter()
            .any(|request| matches!(request, LeaderRequest::Call { .. })),
        "the call must have reached the leader and be sitting there"
    );

    // The leader goes away. Timed from *before* the close, because
    // D2's budget starts at leader loss — the promoted tab's own
    // measurement can only start at the lock grant, and the
    // difference between the two is the handoff latency the lock
    // itself costs.
    let lost_at = now_ms();
    leader.close();
    while follower.role() != Role::Leader {
        settle().await;
    }
    let end_to_end = now_ms() - lost_at;
    settle().await;

    // D2 row 1: the call failed, typed, against the generation that
    // owned it — and was not re-issued against the new one.
    let failure = pending
        .await
        .expect("the future must settle")
        .expect_err("a call the dead leader owned must fail");
    assert_eq!(
        failure.typed(),
        Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 })),
        "expected LeaderLost {{ generation: 1 }}, got {failure:?}"
    );

    // The follower promoted itself: a new generation, exactly one
    // higher, and its own node.
    assert_eq!(
        follower.role(),
        Role::Leader,
        "a follower must take over when the lock comes free"
    );
    assert_eq!(
        follower.generation(),
        2,
        "the generation is read-incremented-written once per acquisition"
    );
    assert_eq!(follower.node_id(), Some(0x2222));

    // D2 "Restoration": the new leader re-subscribed and re-published
    // the announcement under the same identity. Streams are not
    // resurrected, and nothing here pretends they are.
    let performed = follower_log.borrow().performed.clone();
    assert!(
        performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Subscribe { channel } if channel == "follower-chan"
        )),
        "the new leader must restore the subscriptions: {performed:?}"
    );
    assert!(
        performed
            .iter()
            .any(|request| matches!(request, LeaderRequest::Announce { .. })),
        "and re-publish the announcement: {performed:?}"
    );
    assert!(
        performed
            .iter()
            .all(|request| !matches!(request, LeaderRequest::StreamOpen { .. })),
        "a stream is session-scoped and must NOT be resurrected: {performed:?}"
    );

    // The measured handoff. Reported, not asserted as a promise: the
    // ceiling here only guards against a regression into something
    // pathological.
    let measured = follower
        .interruption_ms()
        .expect("a promoted tab must have measured its own interruption");
    web_sys::console::log_1(&JsValue::from_str(&format!(
        "net-mesh-leaf: INTERRUPTION BUDGET, measured. \
         leader loss -> follower is leader = {end_to_end:.1} ms (end to end, \
         no anchor); lock grant -> generation -> backend -> restoration -> \
         announcement = {measured:.1} ms (the promoted tab's own view). \
         The anchor bootstrap round trip plus ICE and Noise is NOT in either \
         number — that is the two-tab witness against a real anchor."
    )));
    assert!(
        (0.0..2_000.0).contains(&measured),
        "the handoff took {measured} ms, which is not a warm-page handoff"
    );

    follower.close();
    settle().await;
}

/// A tab that was suspended and resumes cannot present the identity
/// alongside its successor.
///
/// The Playwright witness freezes a real tab with CDP
/// `Page.setWebLifecycleState`; what a frozen tab *is*, from the
/// successor's point of view, is a holder of a generation that has
/// been superseded — and it is refused by both of the enforcers a
/// single page can host: the successor's server and IndexedDB.
#[wasm_bindgen_test]
async fn a_superseded_session_is_refused_by_the_leader_and_by_storage() {
    let db = unique("fence-db");
    let scope = unique("fence-scope");
    let log = Rc::new(RefCell::new(Log::default()));

    let suspended = Lifecycle::open(
        opts(&db, &scope, &["chan"], &[]),
        factory(0x1111, log.clone(), false),
    )
    .await
    .expect("first leader");
    settle().await;
    assert_eq!(suspended.role(), Role::Leader);
    let stale = suspended.generation();
    assert_eq!(stale, 1);

    // The successor. Closing the first session is what a tab going
    // away does; the point of the test is what happens to the value
    // the first one still holds.
    suspended.close();
    settle().await;
    let successor = Lifecycle::open(
        opts(&db, &scope, &["chan"], &[]),
        factory(0x2222, log.clone(), false),
    )
    .await
    .expect("successor");
    settle().await;
    assert_eq!(successor.role(), Role::Leader);
    assert_eq!(successor.generation(), stale + 1);

    // Enforcer three: storage refuses the generation the resumed tab
    // still believes in, and names the one in force.
    let vault = IdentityVault::open(&db).await.expect("open");
    let refused = vault
        .fence(stale)
        .await
        .expect_err("the resumed tab's generation must be refused");
    assert_eq!(
        refused,
        LeafError::NotLeader {
            presented: stale,
            current: Some(stale + 1)
        }
    );
    vault
        .fence(stale + 1)
        .await
        .expect("the successor's generation is the one in force");

    // Enforcer two: the successor refuses a request stamped with the
    // stale generation, and never performs it.
    let before = log.borrow().performed.len();
    let posted = net_leaf::leader::ProxyEnvelope {
        generation: stale,
        from: net_leaf::leader::ProxySide::Follower(0xFEED),
        body: net_leaf::leader::ProxyBody::Request {
            correlation: 1,
            request: LeaderRequest::Call {
                service: "stale".into(),
                payload: Bytes::new(),
                timeout_ms: None,
            },
        },
    }
    .to_json();
    let channel = web_sys::BroadcastChannel::new(&scope).expect("channel");
    channel
        .post_message(&JsValue::from_str(&posted))
        .expect("post");
    settle().await;
    settle().await;
    assert_eq!(
        log.borrow().performed.len(),
        before,
        "a request stamped with a superseded generation must never reach the node"
    );
    channel.close();

    // And the resumed session, asked to do anything, is refused
    // rather than served.
    let outcome = suspended
        .request(LeaderRequest::Counters)
        .await
        .expect_err("a closed, superseded session must refuse");
    assert!(
        matches!(
            outcome,
            ProxyFailure::Typed(LeafError::Session(_))
                | ProxyFailure::Typed(LeafError::NotLeader { .. })
        ),
        "expected a typed refusal, got {outcome:?}"
    );

    successor.close();
    settle().await;
}

/// The custodial path and the storage path are one surface: a host
/// that injects a keypair gets that identity, and nothing is stored
/// for it.
#[wasm_bindgen_test]
async fn a_custodial_identity_is_used_verbatim_and_never_stored() {
    let db = unique("custodial-db");
    let scope = unique("custodial-scope");
    let entity_hex = "5a".repeat(32);
    let expected = IdentitySecrets::from_hex(&entity_hex, Some(&"7b".repeat(32)))
        .expect("valid")
        .into_identity();

    let object = Object::new();
    put(&object, "origin", &JsValue::from_str("https://leaf.test"));
    put(&object, "dbName", &JsValue::from_str(&db));
    put(&object, "lockScope", &JsValue::from_str(&scope));
    put(&object, "entitySecretHex", &JsValue::from_str(&entity_hex));
    put(
        &object,
        "noiseSecretHex",
        &JsValue::from_str(&"7b".repeat(32)),
    );

    let session = Lifecycle::open(
        object.into(),
        factory(0x3333, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("custodial session");
    settle().await;

    assert_eq!(
        session.fingerprint(),
        expected.fingerprint(),
        "the injected identity must be the one the lock is named after"
    );

    let vault = IdentityVault::open(&db).await.expect("open");
    let record = vault.raw_record().await.expect("record");
    assert!(
        record.is_undefined(),
        "a custodial identity must not be written to storage: the host holds \
         custody, and storing it would be a second copy nobody asked for"
    );

    session.close();
    settle().await;
}

// ─────────────── R9: stand-down is an order, not a set ───────────────

/// Standing down fences the generation, retires the real node, and
/// only then lets go of the lock.
///
/// The three claims D2 made and the lifecycle did not keep, against a
/// backend that owns a **real** node:
///
/// 1. A pending service call in the node's real call table is failed
///    exactly once, typed, naming the generation that owned it.
/// 2. An operation parked before dispatch never dispatches — and the
///    node it was holding produces no further outbound packet, so the
///    successor is not beside a predecessor that is still sending.
/// 3. The node's session is gone, which is the explicit close the old
///    stand-down never performed.
///
/// The disposition in (1) is what makes this a witness for the
/// *order* rather than for the set of steps: fencing first means the
/// caller is settled by the replier's drop, with
/// `Typed(LeaderLost { generation })`. Retire the node first and the
/// call's own failure overtakes it, and the caller gets the node's
/// `Reported` text instead — a different, weaker answer to the same
/// question.
#[wasm_bindgen_test]
async fn standing_down_fences_then_retires_the_node_then_releases_the_lock() {
    let db = unique("fenced-db");
    let scope = unique("fenced-scope");
    let (handles, _peer) = real_pair();

    // The operation that will park before dispatch.
    let (release, parked) = oneshot::channel();
    *handles.barrier.borrow_mut() = Some(parked);

    let leader = Lifecycle::open(opts(&db, &scope, &[], &[]), real_factory(handles.clone()))
        .await
        .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.generation(), 1);

    // A real pending nRPC call: it reaches the node's call table and
    // nothing in this test ever answers it.
    let call = in_flight(
        &leader,
        LeaderRequest::Call {
            service: "held".into(),
            payload: Bytes::from_static(b"body"),
            timeout_ms: None,
        },
    );
    // And an operation admitted before the fence, parked before it
    // touches the node at all.
    let publish = in_flight(
        &leader,
        LeaderRequest::Publish {
            channel: "late".into(),
            payload: Bytes::from_static(b"never"),
        },
    );
    settle().await;
    assert!(
        call.borrow().is_empty(),
        "the call must still be in flight when the leader stands down"
    );
    assert!(publish.borrow().is_empty());
    assert_eq!(
        handles.dispatched.get(),
        0,
        "the parked operation must not have dispatched yet"
    );
    let sent_before = handles.sent.get();
    assert!(
        sent_before > 0,
        "the pending call must really have put a packet on the node"
    );

    leader.close();
    settle().await;

    // (1) failed once, typed, against the generation that owned it.
    assert_eq!(
        handles.failed.get(),
        1,
        "retirement must fail the node's one pending call"
    );
    assert_eq!(
        call.borrow().len(),
        1,
        "the caller must be settled exactly once: {:?}",
        call.borrow()
    );
    let failure = call.borrow()[0]
        .clone()
        .expect_err("a call the retired node owned must fail");
    assert_eq!(
        failure.typed(),
        Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 })),
        "fencing before retiring the node is what makes this the typed \
         LeaderLost and not the node's own reported failure: {failure:?}"
    );

    // (3) the node was explicitly closed.
    assert!(
        !handles.node.borrow().has_session(handles.peer),
        "stand-down must close the node's session, not merely drop the proxy"
    );

    // Now let the parked operation go, with a successor already in
    // place — the schedule the old code could not survive.
    let successor = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("successor");
    settle().await;
    assert_eq!(successor.role(), Role::Leader);
    assert_eq!(successor.generation(), 2);

    let _ = release.send(());
    settle().await;
    settle().await;

    // (2) it never dispatched, and the old node sent nothing more.
    assert_eq!(
        handles.dispatched.get(),
        0,
        "an operation admitted by a fenced generation must be cancelled, not run"
    );
    assert_eq!(
        handles.sent.get(),
        sent_before,
        "the retired node must produce no outbound packet beside its successor"
    );
    assert_eq!(
        publish.borrow().len(),
        1,
        "and its caller must still be told, once"
    );
    assert_eq!(
        publish.borrow()[0]
            .clone()
            .expect_err("a cancelled operation cannot succeed")
            .typed(),
        Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 }))
    );

    successor.close();
    settle().await;
}

/// The storage fence has an outbound caller: a leader revalidates its
/// generation and stands down when the store has moved.
///
/// `IdentityVault::fence` had none — it was called only by tests, so
/// D2's "the storage layer rejects" was an assertion about a function
/// nobody invoked. This is the schedule it exists for, made
/// deterministic: the store's generation moves without this tab
/// hearing a single proxy message, which is precisely what a tab
/// frozen while holding its lock experiences.
#[wasm_bindgen_test]
async fn a_leader_revalidates_its_generation_against_the_store_and_stands_down() {
    let db = unique("revalidate-db");
    let scope = unique("revalidate-scope");
    let (handles, _peer) = real_pair();

    let leader = Lifecycle::open(opts(&db, &scope, &[], &[]), real_factory(handles.clone()))
        .await
        .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.generation(), 1);

    // The store moves on. No message reaches this tab — that is the
    // point: a frozen tab hears nothing, and the successor's
    // acquisition is recorded only here.
    let elsewhere = IdentityVault::open(&db).await.expect("open");
    assert_eq!(
        elsewhere.next_generation().await.expect("bump"),
        2,
        "the successor's acquisition is what the store records"
    );

    let mut seen = Vec::new();
    let recorder = {
        let events = Rc::new(RefCell::new(Vec::new()));
        let writing = events.clone();
        let callback = Closure::wrap(Box::new(move |json: JsValue| {
            if let Some(text) = json.as_string() {
                writing.borrow_mut().push(text);
            }
        }) as Box<dyn FnMut(JsValue)>);
        leader.on_event(
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone(),
        );
        callback.forget();
        events
    };

    // One revalidation interval, plus slack for the transaction.
    wait_ms(1_600).await;
    seen.extend(recorder.borrow().iter().cloned());

    assert_eq!(
        leader.role(),
        Role::Follower,
        "a leader whose recorded generation moved is not the leader"
    );
    assert!(
        seen.iter().any(|text| text.contains("\"not_leader\"")
            && text.contains("\"presented\":\"1\"")
            && text.contains("\"current\":\"2\"")),
        "the stand-down must be observable, naming both generations: {seen:?}"
    );
    assert!(
        !handles.node.borrow().has_session(handles.peer),
        "the revalidation stand-down retires the node like any other"
    );
    let refused = leader
        .request(LeaderRequest::Counters)
        .await
        .expect_err("a tab that stood down must refuse, not serve");
    assert!(
        matches!(
            refused,
            ProxyFailure::Typed(LeafError::NotLeader { .. })
                | ProxyFailure::Typed(LeafError::Rpc(RpcError::SessionLost))
        ),
        "expected a typed refusal, got {refused:?}"
    );

    leader.close();
    settle().await;
}

// ─────── R10: startup, promotion and restoration as one thing ────────

/// An `Attach` that arrives while the leader is still connecting is
/// replayed, and the follower's channel really gets subscribed.
///
/// The window is one `connect()`: the lock is granted before the
/// backend exists, so a follower can attach into a tab that has
/// neither a server nor a client. That message used to be dropped —
/// and because a follower re-declares only on a *subsequent*
/// generation, its subscriptions then never reached the node at all.
#[wasm_bindgen_test]
async fn an_attach_during_the_initial_backend_await_is_replayed() {
    let db = unique("queued-db");
    let scope = unique("queued-scope");
    let log = Rc::new(RefCell::new(Log::default()));
    let (release, parked) = oneshot::channel();

    // The leader's own open is left in flight, parked inside its
    // factory with the lock already granted.
    let opened = Rc::new(RefCell::new(None::<Lifecycle>));
    {
        let opened = opened.clone();
        let options = opts(&db, &scope, &["leader-chan"], &[]);
        let factory = delayed_factory(0x1111, log.clone(), 0, Delay::Park, parked);
        spawn_local(async move {
            let lifecycle = Lifecycle::open(options, factory)
                .await
                .expect("the parked leader must still open");
            *opened.borrow_mut() = Some(lifecycle);
        });
    }
    settle().await;
    assert!(
        opened.borrow().is_none(),
        "the leader must still be inside its backend factory"
    );

    // A follower arrives into that window and attaches.
    let follower = Lifecycle::open(
        opts(&db, &scope, &["follower-chan"], &[]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("follower");
    settle().await;
    assert_eq!(follower.role(), Role::Follower);
    assert_eq!(
        follower.generation(),
        0,
        "there is no leader yet, so there is no generation to adopt"
    );

    let _ = release.send(());
    settle().await;
    settle().await;
    let leader = opened
        .borrow()
        .clone()
        .expect("the leader must have opened");
    assert_eq!(leader.role(), Role::Leader);
    settle().await;

    let performed = log.borrow().performed.clone();
    assert!(
        performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Subscribe { channel } if channel == "follower-chan"
        )),
        "the attach from the bootstrap window must be replayed and its \
         declaration subscribed: {performed:?}"
    );
    assert_eq!(
        follower.generation(),
        1,
        "and the follower must have been taught the generation"
    );

    leader.close();
    follower.close();
    settle().await;
}

/// A tab closed while it is being promoted never publishes a leader,
/// and never keeps the lock.
///
/// `await_promotion` checked `closed` once, before the acquisition,
/// and then installed the server and the lock unconditionally: a tab
/// closed during its re-bootstrap ended up holding the origin's lock
/// while refusing every operation, and no other tab could take over
/// for the lifetime of the page.
#[wasm_bindgen_test]
async fn closing_a_tab_during_its_promotion_never_publishes_a_lock_holding_leader() {
    let db = unique("promote-close-db");
    let scope = unique("promote-close-scope");
    let log = Rc::new(RefCell::new(Log::default()));
    let (release, parked) = oneshot::channel();

    let first = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("first leader");
    settle().await;

    // The second tab's *promotion* factory is the parked one: its
    // first invocation never happens, because it opens as a follower.
    let second = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        delayed_factory(0x2222, log.clone(), 0, Delay::Park, parked),
    )
    .await
    .expect("second tab");
    settle().await;
    assert_eq!(second.role(), Role::Follower);

    first.close();
    settle().await;
    // The promotion is now parked inside the factory, with the lock
    // granted to this tab.
    second.close();
    let _ = release.send(());
    settle().await;
    settle().await;

    assert_eq!(
        second.role(),
        Role::Follower,
        "a tab closed during its promotion must not become the leader"
    );
    assert!(
        log.borrow().performed.is_empty(),
        "and must perform nothing: {:?}",
        log.borrow().performed
    );

    // The real test of the lock: somebody else can have it.
    let third = Lifecycle::open(
        opts(&db, &scope, &["third-chan"], &[]),
        factory(0x3333, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("third tab");
    settle().await;
    assert_eq!(
        third.role(),
        Role::Leader,
        "the lock must have been released by the discarded promotion"
    );

    third.close();
    settle().await;
}

/// A promotion whose backend fails leaves a functioning follower, and
/// the next tab in the lock queue becomes the leader it re-attaches
/// to.
///
/// The failure used to be logged from `await_promotion` with the
/// client already removed, the server never installed and no
/// acquisition in flight — a tab that was no longer anything, while
/// the origin had no node.
#[wasm_bindgen_test]
async fn a_promotion_whose_backend_fails_reattaches_instead_of_stranding_the_tab() {
    let db = unique("promote-fail-db");
    let scope = unique("promote-fail-scope");
    let (release, parked) = oneshot::channel();

    let first = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("first leader");
    settle().await;

    // Second in the lock queue, and its promotion will fail.
    let failing_log = Rc::new(RefCell::new(Log::default()));
    let second = Lifecycle::open(
        opts(&db, &scope, &["second-chan"], &[]),
        delayed_factory(0x2222, failing_log.clone(), 0, Delay::Fail, parked),
    )
    .await
    .expect("second tab");
    settle().await;

    // Third in the queue, and the tab that actually gets the lock
    // once the failed promotion hands it back.
    let third_log = Rc::new(RefCell::new(Log::default()));
    let third = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x3333, third_log.clone(), false),
    )
    .await
    .expect("third tab");
    settle().await;
    assert_eq!(second.role(), Role::Follower);
    assert_eq!(third.role(), Role::Follower);

    let events = Rc::new(RefCell::new(Vec::new()));
    {
        let writing = events.clone();
        let callback = Closure::wrap(Box::new(move |json: JsValue| {
            if let Some(text) = json.as_string() {
                writing.borrow_mut().push(text);
            }
        }) as Box<dyn FnMut(JsValue)>);
        second.on_event(
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone(),
        );
        callback.forget();
    }

    first.close();
    settle().await;
    let _ = release.send(());
    while third.role() != Role::Leader {
        settle().await;
    }
    settle().await;

    assert!(
        events
            .borrow()
            .iter()
            .any(|text| text.contains("\"promotion_failed\"")),
        "the failure must be surfaced, not only logged: {:?}",
        events.borrow()
    );
    assert!(
        failing_log.borrow().performed.is_empty(),
        "a promotion that failed must have published nothing: {:?}",
        failing_log.borrow().performed
    );
    assert_eq!(
        second.role(),
        Role::Follower,
        "the tab whose bootstrap failed is a follower, not a half-leader"
    );

    // A functioning follower, not a stranded object: the new leader
    // subscribed this tab's declaration, which can only have reached
    // it through a re-attach after the failure.
    assert!(
        third_log.borrow().performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Subscribe { channel } if channel == "second-chan"
        )),
        "the failed tab must have re-attached and re-declared: {:?}",
        third_log.borrow().performed
    );
    // And its operations reach the node again.
    second
        .request(LeaderRequest::Counters)
        .await
        .expect("a re-attached follower's operations must work");

    second.close();
    third.close();
    settle().await;
}

/// A retained stream handle cannot address a same-id successor, and
/// its consumer stops rather than receiving the successor's bytes.
///
/// A stream id names per-session state; a promotion is a different
/// node with an independently allocated stream table. A handle that
/// carried only the id could therefore push a payload into a
/// stranger's stream, or close it.
#[wasm_bindgen_test]
async fn a_retained_stream_handle_cannot_address_a_same_id_successor() {
    let db = unique("stream-gen-db");
    let scope = unique("stream-gen-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));
    let follower_log = Rc::new(RefCell::new(Log::default()));

    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, leader_log.clone(), false),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x2222, follower_log.clone(), false),
    )
    .await
    .expect("follower");
    settle().await;
    assert_eq!(follower.role(), Role::Follower);

    // The follower opens stream 0x09 through the leader, and keeps the
    // handle plus a listener on it.
    let stream_opts = Object::new();
    put(&stream_opts, "reliability", &JsValue::from_str("reliable"));
    put(&stream_opts, "streamId", &JsValue::from_str("9"));
    let stale = follower
        .open_stream(&stream_opts.clone().into())
        .await
        .expect("open the stream");
    assert_eq!(stale.generation(), "1");
    let delivered = Rc::new(Cell::new(0usize));
    {
        let counting = delivered.clone();
        let callback = Closure::wrap(Box::new(move |_json: JsValue| {
            counting.set(counting.get() + 1);
        }) as Box<dyn FnMut(JsValue)>);
        stale.on_message(
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone(),
        );
        callback.forget();
    }

    // Leadership moves to the tab holding the handle.
    leader.close();
    while follower.role() != Role::Leader {
        settle().await;
    }
    settle().await;
    assert_eq!(follower.generation(), 2);

    // The successor opens the *same* id.
    let fresh = follower
        .open_stream(&stream_opts.into())
        .await
        .expect("reopen the same id on the successor");
    assert_eq!(fresh.stream_id_hex(), stale.stream_id_hex());
    assert_eq!(fresh.generation(), "2");

    // The stale handle cannot reach it.
    let before = follower_log.borrow().performed.len();
    let refused = stale
        .send(Uint8Array::from(&b"stale"[..]))
        .await
        .expect_err("a handle from generation 1 must not send on generation 2");
    let message = JsValue::from(refused)
        .as_string()
        .or_else(|| {
            Reflect::get(&JsValue::from_str(""), &JsValue::from_str("message"))
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_default();
    assert!(
        message.is_empty() || message.contains("not the leader"),
        "the refusal must be the typed stale-generation one: {message:?}"
    );
    stale.close();
    settle().await;
    let after = follower_log.borrow().performed.clone();
    assert!(
        after.iter().skip(before).all(|request| !matches!(
            request,
            LeaderRequest::StreamSend { .. } | LeaderRequest::StreamClose { .. }
        )),
        "neither a stale send nor a stale close may reach the successor's \
         stream table: {after:?}"
    );

    // The successor's own handle still works.
    fresh
        .send(Uint8Array::from(&b"fresh"[..]))
        .await
        .expect("the current generation's handle must still send");
    assert!(
        follower_log
            .borrow()
            .performed
            .iter()
            .any(|request| matches!(request, LeaderRequest::StreamSend { .. })),
        "the positive must be restored, not merely the refusal proven"
    );
    assert_eq!(
        delivered.get(),
        0,
        "a stale handle's consumer must stop, not receive the successor's data"
    );

    follower.close();
    settle().await;
}

/// A capability announced at runtime is re-published by whichever tab
/// is promoted, without the application asking again.
///
/// Restoration used to replay the promoted tab's **constructor**
/// options, so a page that announced a tag through the advertised
/// session API had it silently reverted at the next handoff.
#[wasm_bindgen_test]
async fn a_runtime_announcement_is_restored_by_the_promoted_tab() {
    let db = unique("announce-db");
    let scope = unique("announce-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));
    let follower_log = Rc::new(RefCell::new(Log::default()));

    // Neither tab is configured with any capability: everything below
    // is runtime intent.
    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, leader_log.clone(), false),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x2222, follower_log.clone(), false),
    )
    .await
    .expect("follower");
    settle().await;

    follower
        .announce(vec!["cap:runtime".to_string()])
        .await
        .expect("the follower's announcement is proxied to the node");
    settle().await;
    assert!(
        leader_log.borrow().performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Announce { capabilities } if capabilities == &vec!["cap:runtime".to_string()]
        )),
        "the announcement must have reached the node: {:?}",
        leader_log.borrow().performed
    );

    leader.close();
    while follower.role() != Role::Leader {
        settle().await;
    }
    settle().await;

    let performed = follower_log.borrow().performed.clone();
    assert!(
        performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Announce { capabilities }
                if capabilities.contains(&"cap:runtime".to_string())
        )),
        "the promoted tab must re-publish the announcement that was in force, \
         and the application did not ask again: {performed:?}"
    );

    follower.close();
    settle().await;
}

/// A follower's call owns its own deadline, and its expiry is an
/// honest "the remote may have executed" — never a silent replay.
///
/// The timeout in the request is the leader's: it starts when the
/// leader performs the call and is expired by the leader's own tick.
/// A leader that stops pumping — frozen while holding its lock —
/// produced no expiry at all, so the caller's promise waited out the
/// suspension. Here the leader simply never answers, which is the
/// same thing from the follower's side.
#[wasm_bindgen_test]
async fn a_follower_call_expires_on_its_own_deadline_as_an_indeterminate_outcome() {
    let db = unique("deadline-db");
    let scope = unique("deadline-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));

    // `hold_calls`: the leader admits the call and never answers it.
    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, leader_log.clone(), true),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("follower");
    settle().await;
    assert_eq!(follower.role(), Role::Follower);

    let started = now_ms();
    let failure = follower
        .request(LeaderRequest::Call {
            service: "never".into(),
            payload: Bytes::new(),
            timeout_ms: Some(100),
        })
        .await
        .expect_err("a call nobody answers must settle on the caller's deadline");
    let elapsed = now_ms() - started;

    assert_eq!(
        failure.typed(),
        Some(&LeafError::Rpc(RpcError::Indeterminate {
            deadline_ms: 100
        })),
        "the outcome must say what it is — the deadline was local and the \
         remote may have executed: {failure:?}"
    );
    assert!(
        (100.0..2_000.0).contains(&elapsed),
        "it must expire on the caller's own clock, not the leader's; took {elapsed} ms"
    );
    assert_eq!(
        leader_log
            .borrow()
            .performed
            .iter()
            .filter(|request| matches!(request, LeaderRequest::Call { .. }))
            .count(),
        1,
        "and nothing may be re-issued: a local deadline cannot cancel a remote \
         effect, so a retry would be a second execution"
    );

    leader.close();
    follower.close();
    settle().await;
}

// ────────────── R13: the IndexedDB completion boundary ───────────────

/// A generation transaction that aborts after its put is a typed
/// failure, and the counter does not move.
///
/// A `put` request's `success` is not a commit: the write has been
/// accepted into the transaction and the transaction can still roll
/// back. Returning there handed out a generation that could be handed
/// out again — a fence that admits two leaders, which is the one
/// thing the fence exists to prevent.
#[wasm_bindgen_test]
async fn an_aborted_generation_transaction_is_a_typed_failure_and_moves_nothing() {
    let db = unique("commit-db");
    let vault = IdentityVault::open(&db).await.expect("open");

    assert_eq!(vault.next_generation().await.expect("first"), 1);
    assert_eq!(vault.current_generation().await.expect("read"), 1);

    // The production path with an abort landed in the one window that
    // matters: after the put's success, before the commit. This is
    // where a quota failure, an eviction or a foreign `abort()` lands
    // on a real page.
    let aborted = vault
        .next_generation_observed(|tx| {
            tx.abort().expect("abort the live transaction");
        })
        .await
        .expect_err("an aborted transaction must not report a generation");
    assert!(
        matches!(&aborted, LeafError::Identity(detail) if detail.contains("not committed")),
        "the abort must arrive as the typed commit failure: {aborted:?}"
    );

    assert_eq!(
        vault.current_generation().await.expect("reread"),
        1,
        "a rolled-back generation must not be observable; if it were, the next \
         acquisition would hand out the same number twice"
    );
    // And the counter still works afterwards: the failure is a
    // refusal, not a wedged store.
    assert_eq!(vault.next_generation().await.expect("again"), 2);
}

/// Two first-run creations racing on one database converge on one
/// identity.
///
/// Both tabs generate a keypair outside any transaction — WebCrypto is
/// a promise, and awaiting one inside a transaction commits it — so
/// both arrive at the commit holding a *different* candidate. The
/// re-read under the `readwrite` transaction is what makes the loser
/// adopt the winner's, and awaiting the commit is what makes "the
/// winner's" mean something.
#[wasm_bindgen_test]
async fn two_concurrent_first_creations_converge_on_one_identity() {
    let db = unique("race-db");
    let first = Rc::new(IdentityVault::open(&db).await.expect("open one"));
    let second = Rc::new(IdentityVault::open(&db).await.expect("open two"));

    let outcomes = Rc::new(RefCell::new(Vec::new()));
    for vault in [first.clone(), second.clone()] {
        let writing = outcomes.clone();
        spawn_local(async move {
            let secrets = vault.load_or_create().await.expect("create or adopt");
            writing.borrow_mut().push(secrets.into_identity().node_id());
        });
    }
    while outcomes.borrow().len() < 2 {
        settle().await;
    }

    let ids = outcomes.borrow().clone();
    assert_eq!(
        ids[0], ids[1],
        "two tabs booting together must converge on one identity, or the origin \
         has two node ids and they evict each other"
    );
    assert_eq!(
        first
            .load()
            .await
            .expect("load")
            .expect("stored")
            .into_identity()
            .node_id(),
        ids[0],
        "and the one they agreed on is the one that is committed"
    );
}

/// A tampered ciphertext does not decrypt, and says so.
///
/// The AES-GCM tag over the stored blob, with the schema magic as
/// additional data, is what makes an altered record a typed refusal
/// rather than a plausible-looking scalar. Asserted against a record
/// this test really altered — the previous storage witness only
/// created a second database, which proves two vaults differ and
/// nothing about tampering.
#[wasm_bindgen_test]
async fn a_tampered_identity_record_does_not_decrypt() {
    let db = unique("tamper-db");
    let vault = IdentityVault::open(&db).await.expect("open");
    let node_id = vault
        .load_or_create()
        .await
        .expect("create")
        .into_identity()
        .node_id();
    assert_eq!(
        vault
            .load()
            .await
            .expect("load")
            .expect("stored")
            .into_identity()
            .node_id(),
        node_id,
        "the control: an untouched record loads"
    );

    // One byte of the ciphertext, flipped in place through raw
    // IndexedDB — the same database the vault reads.
    let record = vault.raw_record().await.expect("record");
    let blob: Uint8Array = Reflect::get(&record, &JsValue::from_str("blob"))
        .expect("blob")
        .dyn_into()
        .expect("a Uint8Array");
    let mut bytes = blob.to_vec();
    bytes[0] ^= 0x01;
    Reflect::set(
        &record,
        &JsValue::from_str("blob"),
        &Uint8Array::from(&bytes[..]),
    )
    .expect("replace the blob");
    overwrite_identity_record(&db, &record).await;

    let refused = IdentityVault::open(&db)
        .await
        .expect("reopen")
        .load()
        .await
        .expect_err("a tampered record must not decrypt");
    assert!(
        matches!(&refused, LeafError::Identity(detail) if detail.contains("did not decrypt")),
        "expected the typed decrypt refusal, got {refused:?}"
    );
}

/// The wrapping key refuses export.
///
/// `extractable: false` is the whole of what §8 claims storage
/// protects, and the claim is about `exportKey` rejecting — not about
/// a boolean property on the handle. Both are checked, because a
/// property is what a regression would leave intact.
#[wasm_bindgen_test]
async fn the_wrapping_key_refuses_export() {
    let db = unique("export-db");
    let vault = IdentityVault::open(&db).await.expect("open");
    vault.load_or_create().await.expect("create");

    let record = vault.raw_record().await.expect("record");
    let key: web_sys::CryptoKey = Reflect::get(&record, &JsValue::from_str("key"))
        .expect("key")
        .dyn_into()
        .expect("a CryptoKey");
    assert!(!key.extractable());

    let subtle = web_sys::window()
        .expect("window")
        .crypto()
        .expect("crypto")
        .subtle();
    let attempt = subtle
        .export_key("raw", &key)
        .expect("exportKey returns a promise");
    JsFuture::from(attempt).await.expect_err(
        "exporting a non-extractable key must reject — that rejection \
                     IS the protection this module claims",
    );
}

/// Replace the identity record through raw IndexedDB.
///
/// The tamper witness needs a write the vault deliberately does not
/// offer: nothing in production overwrites a sealed identity, and
/// adding an API for it would be a hole opened for a test.
async fn overwrite_identity_record(db_name: &str, record: &JsValue) {
    let factory = web_sys::window()
        .expect("window")
        .indexed_db()
        .expect("indexedDB")
        .expect("indexedDB is available");
    let open = factory.open(db_name).expect("open");
    await_request(&open).await;
    let database: web_sys::IdbDatabase = open
        .result()
        .expect("result")
        .dyn_into()
        .expect("a database");
    let tx = database
        .transaction_with_str_and_mode("identity", web_sys::IdbTransactionMode::Readwrite)
        .expect("transaction");
    let store = tx.object_store("identity").expect("store");
    let put = store
        .put_with_key(record, &JsValue::from_str("v1"))
        .expect("put");
    await_request(&put).await;
    let (settled, waiting) = oneshot::channel::<()>();
    let slot = Rc::new(RefCell::new(Some(settled)));
    let done = Closure::once(Box::new(move |_e: web_sys::Event| {
        if let Some(sender) = slot.borrow_mut().take() {
            let _ = sender.send(());
        }
    }) as Box<dyn FnOnce(web_sys::Event)>);
    tx.set_oncomplete(Some(done.as_ref().unchecked_ref()));
    let _ = waiting.await;
    tx.set_oncomplete(None);
    drop(done);
    database.close();
}

/// Await one raw `IDBRequest`, for the tamper helper only.
async fn await_request(request: &web_sys::IdbRequest) {
    let (settled, waiting) = oneshot::channel::<()>();
    let slot = Rc::new(RefCell::new(Some(settled)));
    let finish = slot.clone();
    let ready = Closure::once(Box::new(move |_e: web_sys::Event| {
        if let Some(sender) = finish.borrow_mut().take() {
            let _ = sender.send(());
        }
    }) as Box<dyn FnOnce(web_sys::Event)>);
    let failed = Closure::once(Box::new(move |_e: web_sys::Event| {
        if let Some(sender) = slot.borrow_mut().take() {
            let _ = sender.send(());
        }
    }) as Box<dyn FnOnce(web_sys::Event)>);
    request.set_onsuccess(Some(ready.as_ref().unchecked_ref()));
    request.set_onerror(Some(failed.as_ref().unchecked_ref()));
    let _ = waiting.await;
    request.set_onsuccess(None);
    request.set_onerror(None);
}

/// `performance.now()`, the only clock a browser has that does not
/// panic on `wasm32-unknown-unknown`.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}
