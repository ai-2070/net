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
//! WASM_BINDGEN_TEST_TIMEOUT=120 \
//! cargo test --target wasm32-unknown-unknown --test wasm_leader
//! ```
//!
//! `WASM_BINDGEN_TEST_TIMEOUT` is required, not advisory: the
//! runner's default is 20 seconds per test and
//! `a_follower_call_without_a_timeout_expires_on_the_leafs_default_deadline`
//! waits out the leaf's own 30-second call default. Without it that
//! test kills the driver rather than failing an assertion.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use bytes::Bytes;
use futures_channel::oneshot;
use js_sys::{Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{RtcPeerConnection, RtcPeerConnectionState};

use net_leaf::bootstrap::gloo_timer_sleep;
use net_leaf::error::{LeafError, RpcError};
use net_leaf::identity::{IdentitySecrets, IDENTITY_BLOB_MAGIC};
use net_leaf::leader::{
    envelope, GenerationLease, LeaderBackend, LeaderRequest, ProxyFailure, ProxySide, ProxyValue,
    Replier, ORG_ENVELOPE_ADMITTED, ORG_ENVELOPE_END, ORG_ENVELOPE_ITEM, ORG_ENVELOPE_RETIRED,
};
use net_leaf::leader_session::{
    spawn_fenced, BackendFactory, EventSink, Lifecycle, MeshSession, OpRegistry, Role,
};
use net_leaf::rpc_wire::RpcStatus;
use net_leaf::rtc::RtcLeafTransport;
use net_leaf::storage::IdentityVault;
use net_leaf::stream::Reliability;
use net_leaf::stream_ownership::{answer_stream_request, StreamBackend, StreamOwnership};
use net_leaf::{LeafIdentity, StreamHandle};

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
    /// The event sink the factory was handed — the production surface
    /// a leader's node events flow out through. Captured so a test
    /// can drive an event through the real path instead of asserting
    /// against a source that never fires.
    sink: Option<EventSink>,
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

    fn perform(&mut self, _from: ProxySide, request: LeaderRequest, reply: Replier) {
        self.log.borrow_mut().performed.push(request.clone());
        if self.hold_calls && matches!(request, LeaderRequest::Call { .. }) {
            self.log.borrow_mut().held.push(reply);
            return;
        }
        match request {
            LeaderRequest::Call { .. } => reply.bytes(Bytes::from_static(b"pong")),
            LeaderRequest::Query { .. } | LeaderRequest::Counters => reply.text("[]".into()),
            LeaderRequest::IsEnrolled => reply.flag(true),
            LeaderRequest::StreamOpen {
                stream_id, peer, ..
            } => {
                let id = stream_id.unwrap_or(9);
                reply.stream(id, id, peer.unwrap_or(0xfeed), 1)
            }
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
    Rc::new(move |_opts, sink, _lease| {
        // Captured before the first await: the sink is the seam a
        // test drives an event through.
        log.borrow_mut().sink = Some(sink);
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

/// What a parked bootstrap did, as seen from inside the factory.
///
/// The discriminator L1 needs. A factory barrier alone cannot tell
/// "the bootstrap was cancelled" from "the bootstrap finished late":
/// both leave the log empty until the barrier is released. So the
/// factory future itself records whether it was **dropped while
/// parked**, which is only possible if something owned and cancelled
/// it.
#[derive(Default)]
struct BootstrapMarks {
    entered: Cell<usize>,
    resumed: Cell<usize>,
    abandoned: Cell<usize>,
}

/// Armed while a factory future is parked; counts an abandonment if
/// that future is dropped before it resumed.
struct Abandonment {
    marks: Rc<BootstrapMarks>,
    armed: Cell<bool>,
}

impl Drop for Abandonment {
    fn drop(&mut self) {
        if self.armed.get() {
            self.marks.abandoned.set(self.marks.abandoned.get() + 1);
        }
    }
}

/// A factory whose `nth` invocation parks on `barrier` and reports,
/// through `marks`, whether it was cancelled there.
fn observed_factory(
    node_id: u64,
    log: Rc<RefCell<Log>>,
    nth: usize,
    barrier: oneshot::Receiver<()>,
    marks: Rc<BootstrapMarks>,
) -> BackendFactory {
    let barrier = Rc::new(RefCell::new(Some(barrier)));
    let invocations = Rc::new(Cell::new(0usize));
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        let barrier = barrier.clone();
        let marks = marks.clone();
        let invocation = invocations.get();
        invocations.set(invocation + 1);
        Box::pin(async move {
            if invocation == nth {
                marks.entered.set(marks.entered.get() + 1);
                let abandonment = Abandonment {
                    marks: marks.clone(),
                    armed: Cell::new(true),
                };
                let parked = barrier.borrow_mut().take();
                if let Some(parked) = parked {
                    let _ = parked.await;
                }
                abandonment.armed.set(false);
                marks.resumed.set(marks.resumed.get() + 1);
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

/// A factory that parks on `barrier`, then runs `hook` inside the
/// very poll that resumes it, and only then completes with a real
/// backend.
///
/// L1's seam. The promotion-close claim has exactly one
/// discriminating schedule: the close lands while the factory is
/// COMPLETING, so a backend exists and the only thing that can
/// discard it is the post-await `closed` check. `marks` reports the
/// same entered/resumed/abandoned triple `observed_factory` does, so
/// the test can show the future ran past its barrier and was never
/// dropped there — the discard was a refusal, not a cancellation.
fn completing_factory(
    node_id: u64,
    log: Rc<RefCell<Log>>,
    barrier: oneshot::Receiver<()>,
    marks: Rc<BootstrapMarks>,
    hook: Rc<dyn Fn()>,
) -> BackendFactory {
    let barrier = Rc::new(RefCell::new(Some(barrier)));
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        let barrier = barrier.clone();
        let marks = marks.clone();
        let hook = hook.clone();
        Box::pin(async move {
            marks.entered.set(marks.entered.get() + 1);
            let abandonment = Abandonment {
                marks: marks.clone(),
                armed: Cell::new(true),
            };
            let parked = barrier.borrow_mut().take();
            if let Some(parked) = parked {
                let _ = parked.await;
            }
            abandonment.armed.set(false);
            marks.resumed.set(marks.resumed.get() + 1);
            // Inside this very poll: whatever the hook does lands in
            // the same turn as the factory's completion.
            hook();
            let backend: Box<dyn LeaderBackend> = Box::new(TestBackend {
                node_id,
                log,
                hold_calls: false,
            });
            Ok(backend)
        })
    })
}

/// What a bootstrap that really built RTC state left behind.
///
/// A **constructor observer**, not a replacement transport: the
/// factory below drives the production
/// [`RtcLeafTransport::create_offer`], which is the call
/// `wasm::LeafNode::connect` makes before its first suspension, and
/// keeps the `RTCPeerConnection` the browser actually created. Every
/// state read off it afterwards is the engine's own.
#[derive(Default)]
struct RtcAttempt {
    /// The real connection the parked bootstrap created.
    connection: RefCell<Option<RtcPeerConnection>>,
    /// Whether the browser produced a real offer with SDP for it.
    offered: Cell<bool>,
    /// That connection's state, sampled by the **successor's**
    /// factory — i.e. at the moment the origin's lock was granted
    /// onwards. This is the ordering oracle: a successor can only be
    /// running because the cancelled bootstrap released the lock, so
    /// a reading of `closed` here is a close that happened *before*
    /// the release.
    at_successor: Cell<Option<RtcPeerConnectionState>>,
}

/// The peer id the parked bootstrap offers to. Any id: the offer is
/// local, and nothing answers it.
const RTC_PEER: u64 = 0x00AB_CDEF_1234_5678;

/// A factory whose `nth` invocation creates a **real** production RTC
/// attempt — an `RTCPeerConnection` with its Net DataChannel and a
/// local offer — and then parks, exactly where a real `connect()`
/// parks while it waits for an answer, a channel, Noise and
/// enrollment.
///
/// The transport is a local of the factory future, as the real one is
/// a local of `connect`'s. Cancelling the bootstrap drops that
/// future, and what happens to the browser's connection then is the
/// property under test.
fn rtc_parking_factory(
    node_id: u64,
    log: Rc<RefCell<Log>>,
    nth: usize,
    barrier: oneshot::Receiver<()>,
    attempt: Rc<RtcAttempt>,
) -> BackendFactory {
    let barrier = Rc::new(RefCell::new(Some(barrier)));
    let invocations = Rc::new(Cell::new(0usize));
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        let barrier = barrier.clone();
        let attempt = attempt.clone();
        let invocation = invocations.get();
        invocations.set(invocation + 1);
        Box::pin(async move {
            if invocation == nth {
                let transport = RtcLeafTransport::new(Rc::new(|_, _| {}));
                let offer = transport
                    .create_offer(RTC_PEER, &[])
                    .await
                    .map_err(|error| LeafError::Session(format!("create_offer: {error}")))?;
                attempt.offered.set(!offer.0.is_empty());
                *attempt.connection.borrow_mut() = transport.peer_connection(RTC_PEER);
                let parked = barrier.borrow_mut().take();
                if let Some(parked) = parked {
                    let _ = parked.await;
                }
                // Reached only by a bootstrap that was NOT cancelled;
                // `transport` is dropped here either way.
                drop(transport);
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

/// A factory that samples the predecessor's connection state on the
/// way in, then behaves like any other.
fn sampling_factory(
    node_id: u64,
    log: Rc<RefCell<Log>>,
    attempt: Rc<RtcAttempt>,
) -> BackendFactory {
    Rc::new(move |_opts, _sink, _lease| {
        let log = log.clone();
        attempt.at_successor.set(
            attempt
                .connection
                .borrow()
                .as_ref()
                .map(RtcPeerConnection::connection_state),
        );
        Box::pin(async move {
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
    /// And the packets themselves, so the **peer** can be the oracle
    /// rather than the sender's own counter: "the retired node went
    /// quiet" is a claim about what arrived somewhere else.
    wire: Rc<RefCell<Vec<(u64, Bytes)>>>,
    /// How many pending calls the retirement failed.
    failed: Rc<Cell<usize>>,
    /// Released by the test: the operation that parks before it
    /// dispatches waits on this.
    barrier: Rc<RefCell<Option<oneshot::Receiver<()>>>>,
    /// Operations that dispatched after being parked.
    dispatched: Rc<Cell<usize>>,
    /// The **production** event sink this backend was handed by the
    /// lifecycle, so a synchronous arm can deliver a real node event
    /// exactly where production's `StreamSend` delivers one.
    sink: Rc<RefCell<Option<EventSink>>>,
    /// The streams this backend owns.
    ///
    /// **The production type**, not a second copy of the addressing:
    /// `net_leaf::stream_ownership::StreamOwnership` is what the real
    /// `NodeBackend` holds too, so a change to how an open is
    /// addressed is visible to the witnesses below. A fixture with its
    /// own table is a fixture that cannot see the mechanism change.
    streams: Rc<StreamOwnership<StreamHandle>>,
    /// How many reliability sweeps a synchronous send's pump runs,
    /// and how far each one moves the node's clock argument. Zero for
    /// every test but the one whose subject is a retransmit budget
    /// expiring inside another stream's send.
    sweeps: Rc<Cell<u32>>,
    /// A **production** [`RtcLeafTransport`] holding a real
    /// `RTCPeerConnection`, installed by [`attach_real_rtc`].
    ///
    /// So that retirement's resource effect is observed where it
    /// actually happens — in the browser — rather than only in this
    /// file's bookkeeping. `wasm::LeafNode::retire` ends in
    /// `transport.close_all()`, and this is that call's real
    /// subject.
    transport: Rc<RefCell<Option<RtcLeafTransport>>>,
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

    fn perform(&mut self, _from: ProxySide, request: LeaderRequest, reply: Replier) {
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
                    let produced = node.take_outbound();
                    handles.sent.set(handles.sent.get() + produced.len());
                    handles
                        .wire
                        .borrow_mut()
                        .extend(produced.into_iter().map(|out| (out.peer, out.packet)));
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
                        let produced = node.take_outbound();
                        handles.sent.set(handles.sent.get() + produced.len());
                        handles
                            .wire
                            .borrow_mut()
                            .extend(produced.into_iter().map(|out| (out.peer, out.packet)));
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
                    let produced = node.take_outbound();
                    handles.sent.set(handles.sent.get() + produced.len());
                    handles
                        .wire
                        .borrow_mut()
                        .extend(produced.into_iter().map(|out| (out.peer, out.packet)));
                    outcome
                };
                match outcome {
                    Ok(_) => reply.bytes(Bytes::new()),
                    Err(error) => reply.fail(ProxyFailure::Reported(error.to_string())),
                }
            }
            LeaderRequest::Counters | LeaderRequest::Query { .. } => reply.text("[]".into()),
            LeaderRequest::IsEnrolled => reply.flag(true),
            // The SAME shared dispatch production uses. This fixture
            // supplies only the three type-specific actions
            // (`StreamBackend` below), so it cannot reconstruct the
            // reply — which is exactly how the production identity
            // wiring stayed untested while only `adopt` was shared.
            other @ (LeaderRequest::StreamOpen { .. }
            | LeaderRequest::StreamSend { .. }
            | LeaderRequest::StreamClose { .. }) => {
                let owned = handles.streams.clone();
                // The match above admits only the three stream
                // variants, so the hand-back is unreachable here.
                // Asserted rather than ignored: a silent `let _`
                // would drop a live replier if that ever changed.
                // Evaluated unconditionally: a `debug_assert!`
                // around the CALL would remove the dispatch from a
                // release build entirely.
                let unhandled = answer_stream_request(&owned, self, other, reply);
                debug_assert!(
                    unhandled.is_none(),
                    "the stream arms prefilter, so nothing is handed back"
                );
                drop(unhandled);
            }
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
        drop(node);
        // And the resource half, which is the last thing
        // `wasm::LeafNode::retire` does: the node's RTC links go with
        // it. Real here, not recorded — the connection this closes is
        // the browser's, and its state afterwards is the engine's own
        // answer rather than this file's.
        if let Some(transport) = self.handles.transport.borrow().as_ref() {
            transport.close_all();
        }
        self.handles.failed.set(failed);
        failed
    }
}

/// Collect this session's event JSON, so a test can assert on what a
/// page would have observed.
fn record_events(lifecycle: &Lifecycle) -> Rc<RefCell<Vec<String>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let writing = seen.clone();
    let callback = Closure::wrap(Box::new(move |json: JsValue| {
        if let Some(text) = json.as_string() {
            writing.borrow_mut().push(text);
        }
    }) as Box<dyn FnMut(JsValue)>);
    lifecycle.on_event(
        callback
            .as_ref()
            .unchecked_ref::<js_sys::Function>()
            .clone(),
    );
    callback.forget();
    seen
}

/// The capability list of the most recent `Announce` a backend
/// performed.
///
/// The *last* one, deliberately: what the network currently believes
/// is the document published most recently, and an earlier correct
/// announcement followed by a narrower one is precisely the defect
/// reconciliation exists to prevent.
fn last_announcement(log: &Rc<RefCell<Log>>) -> Option<Vec<String>> {
    log.borrow()
        .performed
        .iter()
        .rev()
        .find_map(|request| match request {
            LeaderRequest::Announce { capabilities } => Some(capabilities.clone()),
            _ => None,
        })
}

/// Hand every packet the leader's node produced to the real peer, and
/// say how many arrived.
///
/// The peer is the oracle. "The retired node sent nothing more" read
/// off the sender's own queue cannot tell "produced nothing" from
/// "produced something that was never charged"; the peer's
/// authenticated ingress can.
fn deliver_to_peer(handles: &RealNode, peer: &mut net_leaf::LeafNode) -> usize {
    let packets = core::mem::take(&mut *handles.wire.borrow_mut());
    let count = packets.len();
    let now = net_leaf::clock::now();
    for (to, packet) in packets {
        assert_eq!(to, peer.node_id(), "the leader's packets are for its peer");
        peer.on_datagram(handles.node.borrow().node_id(), packet, now);
    }
    peer.take_outbound();
    peer.drain_events();
    count
}

/// The peer's own authenticated-ingress count, read through the
/// counter JSON the surface publishes.
fn peer_packets_in(peer: &net_leaf::LeafNode) -> u64 {
    let json: serde_json::Value =
        serde_json::from_str(&peer.counters().to_json()).expect("the counters are JSON");
    json["packets_in"]
        .as_str()
        .expect("u64s cross as decimal strings")
        .parse()
        .expect("decimal")
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
            wire: Rc::new(RefCell::new(Vec::new())),
            failed: Rc::new(Cell::new(0)),
            barrier: Rc::new(RefCell::new(None)),
            dispatched: Rc::new(Cell::new(0)),
            sink: Rc::new(RefCell::new(None)),
            streams: Rc::new(StreamOwnership::default()),
            sweeps: Rc::new(Cell::new(0)),
            transport: Rc::new(RefCell::new(None)),
        },
        peer,
    )
}

/// A second authenticated peer session on the same leader node.
///
/// Returns the peer's node id. Two sessions on one node is what makes
/// "one label, two peers, one wire id" reachable at all — the shape
/// [`crate::node::LeafEvent::StreamData`] documents as ordinary.
fn add_peer(handles: &RealNode) -> (net_leaf::LeafNode, u64) {
    let mut peer = net_leaf::LeafNode::new(LeafIdentity::generate().expect("identity"), 11);
    let mut psk = [0u8; 32];
    getrandom::fill(&mut psk).expect("browser CSPRNG");
    let peer_id = peer.node_id();
    let one = handles
        .node
        .borrow_mut()
        .begin_handshake(peer_id, &psk, peer.identity().noise().public_key(), 1)
        .expect("msg1");
    let leader_id = handles.node.borrow().node_id();
    let two = peer
        .accept_handshake(leader_id, &psk, &one, 1)
        .expect("msg2");
    handles
        .node
        .borrow_mut()
        .complete_handshake(peer_id, &two)
        .expect("session");
    handles.node.borrow_mut().drain_events();
    peer.drain_events();
    handles.node.borrow_mut().take_outbound();
    (peer, peer_id)
}

impl StreamBackend for RealNodeBackend {
    type Stream = StreamHandle;

    fn open(
        &self,
        label: &str,
        reliability: Reliability,
        stream_id: Option<u64>,
        channel_hash: Option<u16>,
        peer: Option<u64>,
    ) -> Result<Self::Stream, ProxyFailure> {
        self.handles
            .node
            .borrow_mut()
            .open_stream(
                // The request's peer when it names one, and this
                // double's own peer otherwise — the same "absent means
                // the anchor" rule the real backend applies.
                peer.unwrap_or(self.handles.peer),
                label,
                reliability,
                stream_id,
                channel_hash,
            )
            .map_err(|error| ProxyFailure::Reported(error.to_string()))
    }

    /// **Synchronous, and that is the whole point.**
    ///
    /// Production's `NodeBackend::send` calls a real
    /// `wasm::LeafStream::send`, which pumps the node, drives
    /// reliability and *dispatches every event that produced* before
    /// returning — all inside the `Shared.server` borrow its caller
    /// holds. So this pumps and delivers to the production sink in the
    /// same frame rather than spawning.
    fn send(&self, stream: &Self::Stream, payload: &[u8]) -> Result<(), ProxyFailure> {
        let handles = &self.handles;
        let sink = handles.sink.borrow().clone();
        let (outcome, events) = {
            let mut node = handles.node.borrow_mut();
            let outcome = node.stream_send(*stream, payload);
            // `sweeps` is how many ticks this pump runs; the elapsed
            // time is real, because `get_timed_out` reads the clock
            // itself rather than taking the tick's argument.
            for _ in 0..handles.sweeps.get() {
                node.tick(net_leaf::clock::now());
            }
            let produced = node.take_outbound();
            handles.sent.set(handles.sent.get() + produced.len());
            handles
                .wire
                .borrow_mut()
                .extend(produced.into_iter().map(|out| (out.peer, out.packet)));
            (outcome, node.drain_events())
        };
        if let Some(sink) = sink {
            for event in &events {
                sink(&event.to_json());
            }
        }
        outcome.map_err(|error| ProxyFailure::Reported(error.to_string()))
    }

    fn close(&self, stream: Self::Stream) {
        let mut node = self.handles.node.borrow_mut();
        let _ = node.close_stream(stream);
        node.take_outbound();
    }
}

fn real_factory(handles: RealNode) -> BackendFactory {
    Rc::new(move |_opts, sink: EventSink, lease: GenerationLease| {
        let handles = handles.clone();
        let node_id = handles.node.borrow().node_id();
        // The lifecycle's own sink, kept so the synchronous
        // `StreamSend` arm delivers through the production path
        // rather than through a test channel.
        *handles.sink.borrow_mut() = Some(sink);
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

/// Give `handles` a **production** RTC transport carrying one real
/// `RTCPeerConnection`, and hand back that connection.
///
/// The same production call `wasm::LeafNode::connect` makes —
/// `create_offer` creates the connection, creates the Net
/// DataChannel and produces a local offer — so what a later
/// retirement or cancellation closes is a connection the browser
/// really built, and its state afterwards is the engine's answer.
async fn attach_real_rtc(handles: &RealNode) -> RtcPeerConnection {
    let transport = RtcLeafTransport::new(Rc::new(|_, _| {}));
    let offer = transport
        .create_offer(RTC_PEER, &[])
        .await
        .expect("the browser must create a real offer");
    assert!(
        !offer.0.is_empty(),
        "an offer with no SDP is not a real attempt"
    );
    let connection = transport
        .peer_connection(RTC_PEER)
        .expect("the transport keeps the link it created");
    assert_ne!(
        connection.connection_state(),
        RtcPeerConnectionState::Closed,
        "the premise: the connection is live before anything retires it"
    );
    *handles.transport.borrow_mut() = Some(transport);
    connection
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
    assert_eq!(
        vault
            .fence(5)
            .await
            .expect_err("nor a generation from the future"),
        LeafError::NotLeader {
            presented: 5,
            current: Some(4)
        },
        "a generation from the future is refused by the same typed fence, \
         naming the one in force"
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

    // The premise the "not resurrected" claim below needs: the
    // promoted tab DID hold a stream of its own before the handoff.
    // With none ever opened, `!matches!(StreamOpen)` is true before
    // and after any restoration logic and the assertion cannot fail.
    let stream_opts = Object::new();
    put(&stream_opts, "label", &JsValue::from_str("app"));
    put(&stream_opts, "streamId", &JsValue::from_str("5"));
    follower
        .open_stream(&stream_opts.clone().into())
        .await
        .expect("the promoted tab holds a stream of its own before the handoff");
    assert!(
        leader_log
            .borrow()
            .performed
            .iter()
            .any(|request| matches!(request, LeaderRequest::StreamOpen { .. })),
        "the premise: the stream open really happened"
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
    // The successor, queued for the lock the resumed tab still holds.
    // The point of the test is what happens to the value the first
    // tab still holds after the store has moved on without it — the
    // exact schedule of a tab frozen while holding its lock. (The
    // previous takeover was `suspended.close()`, which made the final
    // refusal leg's `Session(_)` arm a foregone conclusion: a closed
    // session answers `Session(_)` whatever the fence does.)
    let successor = Lifecycle::open(
        opts(&db, &scope, &["chan"], &[]),
        factory(0x2222, log.clone(), false),
    )
    .await
    .expect("successor");
    settle().await;
    assert_eq!(successor.role(), Role::Follower);

    // Enforcer three: storage refuses the generation the resumed tab
    // still believes in, and names the one in force. The store moves
    // on first — no proxy message reaches this tab, which is the
    // point — and the fence's own caller (`guard_lease`) is what
    // stands it down and hands the lock on.
    let vault = IdentityVault::open(&db).await.expect("open");
    assert_eq!(
        vault.next_generation().await.expect("the store moved"),
        2,
        "the store's generation moves without this tab hearing anything"
    );
    // One revalidation interval, plus slack.
    wait_ms(1_600).await;
    settle().await;
    while successor.role() != Role::Leader {
        settle().await;
    }
    settle().await;
    assert_eq!(
        successor.generation(),
        stale + 2,
        "the store's own move plus the successor's acquisition"
    );
    let refused = vault
        .fence(stale)
        .await
        .expect_err("the resumed tab's generation must be refused");
    assert_eq!(
        refused,
        LeafError::NotLeader {
            presented: stale,
            current: Some(successor.generation())
        }
    );
    vault
        .fence(successor.generation())
        .await
        .expect("the successor's generation is the one in force");

    // Enforcer two: the successor refuses a request stamped with the
    // stale generation, and never performs it. The positive control
    // first: the same body from the same unregistered sender stamped
    // with the generation IN FORCE must be served — without it, a
    // server that dropped unknown senders (or ignored every proxied
    // request) would keep the refusal leg below green.
    let post = |generation: u64, service: &str| {
        net_leaf::leader::ProxyEnvelope {
            generation,
            from: net_leaf::leader::ProxySide::Follower(0xFEED),
            body: net_leaf::leader::ProxyBody::Request {
                correlation: 1,
                request: Box::new(LeaderRequest::Call {
                    service: service.into(),
                    payload: Bytes::new(),
                    timeout_ms: None,
                }),
            },
        }
        .to_json()
    };
    let channel = web_sys::BroadcastChannel::new(&scope).expect("channel");
    let before = log.borrow().performed.len();
    channel
        .post_message(&JsValue::from_str(&post(successor.generation(), "control")))
        .expect("post");
    settle().await;
    settle().await;
    assert_eq!(
        log.borrow().performed.len(),
        before + 1,
        "the control: the same sender stamped with the generation in force is served"
    );
    let before = log.borrow().performed.len();
    channel
        .post_message(&JsValue::from_str(&post(stale, "stale")))
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
    // rather than served — by the fence that stood it down, not by a
    // closed guard. The old disjunction accepted `Session(_)`, which
    // the old `close()`-based takeover guaranteed: the leg could not
    // fail on the fence axis at all. `suspended` is OPEN here; it
    // stopped serving because the store moved past its generation.
    let before = log.borrow().performed.len();
    let outcome = suspended
        .request(LeaderRequest::Counters)
        .await
        .expect_err("a superseded session must refuse");
    assert!(
        matches!(outcome, ProxyFailure::Typed(LeafError::NotLeader { .. })),
        "expected the typed fence refusal, got {outcome:?}"
    );
    assert_eq!(
        log.borrow().performed.len(),
        before,
        "and nothing may be served while refusing"
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

/// An explicitly retired leader goes quiet **at its peer**, while the
/// tab that owned it stays alive and answers for itself.
///
/// The strengthened schedule, and the four things the witness above
/// does not discriminate:
///
/// 1. **The old owner stays alive.** Nothing here drops the
///    predecessor `Lifecycle`. A browser handoff that destroys the
///    page cannot tell "retirement stopped the old node" from "the
///    process that owned it died", and explicit `close()` is exactly
///    the case where the owner is still there.
/// 2. **The failure is named exactly.** Not "some typed error": the
///    `LeaderLost` variant *and* the generation that owned the call,
///    because a predicate that accepts any kind would pass on a
///    stand-down that reported the node's own text and lost the
///    generation.
/// 3. **The parked work is released after recovery**, with the
///    successor already installed — the interleaving where a
///    cancelled operation would be running beside a live successor.
/// 4. **The count is the peer's.** "It sent nothing more" is read off
///    the real peer node's authenticated ingress after recovery, not
///    off the sender's own `take_outbound`: a sender-side counter
///    cannot distinguish "produced nothing" from "produced something
///    nobody charged".
/// 5. **The resource is the browser's.** Retirement's last step is
///    `transport.close_all()`, so this backend owns a **production**
///    [`RtcLeafTransport`] carrying a real `RTCPeerConnection`, and
///    the assertion after retirement is the engine's own
///    `connectionState`. A transport whose map was merely emptied,
///    or whose link was retained by the low-water closure it
///    installed, leaves that connection open — which is the
///    difference between a retirement and a forgotten one.
///
/// Still absent, and named rather than implied: production
/// `NodeBackend::shutdown` itself, which needs a `wasm::LeafNode`
/// from a real `connect()` and therefore an anchor. This file's
/// premise is that there is none; the installed-node chain is
/// covered against a real anchor by the two-tab witness in
/// `net/crates/net/tests/rtc_browser/`. What is production here is
/// the node, the call table, the transport and the connection.
#[wasm_bindgen_test]
async fn an_explicitly_retired_leader_goes_quiet_at_its_peer_while_its_page_stays_alive() {
    let db = unique("quiet-db");
    let scope = unique("quiet-scope");
    let (handles, mut peer) = real_pair();

    let (release, parked) = oneshot::channel();
    *handles.barrier.borrow_mut() = Some(parked);
    let connection = attach_real_rtc(&handles).await;

    let leader = Lifecycle::open(opts(&db, &scope, &[], &[]), real_factory(handles.clone()))
        .await
        .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.generation(), 1);

    // Real production work: a call in the node's real call table, and
    // an operation admitted under generation 1 and parked before it
    // touches the node at all.
    let call = in_flight(
        &leader,
        LeaderRequest::Call {
            service: "held".into(),
            payload: Bytes::from_static(b"body"),
            timeout_ms: None,
        },
    );
    let publish = in_flight(
        &leader,
        LeaderRequest::Publish {
            channel: "late".into(),
            payload: Bytes::from_static(b"never"),
        },
    );
    settle().await;
    assert!(call.borrow().is_empty());
    assert!(publish.borrow().is_empty());
    assert_eq!(handles.dispatched.get(), 0);

    // The call's packet really reaches the peer, which authenticates
    // it. This is the baseline the silence is measured against: an
    // assertion that nothing arrives is worth nothing unless
    // something does first.
    let delivered = deliver_to_peer(&handles, &mut peer);
    assert!(
        delivered > 0,
        "the pending call must have put an authenticated packet on the peer"
    );
    let executed_before = peer_packets_in(&peer);
    assert_eq!(executed_before, delivered as u64);

    // Explicit retirement. The page — and this `Lifecycle` — stay.
    leader.close();
    settle().await;

    assert_eq!(
        call.borrow().len(),
        1,
        "the caller must be settled exactly once: {:?}",
        call.borrow()
    );
    assert_eq!(
        call.borrow()[0]
            .clone()
            .expect_err("a call the retired node owned must fail")
            .typed(),
        Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 })),
        "the exact typed failure and the exact generation, not merely some kind"
    );
    assert!(
        !handles.node.borrow().has_session(handles.peer),
        "retirement must close the node's session"
    );
    assert_eq!(
        connection.connection_state(),
        RtcPeerConnectionState::Closed,
        "and it must close the RTC connection the node held — the engine's own \
         state, not a count of links the transport forgot"
    );
    assert!(
        handles
            .transport
            .borrow()
            .as_ref()
            .and_then(|transport| transport.peer_connection(RTC_PEER))
            .is_none(),
        "the retired link must be out of the transport as well"
    );

    // Recovery: a successor is installed and leading.
    let successor_log = Rc::new(RefCell::new(Log::default()));
    let successor = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x2222, successor_log.clone(), false),
    )
    .await
    .expect("successor");
    settle().await;
    assert_eq!(successor.role(), Role::Leader);
    assert_eq!(successor.generation(), 2);

    // Only now is the delayed work released.
    let _ = release.send(());
    settle().await;
    settle().await;

    assert_eq!(
        handles.dispatched.get(),
        0,
        "an operation admitted by the retired generation must be cancelled, not run"
    );
    assert_eq!(
        deliver_to_peer(&handles, &mut peer),
        0,
        "and the retired node must have produced no further packet at all"
    );
    assert_eq!(
        peer_packets_in(&peer),
        executed_before,
        "the peer's own execution count must not move after the predecessor was \
         retired: this is the silence, observed where it matters"
    );
    assert_eq!(
        publish.borrow()[0]
            .clone()
            .expect_err("a cancelled operation cannot succeed")
            .typed(),
        Some(&LeafError::Rpc(RpcError::LeaderLost { generation: 1 }))
    );

    // The predecessor is still here, and says so: a retired owner
    // refuses rather than disappears.
    // The typed refusal, not its prose: a reworded-but-correct
    // message must not break this witness, and a differently-phrased
    // wrong refusal must not pass it.
    let refused = leader.request(LeaderRequest::Counters).await;
    assert!(
        matches!(
            refused.expect_err("a closed session refuses").typed(),
            Some(&LeafError::Session(_))
        ),
        "a retired owner refuses with the typed session refusal"
    );
    assert_eq!(
        leader.generation(),
        1,
        "and it still names its own generation"
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
    let recorder = record_events(&leader);

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
    let marks = Rc::new(BootstrapMarks::default());

    // The hook the completing factory runs in its own completion
    // turn: close this tab. Bridged through a cell because the
    // session does not exist when the factory is built.
    let hook_target: Rc<RefCell<Option<Lifecycle>>> = Rc::new(RefCell::new(None));
    let closing = hook_target.clone();
    let hook = Rc::new(move || {
        if let Some(session) = closing.borrow().as_ref() {
            session.close();
        }
    });

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
        completing_factory(0x2222, log.clone(), parked, marks.clone(), hook),
    )
    .await
    .expect("second tab");
    *hook_target.borrow_mut() = Some(second.clone());
    settle().await;
    assert_eq!(second.role(), Role::Follower);

    first.close();
    settle().await;
    // The promotion is now parked inside the factory, with the lock
    // granted to this tab. Releasing the barrier lets the factory run
    // to completion — and the hook closes this tab in that same turn,
    // so a backend EXISTS when the close lands. The only thing that
    // can discard it is the post-await `closed` check: deleting that
    // check leaves the sibling's pure-cancellation mechanism to
    // explain this same outcome and reintroduces a lock-holding
    // leader that refuses every operation.
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
    // The mechanism, not just the outcome: the factory ran past its
    // barrier and produced a backend (resumed), and was never dropped
    // while parked (not abandoned). What happened to that backend is
    // therefore the post-await refusal, and nothing else.
    assert_eq!(marks.entered.get(), 1, "the factory was reached");
    assert_eq!(
        marks.resumed.get(),
        1,
        "the factory future RESUMED and produced a backend — the claim is that \
         its result was discarded afterwards, not that it never ran"
    );
    assert_eq!(
        marks.abandoned.get(),
        0,
        "and it must not be cancelled: the discard is a refusal, not a cancellation"
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

/// Closing a tab whose promotion is still parked **cancels** that
/// bootstrap and frees the origin's lock, without waiting for the
/// factory.
///
/// The difference from the test above is the whole of L1. That one
/// releases the barrier and *then* asks whether a third tab can have
/// the lock, so it establishes the post-await publication refusal:
/// the bootstrap completed, and its result was discarded. It says
/// nothing about a bootstrap that has not completed — and a real one
/// is a `connect()`: an attach, an offer, a trickle, a Noise
/// handshake, an enrollment. The granted lock lived in
/// `take_leadership`'s own suspended stack, so `close()` found no
/// server to retire, no lease to revoke and no lock to release, and
/// could not cancel the factory either. The origin's only lock
/// therefore stayed held until that connect finished on its own,
/// while the work it was doing went on for a session the page had
/// already closed.
///
/// So here the barrier is **never** released before the successor is
/// required to be leading, and the factory future itself reports
/// whether it was dropped while parked — which is the one thing a
/// late completion cannot look like.
#[wasm_bindgen_test]
async fn closing_a_promoting_tab_cancels_its_parked_bootstrap_and_frees_the_lock() {
    let db = unique("cancel-db");
    let scope = unique("cancel-scope");
    let log = Rc::new(RefCell::new(Log::default()));
    let marks = Rc::new(BootstrapMarks::default());
    let (release, parked) = oneshot::channel();

    let first = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("first leader");
    settle().await;

    let second = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        observed_factory(0x2222, log.clone(), 0, parked, marks.clone()),
    )
    .await
    .expect("second tab");
    settle().await;
    assert_eq!(second.role(), Role::Follower);

    first.close();
    settle().await;
    assert_eq!(
        marks.entered.get(),
        1,
        "the promotion must really have reached the factory and parked there"
    );
    assert_eq!(marks.resumed.get(), 0);
    assert_eq!(marks.abandoned.get(), 0, "and must not be cancelled yet");

    // The page closes the tab while that bootstrap is still parked.
    second.close();
    settle().await;

    assert_eq!(
        marks.abandoned.get(),
        1,
        "close must cancel the bootstrap, not wait for it: the factory future \
         has to be dropped while parked"
    );
    assert_eq!(
        marks.resumed.get(),
        0,
        "a cancelled bootstrap must not go on to build a backend"
    );

    // And the lock is free *now*, with the barrier still parked —
    // which is what a queued successor is entitled to.
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
        "the cancelled bootstrap must have released the origin's lock before its \
         factory finished, or no successor can ever take over from a parked connect"
    );
    assert_eq!(third.generation(), 3);

    // Releasing the barrier afterwards changes nothing: there is no
    // future left to resume.
    let _ = release.send(());
    settle().await;
    settle().await;
    assert_eq!(marks.resumed.get(), 0);
    assert_eq!(second.role(), Role::Follower);
    assert!(
        log.borrow().performed.is_empty(),
        "the cancelled tab must perform nothing, ever: {:?}",
        log.borrow().performed
    );

    third.close();
    settle().await;
}

/// A cancelled bootstrap **closes the RTC resources it had already
/// created**, and does so before the origin's lock reaches a
/// successor.
///
/// The test above establishes that cancellation drops the parked
/// factory future and frees the lock. That is not the same claim as
/// this one. A real bootstrap is `wasm::LeafNode::connect`, and by
/// the time it parks — on the answer, on the channel, on Noise, on
/// enrollment — it owns a live `RTCPeerConnection` with the Net
/// DataChannel on it: `create_offer` installs the link *before* its
/// first await. Dropping the future used to leave all of that
/// running, because the transport's peers map was kept alive by a
/// cycle through the very handler it stored —
///
/// ```text
/// peers map -> PeerLink._closures -> low_water_handler -> peers map
/// ```
///
/// — so nothing reached zero, nothing was dropped, and no
/// `RTCPeerConnection.close()` ever ran. The successor was then
/// granted the origin's lock while its predecessor's ICE agent was
/// still gathering and its channel still open.
///
/// Two things make this a witness rather than a restatement:
///
/// 1. **The connection is the browser's.** The factory drives the
///    production [`RtcLeafTransport::create_offer`] and keeps the
///    `RTCPeerConnection` the engine created; every state below is
///    read off that object. Nothing replaces the transport.
/// 2. **The ordering is observed, not assumed.** The successor's own
///    factory samples that state on the way in. A successor can only
///    be running because the cancelled bootstrap released the lock,
///    so `closed` sampled there is a close that happened *before*
///    the release — which is the whole lock-ordering claim, and the
///    one a "drop the lock and clean up later" repair would fail.
#[wasm_bindgen_test]
async fn a_cancelled_bootstraps_real_rtc_connection_is_closed_before_the_lock_moves() {
    let db = unique("rtc-cancel-db");
    let scope = unique("rtc-cancel-scope");
    let attempt = Rc::new(RtcAttempt::default());
    let (release, parked) = oneshot::channel();

    let first = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("first leader");
    settle().await;

    // The tab whose promotion will really build RTC state and park.
    let second = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        rtc_parking_factory(
            0x2222,
            Rc::new(RefCell::new(Log::default())),
            0,
            parked,
            attempt.clone(),
        ),
    )
    .await
    .expect("second tab");
    settle().await;
    assert_eq!(second.role(), Role::Follower);

    first.close();
    // `create_offer` is two real promises deep, so this waits for the
    // connection rather than assuming one turn is enough.
    while attempt.connection.borrow().is_none() {
        settle().await;
    }
    let connection = attempt
        .connection
        .borrow()
        .clone()
        .expect("the parked bootstrap created a real connection");

    // The premise. Without it "closed" afterwards would be consistent
    // with a connection that was never brought up at all.
    assert!(
        attempt.offered.get(),
        "the parked bootstrap must really have produced an offer with SDP"
    );
    assert_ne!(
        connection.connection_state(),
        RtcPeerConnectionState::Closed,
        "the bootstrap's connection is live while it is parked"
    );

    // A successor queues for the lock *before* the cancellation, so
    // its factory is what runs at the release.
    let third_log = Rc::new(RefCell::new(Log::default()));
    let third = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        sampling_factory(0x3333, third_log.clone(), attempt.clone()),
    )
    .await
    .expect("third tab");
    settle().await;
    assert_eq!(third.role(), Role::Follower);
    assert_eq!(
        attempt.at_successor.get(),
        None,
        "nothing has been promoted yet, so nothing has sampled yet"
    );

    // The page closes the tab whose promotion is still parked.
    second.close();
    while third.role() != Role::Leader {
        settle().await;
    }
    settle().await;

    assert_eq!(third.generation(), 3);
    assert_eq!(
        attempt.at_successor.get(),
        Some(RtcPeerConnectionState::Closed),
        "the cancelled bootstrap's real connection must already be closed at the \
         moment the lock reaches its successor"
    );
    assert_eq!(
        connection.connection_state(),
        RtcPeerConnectionState::Closed,
        "and it must stay closed"
    );

    // Releasing the barrier afterwards resumes nothing: there is no
    // future left, and no second connection appears.
    let _ = release.send(());
    settle().await;
    settle().await;
    assert_eq!(
        connection.connection_state(),
        RtcPeerConnectionState::Closed
    );

    third.close();
    settle().await;
}

/// A promotion whose **generation transaction** fails re-attaches and
/// queues again, exactly as a failed backend does.
///
/// R13 stopped reporting an uncommitted write as a success, which is
/// the repair this schedule needs in order to exist at all: the
/// abort now arrives as a typed failure. But it arrived above the
/// recovery path — `next_generation().await?` propagated straight
/// out of `take_leadership`, past the reattach-and-requeue the
/// backend branch had — so `await_promotion` logged it and returned.
/// The sole surviving tab was then holding the origin's lock with no
/// node, no client and no acquisition in flight: stranded by the
/// very honesty that was added to help it.
///
/// The break is a real storage failure, not an injected one: a
/// database at the vault's own version whose generation store is
/// missing — what a build with one store, or an upgrade interrupted
/// between two `createObjectStore` calls, leaves behind. The vault
/// opens it without an upgrade, the identity loads, and
/// `next_generation` is the operation that cannot run. The two tabs
/// are given different databases and the **same** lock scope, which
/// is how one tab can have a working store while the other does not.
#[wasm_bindgen_test]
async fn a_promotion_whose_generation_transaction_fails_requeues_instead_of_stranding_the_tab() {
    let scope = unique("gen-fail-scope");
    let healthy = unique("gen-fail-db");
    let broken = unique("gen-fail-broken-db");
    create_db_without_the_generation_store(&broken).await;

    let first = Lifecycle::open(
        opts(&healthy, &scope, &[], &[]),
        factory(0x1111, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("first leader");
    settle().await;
    assert_eq!(first.role(), Role::Leader);

    let second = Lifecycle::open(
        opts(&broken, &scope, &["second-chan"], &[]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("second tab");
    settle().await;
    assert_eq!(second.role(), Role::Follower);
    let events = record_events(&second);

    first.close();
    settle().await;

    assert_eq!(
        second.role(),
        Role::Follower,
        "a tab whose generation could not be allocated is not the leader"
    );
    let seen: Vec<String> = events.borrow().clone();
    assert!(
        seen.iter()
            .any(|json| json.contains("\"promotion_failed\"")
                && json.contains("\"generation\":\"0\"")),
        "the failure must be surfaced to the page, naming generation zero because \
         none was allocated — the counter starts at one: {seen:?}"
    );

    // A *functioning* follower, which is the whole point: the origin
    // now has no node, and the next tab to bring one up must find
    // this tab's declarations waiting for it.
    let third_log = Rc::new(RefCell::new(Log::default()));
    let third = Lifecycle::open(
        opts(&healthy, &scope, &[], &[]),
        factory(0x3333, third_log.clone(), false),
    )
    .await
    .expect("third tab");
    settle().await;
    settle().await;
    assert_eq!(
        third.role(),
        Role::Leader,
        "the failed promotion must have given the lock back"
    );
    assert!(
        third_log.borrow().performed.iter().any(|request| matches!(
            request,
            LeaderRequest::Subscribe { channel } if channel == "second-chan"
        )),
        "the failed tab must have re-attached and re-declared: {:?}",
        third_log.borrow().performed
    );

    // Closed first, so its requeued acquisition stops retrying
    // against a store that will never work.
    second.close();
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

    let events = record_events(&second);

    // The composed work, observed on the WIRE. `performed` alone can
    // never fail this claim here — `Delay::Fail` never builds a
    // backend, so nothing could reach `performed` whatever the
    // promotion composed. The one place a half-built promotion's
    // composed announcement surfaces is the broadcast channel, and
    // this listener is it.
    let wire = web_sys::BroadcastChannel::new(&scope).expect("wire listener");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let collecting = seen.clone();
    let on_message = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        if let Some(text) = event.data().as_string() {
            collecting.borrow_mut().push(text);
        }
    }) as Box<dyn FnMut(web_sys::MessageEvent)>);
    wire.set_onmessage(Some(
        on_message.as_ref().unchecked_ref::<js_sys::Function>(),
    ));

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
    // The listener's own positive control: the promotion that DID
    // succeed announces itself (0x3333 = 13107), so a silent
    // listener cannot make the refusal below vacuous.
    assert!(
        seen.borrow().iter().any(|text| {
            text.contains("\"kind\":\"leadership\"") && text.contains("\"node_id\":\"13107\"")
        }),
        "the wire listener must hear the promotion that succeeded: {:?}",
        seen.borrow()
    );
    // And the failed tab (0x2222 = 8738) published NOTHING — not
    // even the leadership announcement it was mid-composing.
    assert!(
        seen.borrow().iter().all(|text| {
            !(text.contains("\"kind\":\"leadership\"") && text.contains("\"node_id\":\"8738\""))
        }),
        "a promotion that failed must have published nothing, not even the \
         announcement it was mid-composing: {:?}",
        seen.borrow()
    );
    wire.set_onmessage(None);
    wire.close();
    drop(on_message);
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
    // Read off the REAL error object. (This used to run
    // `Reflect::get(&JsValue::from_str(""), …)` — a property read on
    // an empty string primitive — so the second stage was always
    // `""` and the assert below accepted every failure at every
    // stage as "the typed stale-generation one".)
    let error = JsValue::from(refused);
    let message = error
        .as_string()
        .or_else(|| {
            Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_default();
    assert!(
        message.contains("not the leader"),
        "the refusal must be the typed stale-generation one, not any failure: {message:?}"
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
    // The event source, at last: the production sink this promotion's
    // factory was handed, driven with one `stream_data` event for
    // this stream's own key. With no event in flight, "the stale
    // consumer stopped" was true by construction — nothing could
    // reach either consumer.
    let fresh_count = Rc::new(Cell::new(0usize));
    {
        let counting = fresh_count.clone();
        let callback = Closure::wrap(Box::new(move |_json: JsValue| {
            counting.set(counting.get() + 1);
        }) as Box<dyn FnMut(JsValue)>);
        fresh.on_message(
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone(),
        );
        callback.forget();
    }
    // The fixture's reply named the peer 0xfeed and the stream id 9 —
    // its `unwrap_or` defaults for an open that named neither — and
    // the filter is decimal, exactly as `to_json` writes it.
    let event = format!(
        "{{\"type\":\"stream_data\",\"peer_node\":\"{}\",\"incarnation\":\"1\",\
         \"stream_id\":\"{}\",\"seq\":\"1\",\"payload\":\"aGk=\"}}",
        0xfeed, 9
    );
    let sink = follower_log
        .borrow()
        .sink
        .clone()
        .expect("the promotion's factory was handed the production sink");
    sink(&event);
    settle().await;
    assert_eq!(
        fresh_count.get(),
        1,
        "the event source must actually fire for the current generation's consumer"
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

    // The LAST announcement, not a hit somewhere in the log: the
    // claim is "the announcement that was in force", and a cap
    // published here and then narrowed away by a later reconcile is
    // exactly the "silently reverted at the next handoff" defect,
    // deferred one step.
    assert_eq!(
        last_announcement(&follower_log),
        Some(vec!["cap:runtime".to_string()]),
        "the promoted tab must re-publish the announcement that was in force, \
         and the application did not ask again: {:?}",
        follower_log.borrow().performed
    );

    follower.close();
    settle().await;
}

/// A leader's own `announce()` publishes the origin's union, not just
/// its own list — so it cannot withdraw a live follower's capability.
///
/// `Lifecycle::announce` published its argument verbatim and recorded
/// it as everything this tab had announced. Reconciliation had
/// already published A∪B, so a leader with intent A beside a
/// follower with intent B that called `announce(C)` replaced A∪B with
/// C: the follower's tag vanished from the network document
/// indefinitely, because nothing schedules reconciliation for a
/// leader-local request and B has no reason to say anything again.
/// A querying peer then picks a node that no longer offers what it
/// asked for.
///
/// The assertion is on the **last** announcement performed, not on
/// the presence of one somewhere in the log: an earlier correct union
/// followed by a narrower replacement is exactly the defect.
#[wasm_bindgen_test]
async fn a_leaders_own_announcement_keeps_its_followers_capabilities() {
    let db = unique("union-db");
    let scope = unique("union-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));

    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &["cap:leader"]),
        factory(0x1111, leader_log.clone(), false),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &["cap:follower"]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("follower");
    settle().await;
    settle().await;

    assert_eq!(
        last_announcement(&leader_log),
        Some(vec!["cap:follower".to_string(), "cap:leader".to_string()]),
        "the premise: reconciliation has already published the union"
    );

    // The page on the leading tab changes its own intent.
    leader
        .announce(vec!["cap:leader-2".to_string()])
        .await
        .expect("the leader's own announcement");
    settle().await;

    assert_eq!(
        last_announcement(&leader_log),
        Some(vec!["cap:follower".to_string(), "cap:leader-2".to_string()]),
        "a leader's announce() is its own intent, and the document is the union: \
         the still-attached follower's tag must survive it"
    );

    follower.close();
    leader.close();
    settle().await;
}

/// When the last capability-declaring follower detaches, its tag is
/// **withdrawn** rather than left published.
///
/// `reconcile_announcement` returned early on an empty union, so the
/// departed tab's capability stayed in the document that was last
/// published — forever, because every later reconciliation computed
/// the same empty set and returned at the same line. An announced tag
/// the origin does not offer is worse than no tag: it is what a
/// querying peer dials before being refused.
#[wasm_bindgen_test]
async fn the_last_capability_declaring_followers_tag_is_withdrawn() {
    let db = unique("withdraw-db");
    let scope = unique("withdraw-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));

    // The leader declares nothing of its own, so the follower is the
    // only source of a capability and the union goes to empty.
    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        factory(0x1111, leader_log.clone(), false),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &["cap:only"]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("follower");
    settle().await;
    settle().await;
    assert_eq!(
        last_announcement(&leader_log),
        Some(vec!["cap:only".to_string()]),
        "the premise: the follower's tag is published"
    );

    // The tab offering it goes away.
    follower.close();
    settle().await;
    settle().await;

    assert_eq!(
        last_announcement(&leader_log),
        Some(Vec::new()),
        "the departed follower's capability must be withdrawn by an actual \
         announcement, not merely dropped from a set nobody publishes: {:?}",
        leader_log.borrow().performed
    );

    leader.close();
    settle().await;
}

/// An **unchanged** follower announcement leaves the published union
/// alone.
///
/// The sibling of the leader-local overwrite above, and the one that
/// survived it. A follower's `Announce` was performed verbatim: the
/// backend published *that tab's* list, so the leader's own live
/// capability — and every other follower's — was withdrawn by a tab
/// that had merely repeated what it already wanted. Reconciliation
/// could not repair it either, because the union publisher's cache
/// still recorded the union as the published document, so its
/// equality check returned early. Nothing later corrected it: the
/// follower has no reason to speak again.
///
/// The follower's declaration therefore does not publish anything. It
/// is recorded, and its caller is parked on the authoritative union
/// publication that follows — so the promise still settles, and still
/// means what it says.
#[wasm_bindgen_test]
async fn an_unchanged_follower_announcement_keeps_the_published_union() {
    let db = unique("refresh-db");
    let scope = unique("refresh-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));

    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &["cap:leader"]),
        factory(0x1111, leader_log.clone(), false),
    )
    .await
    .expect("leader");
    settle().await;
    let follower = Lifecycle::open(
        opts(&db, &scope, &[], &["cap:follower"]),
        factory(0x2222, Rc::new(RefCell::new(Log::default())), false),
    )
    .await
    .expect("follower");
    settle().await;
    settle().await;

    let union = vec!["cap:follower".to_string(), "cap:leader".to_string()];
    assert_eq!(
        last_announcement(&leader_log),
        Some(union.clone()),
        "the premise: reconciliation has already published the union"
    );
    let before = leader_log.borrow().performed.len();

    // The follower re-declares exactly the intent it already holds.
    follower
        .announce(vec!["cap:follower".to_string()])
        .await
        .expect("a follower's announcement must still complete");
    settle().await;
    settle().await;

    assert_eq!(
        last_announcement(&leader_log),
        Some(union.clone()),
        "an unchanged follower announcement must not narrow the published \
         document: {:?}",
        leader_log.borrow().performed
    );
    assert!(
        leader_log
            .borrow()
            .performed
            .iter()
            .skip(before)
            .all(|request| match request {
                LeaderRequest::Announce { capabilities } => capabilities == &union,
                _ => true,
            }),
        "and no announcement carrying one tab's list alone may reach the node at \
         all: {:?}",
        leader_log.borrow().performed
    );

    follower.close();
    leader.close();
    settle().await;
}

/// A proxied synchronous send that makes **another** stream fail
/// broadcasts that failure instead of panicking on the server borrow.
///
/// `Lifecycle::request` and the incoming-proxy `dispatch` both hold
/// `Shared.server` while the backend performs. Most arms spawn, but
/// `StreamSend` does not: it calls a real `wasm::LeafStream::send`,
/// which pumps the node, drives reliability and dispatches whatever
/// that produced — all inside the outer borrow. A reliable
/// descriptor on a *different* stream whose retry budget runs out in
/// that sweep therefore reaches the production `event_sink` while the
/// server is already borrowed, and the sink's own
/// `server.borrow_mut()` was a panic. No application callback is
/// needed to trigger it.
///
/// `try_borrow_mut` and dropping the event would have traded the
/// panic for a follower that is never told its stream died, so the
/// broadcast is **deferred**, not suppressed: queued by the sink and
/// taken by the flush that runs once the borrow is gone.
///
/// The schedule is the reachable one: stream X is sent on and never
/// acknowledged, the periodic ticker has not swept, and the page
/// sends on healthy stream Y. Y's own pump is what notices X.
#[wasm_bindgen_test]
async fn a_proxied_send_broadcasts_another_streams_terminal_event_instead_of_panicking() {
    let db = unique("reborrow-db");
    let scope = unique("reborrow-scope");
    let (handles, _peer) = real_pair();

    let leader = Lifecycle::open(opts(&db, &scope, &[], &[]), real_factory(handles.clone()))
        .await
        .expect("leader");
    let seen = record_events(&leader);
    settle().await;
    assert_eq!(leader.role(), Role::Leader);

    // X: reliable, sent once, never acknowledged. Its retransmit
    // budget starts running now, against the wall clock the
    // reliability layer reads for itself.
    let opened_x = leader
        .request(LeaderRequest::StreamOpen {
            label: "doomed".into(),
            reliability: Reliability::Reliable,
            stream_id: Some(17),
            channel_hash: None,
            peer: None,
        })
        .await
        .expect("open X");
    let ProxyValue::Stream { handle: x, .. } = opened_x else {
        panic!("open X did not answer with a stream: {opened_x:?}");
    };
    leader
        .request(LeaderRequest::StreamSend {
            handle: x,
            payload: Bytes::from_static(b"unacknowledged"),
        })
        .await
        .expect("send on X");
    let opened_y = leader
        .request(LeaderRequest::StreamOpen {
            label: "healthy".into(),
            reliability: Reliability::Reliable,
            stream_id: Some(34),
            channel_hash: None,
            peer: None,
        })
        .await
        .expect("open Y");
    let ProxyValue::Stream { handle: y, .. } = opened_y else {
        panic!("open Y did not answer with a stream: {opened_y:?}");
    };

    // One sweep per attempt outside any server borrow spends X's
    // retry budget. Attempts are paced by the wire's backed-off RTO
    // (`ReliableStream::retransmit_timeout`), so attempt `n` needs
    // `DEFAULT_RTO << n` to come due and each sweep that fires resets
    // the packet's `sent_at` — the waits are read off that ladder
    // rather than assumed to be one fixed RTO apart. None of these
    // sweeps can give up yet.
    let rto = net_wire::reliability::ReliableStream::DEFAULT_RTO;
    let attempts = net_wire::reliability::ReliableStream::DEFAULT_MAX_RETRIES;
    for attempt in 0..attempts {
        let due = net_wire::reliability::ReliableStream::retransmit_timeout(rto, attempt);
        wait_ms(due.as_millis() as i32 + 40).await;
        let mut node = handles.node.borrow_mut();
        node.tick(net_leaf::clock::now());
        node.take_outbound();
        assert!(
            node.drain_events().is_empty(),
            "the premise: X is still recoverable after this sweep"
        );
    }
    assert!(
        !seen
            .borrow()
            .iter()
            .any(|json| json.contains("\"type\":\"stream_failed\"")),
        "the premise: nothing has failed yet"
    );

    // The sweep after the budget is spent is the one that gives up,
    // and it runs inside Y's synchronous send — under the caller's
    // server borrow. Its wait is the exhausted attempt's own
    // backed-off timeout.
    let final_due = net_wire::reliability::ReliableStream::retransmit_timeout(rto, attempts);
    wait_ms(final_due.as_millis() as i32 + 40).await;
    handles.sweeps.set(1);
    let sent = leader
        .request(LeaderRequest::StreamSend {
            handle: y,
            payload: Bytes::from_static(b"healthy"),
        })
        .await;
    assert!(
        sent.is_ok(),
        "the healthy stream's send must have a defined result: {sent:?}"
    );
    settle().await;
    settle().await;

    assert!(
        seen.borrow().iter().any(|json| {
            json.contains("\"type\":\"stream_failed\"")
                && json.contains("\"stream_id\":\"17\"")
                && json.contains("\"reason\":\"retransmits_exhausted\"")
        }),
        "X's terminal event must reach the session rather than being lost to a \
         failed borrow: {:?}",
        seen.borrow()
    );

    // And the session still serves afterwards, which a panicked
    // callback frame would not have left true.
    leader
        .request(LeaderRequest::Counters)
        .await
        .expect("the session must still serve after the deferred broadcast");

    leader.close();
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

/// A follower's call with **no** timeout expires too, on the leaf's
/// own default, with the same disposition.
///
/// This is the branch the explicit-timeout repair left open, and it
/// is the one an ordinary page takes: `session.call(service, bytes)`
/// passes `None`. The node reads `None` as
/// [`net_leaf::rpc::DEFAULT_CALL_TIMEOUT_MS`] — but it applies that
/// default only when it *executes* the request, so a leader frozen
/// while holding its lock applied nothing and the caller's promise
/// had no deadline anywhere in the system. The follower now arms the
/// same default locally.
///
/// The wait is the default, and that is why this test is slow: there
/// is no shorter honest way to observe a 30-second deadline, and
/// asserting the number inside `Indeterminate` is what pins it to the
/// node's own constant rather than to a value invented here.
#[wasm_bindgen_test]
async fn a_follower_call_without_a_timeout_expires_on_the_leafs_default_deadline() {
    let db = unique("default-deadline-db");
    let scope = unique("default-deadline-scope");
    let leader_log = Rc::new(RefCell::new(Log::default()));

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
            timeout_ms: None,
        })
        .await
        .expect_err("an ordinary call nobody answers must still settle");
    let elapsed = now_ms() - started;

    assert_eq!(
        failure.typed(),
        Some(&LeafError::Rpc(RpcError::Indeterminate {
            deadline_ms: 30_000
        })),
        "the armed default must be the node's own, and the disposition the same \
         Indeterminate an explicit deadline produces: {failure:?}"
    );
    assert!(
        (30_000.0..40_000.0).contains(&elapsed),
        "it must expire on the default, not sooner and not never; took {elapsed} ms"
    );
    assert_eq!(
        leader_log
            .borrow()
            .performed
            .iter()
            .filter(|request| matches!(request, LeaderRequest::Call { .. }))
            .count(),
        1,
        "and still nothing is re-issued"
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
    // The typed variant, not its prose: a reworded-but-correct
    // message must not break this witness. The claim — a commit
    // failure, not a success — is carried by the behavioural legs
    // below: the rolled-back generation is unobservable and the store
    // still works afterwards.
    assert!(
        matches!(&aborted, LeafError::Identity(_)),
        "the abort must arrive as the typed identity failure: {aborted:?}"
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
    // The typed variant, not its prose. The claim itself is the
    // behavioural half: the control above loads an untouched record
    // through this same call, so what refuses here is the tamper.
    assert!(
        matches!(&refused, LeafError::Identity(_)),
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

/// Create `db_name` at the vault's own version with the identity
/// store only, so the generation counter's store is missing.
///
/// Not an injected failure: this is the state a build with one store,
/// or an upgrade interrupted between the two `createObjectStore`
/// calls, leaves on a real page. The vault opens it without running
/// an upgrade — the version already matches — so the identity path
/// works and the generation transaction is the one that cannot.
async fn create_db_without_the_generation_store(db_name: &str) {
    let factory = web_sys::window()
        .expect("window")
        .indexed_db()
        .expect("indexedDB")
        .expect("indexedDB is available");
    let open = factory.open_with_u32(db_name, 1).expect("open");
    let target = open.clone();
    let upgrade = Closure::once(Box::new(move |_e: web_sys::Event| {
        let database: web_sys::IdbDatabase = target
            .result()
            .expect("result")
            .dyn_into()
            .expect("a database");
        database
            .create_object_store("identity")
            .expect("the identity store, and deliberately not the leader one");
    }) as Box<dyn FnOnce(web_sys::Event)>);
    open.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    await_request(&open).await;
    open.set_onupgradeneeded(None);
    drop(upgrade);
    let database: web_sys::IdbDatabase = open
        .result()
        .expect("result")
        .dyn_into()
        .expect("a database");
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

/// Two authenticated peers, one wire stream id, and all four
/// operations — on the leader's own `MeshSession` and on a follower's.
///
/// `MeshSession.openStream` always wraps `ProxyStream`, so this is not
/// a follower-only property: the leader tab's own session had the same
/// gap. Both paths run here, and the follower's runs through the real
/// `BroadcastChannel` request/reply carrier.
///
/// Every operation is owner-scoped, and before the repair three were
/// not: the backend keyed its open-stream table by the wire id, so the
/// second open overwrote the first's slot and the earlier caller's
/// `send` reached — and its `close` closed — the other peer's stream;
/// and `ProxyStream::on_message` filtered by the id alone, so each
/// consumer took whichever peer's payload arrived. The addressing now
/// lives in the production `StreamOwnership` this fixture's backend
/// holds too, so a change to it is visible from here.
async fn two_peers_own_their_streams(through_follower: bool) {
    let db = unique("stream-own-db");
    let scope = unique("stream-own-scope");
    let (handles, _peer_a_node) = real_pair();
    let peer_a = handles.peer;
    let (_peer_b_node, peer_b) = add_peer(&handles);

    let leader = Lifecycle::open(opts(&db, &scope, &[], &[]), real_factory(handles.clone()))
        .await
        .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);

    // The session under test: the leader's own, or a follower whose
    // every request crosses the carrier to that leader.
    let follower = if through_follower {
        let f = Lifecycle::open(
            opts(&db, &scope, &[], &[]),
            factory(0x9999, Rc::new(RefCell::new(Log::default())), false),
        )
        .await
        .expect("follower");
        settle().await;
        assert_eq!(
            f.role(),
            Role::Follower,
            "the second tab must be a follower"
        );
        Some(f)
    } else {
        None
    };
    let session = follower.as_ref().unwrap_or(&leader);

    // One label, one explicit wire id, two peers.
    let open = |peer: u64| {
        let o = Object::new();
        put(&o, "reliability", &JsValue::from_str("reliable"));
        put(&o, "streamId", &JsValue::from_str("9"));
        put(&o, "peer", &JsValue::from_str(&format!("{peer:016x}")));
        o
    };
    let a = session
        .open_stream(&open(peer_a).into())
        .await
        .expect("open to peer A");
    let b = session
        .open_stream(&open(peer_b).into())
        .await
        .expect("open to peer B");

    // Same wire id — the ordinary case, and the reason the id cannot
    // be the owner. Different resolved peers, each read off the stream
    // the leader's node actually opened.
    assert_eq!(a.stream_id_hex(), b.stream_id_hex());
    assert_eq!(a.peer_node_hex(), format!("{peer_a:016x}"));
    assert_eq!(b.peer_node_hex(), format!("{peer_b:016x}"));

    // The identity is RESOLVED, not echoed. An open that names no
    // peer still learns which authenticated peer it got — so a handle
    // that repeated the request's options back would read zero here,
    // and its filter would match nothing while looking filled in.
    let anonymous = Object::new();
    put(&anonymous, "reliability", &JsValue::from_str("reliable"));
    put(&anonymous, "streamId", &JsValue::from_str("11"));
    let unnamed = session
        .open_stream(&anonymous.into())
        .await
        .expect("open without naming a peer");
    assert_eq!(
        unnamed.peer_node_hex(),
        format!("{peer_a:016x}"),
        "an open that named no peer must report the peer it actually got"
    );
    unnamed.close();
    settle().await;

    // ── receive ──────────────────────────────────────────────────
    // Both consumers listen; the session's event vector carries both
    // peers' frames, exactly as the node emits them.
    let to_a = Rc::new(RefCell::new(Vec::<String>::new()));
    let to_b = Rc::new(RefCell::new(Vec::<String>::new()));
    for (stream, seen) in [(&a, to_a.clone()), (&b, to_b.clone())] {
        let collecting = seen;
        let callback = Closure::wrap(Box::new(move |json: JsValue| {
            collecting
                .borrow_mut()
                .push(json.as_string().unwrap_or_default());
        }) as Box<dyn FnMut(JsValue)>);
        stream.on_message(
            callback
                .as_ref()
                .unchecked_ref::<js_sys::Function>()
                .clone(),
        );
        callback.forget();
    }
    let sink = handles.sink.borrow().clone().expect("the production sink");
    let wire_id = u64::from_str_radix(&a.stream_id_hex(), 16).expect("hex");
    // Distinct payloads, so "received something" cannot pass for
    // "received its own": `AQ==` is [1] and `Ag==` is [2].
    for (peer, payload) in [(peer_a, "AQ=="), (peer_b, "Ag==")] {
        sink(&format!(
            "{{\"type\":\"stream_data\",\"peer_node\":\"{peer}\",\"incarnation\":\"1\",\
             \"stream_id\":\"{wire_id}\",\"seq\":\"1\",\"payload\":\"{payload}\"}}"
        ));
    }
    settle().await;
    let a_seen = to_a.borrow().clone();
    let b_seen = to_b.borrow().clone();
    assert_eq!(a_seen.len(), 1, "A took exactly one frame: {a_seen:?}");
    assert_eq!(b_seen.len(), 1, "B took exactly one frame: {b_seen:?}");
    assert!(
        a_seen[0].contains("\"payload\":\"AQ==\"") && a_seen[0].contains(&peer_a.to_string()),
        "A must take A's payload, got {a_seen:?}"
    );
    assert!(
        b_seen[0].contains("\"payload\":\"Ag==\"") && b_seen[0].contains(&peer_b.to_string()),
        "B must take B's payload, got {b_seen:?}"
    );

    // ── send ─────────────────────────────────────────────────────
    // The backend's arm pumps the real node and records what went out,
    // so the destination is observable where the fixture puts it.
    handles.wire.borrow_mut().clear();
    a.send(Uint8Array::from(&b"for-a"[..]))
        .await
        .expect("send on A");
    settle().await;
    let addressed: Vec<u64> = handles
        .wire
        .borrow()
        .iter()
        .map(|(peer, _)| *peer)
        .collect();
    assert!(
        !addressed.is_empty(),
        "A's send must put a packet on the wire"
    );
    assert!(
        addressed.iter().all(|peer| *peer == peer_a),
        "A's payload must go to peer A alone, got {addressed:?}"
    );

    handles.wire.borrow_mut().clear();
    b.send(Uint8Array::from(&b"for-b"[..]))
        .await
        .expect("send on B");
    settle().await;
    let addressed: Vec<u64> = handles
        .wire
        .borrow()
        .iter()
        .map(|(peer, _)| *peer)
        .collect();
    assert!(
        !addressed.is_empty() && addressed.iter().all(|peer| *peer == peer_b),
        "B's payload must go to peer B alone, got {addressed:?}"
    );

    // ── close ────────────────────────────────────────────────────
    a.close();
    settle().await;

    // A is demonstrably unusable: the backend gave up the stream its
    // handle owned, so this is refused rather than delivered to B's.
    handles.wire.borrow_mut().clear();
    let refused = a.send(Uint8Array::from(&b"after-close"[..])).await;
    assert!(
        refused.is_err(),
        "a closed stream must refuse to send, not reach another open"
    );
    assert!(
        handles.wire.borrow().is_empty(),
        "a closed stream's send must put nothing on the wire: {:?}",
        handles.wire.borrow().len()
    );

    // And B still works, both ways, under the same wire id.
    handles.wire.borrow_mut().clear();
    b.send(Uint8Array::from(&b"still-b"[..]))
        .await
        .expect("B must still send after A closed");
    settle().await;
    let addressed: Vec<u64> = handles
        .wire
        .borrow()
        .iter()
        .map(|(peer, _)| *peer)
        .collect();
    assert!(
        !addressed.is_empty() && addressed.iter().all(|peer| *peer == peer_b),
        "B's post-close payload must still go to peer B alone, got {addressed:?}"
    );
    to_b.borrow_mut().clear();
    sink(&format!(
        "{{\"type\":\"stream_data\",\"peer_node\":\"{peer_b}\",\"incarnation\":\"1\",\
         \"stream_id\":\"{wire_id}\",\"seq\":\"2\",\"payload\":\"Ag==\"}}"
    ));
    settle().await;
    assert_eq!(
        to_b.borrow().len(),
        1,
        "B still receives its own frames after A closed"
    );

    if let Some(follower) = follower {
        follower.close();
    }
    leader.close();
    settle().await;
}

/// The leader's own `MeshSession` — which wraps the same
/// `ProxyStream` a follower's does.
#[wasm_bindgen_test]
async fn two_peers_under_one_wire_id_own_their_streams_independently() {
    two_peers_own_their_streams(false).await;
}

/// The same property through a follower, so every request crosses the
/// real carrier to the leader's backend.
#[wasm_bindgen_test]
async fn a_follower_owns_its_streams_independently_under_one_wire_id() {
    two_peers_own_their_streams(true).await;
}

// ─────────── org terminals at the JS boundary (LEAF-1) ───────────
//
// One call, several consumers: `OrgDuplexCallHandle::stream()` mints a
// fresh `OrgByteStreamHandle` over the same `OrgCall` per call, and the
// call's terminal is latched so `finish` and every `next` agree on it.
// Before the repair, `next_outcome` answered a poll made AFTER the
// first terminal delivery with a fabricated `Completed { body: [] }` —
// so the second consumer of a call whose real terminal was
// `Retired { Revoked }`, an `AdmissionDenied` or any refusal saw a
// clean end-of-stream. These witnesses hold the remote terminal fixed
// (the leader-side half a proxied call actually crosses) and assert
// that EVERY consumer — and every over-poll past the terminal —
// observes that same typed terminal.

/// What the remote answers a call's response half with: the typed
/// terminal, spelled exactly as the leader's org relay spells it —
/// an `ORG_ENVELOPE_RETIRED` envelope for a retirement, a typed
/// `ProxyFailure` for a refusal.
#[derive(Clone)]
enum TerminalAnswer {
    /// A retirement, as the relay's retire envelope and its frozen
    /// `OrgRetireReason` string.
    Retired(&'static str),
    /// A refusal, as `reply.fail` carries it: the wire status and the
    /// service's message.
    Refused(u16, String),
}

/// A leader backend that acknowledges one org call and then answers
/// every `OrgNext` with the named terminal — the leader-side half a
/// witness needs, without a node, a session or a provider.
struct TerminalBackend {
    node_id: u64,
    terminal: TerminalAnswer,
}

impl LeaderBackend for TerminalBackend {
    fn node_id(&self) -> u64 {
        self.node_id
    }

    fn perform(&mut self, _from: ProxySide, request: LeaderRequest, reply: Replier) {
        match request {
            // The open is acknowledged exactly as the real relay
            // acknowledges one: an empty END envelope.
            LeaderRequest::OrgCall { .. } => reply.bytes(Bytes::from_static(&[ORG_ENVELOPE_END])),
            LeaderRequest::OrgNext { .. } => match &self.terminal {
                TerminalAnswer::Retired(reason) => {
                    let mut envelope = Vec::with_capacity(1 + reason.len());
                    envelope.push(ORG_ENVELOPE_RETIRED);
                    envelope.extend_from_slice(reason.as_bytes());
                    reply.bytes(Bytes::from(envelope));
                }
                TerminalAnswer::Refused(status, message) => {
                    reply.fail(ProxyFailure::Typed(LeafError::Rpc(RpcError::Refused {
                        status: *status,
                        message: message.clone(),
                    })));
                }
            },
            _ => reply.bytes(Bytes::new()),
        }
    }

    fn shutdown(&mut self, _generation: u64) -> usize {
        0
    }
}

/// A factory for [`TerminalBackend`], in the shape [`factory`] takes.
fn terminal_factory(node_id: u64, terminal: TerminalAnswer) -> BackendFactory {
    Rc::new(move |_opts, _sink, _lease| {
        let terminal = terminal.clone();
        Box::pin(async move {
            let backend: Box<dyn LeaderBackend> = Box::new(TerminalBackend { node_id, terminal });
            Ok(backend)
        })
    })
}

/// The org call options a proxied org call carries. The proofs are
/// opaque bytes over the proxy — the leader's node mints the signed
/// opening — so their shape is not what is under test here.
fn org_call_opts() -> JsValue {
    let credentials = Object::new();
    put(
        &credentials,
        "membership",
        &Uint8Array::from(&b"membership-wire"[..]).into(),
    );
    put(
        &credentials,
        "dispatcher",
        &Uint8Array::from(&b"dispatcher-wire"[..]).into(),
    );
    put(
        &credentials,
        "actingOrg",
        &JsValue::from_str(&format!("{:064x}", 1)),
    );
    put(
        &credentials,
        "providerOwnerOrg",
        &JsValue::from_str(&format!("{:064x}", 2)),
    );
    put(
        &credentials,
        "provider",
        &JsValue::from_str(&format!("{:064x}", 3)),
    );
    let object = Object::new();
    put(&object, "credentials", &credentials);
    object.into()
}

/// The `{ done, error? }` item `next()` resolves: the done flag, the
/// terminal error object's `kind`, and that object serialized — so two
/// consumers' terminals are compared IDENTICALLY rather than against
/// prose a rewording could keep true.
fn terminal_item(item: &JsValue) -> (bool, Option<String>, Option<String>) {
    let done = Reflect::get(item, &JsValue::from_str("done"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let error = Reflect::get(item, &JsValue::from_str("error"))
        .ok()
        .filter(|value| !value.is_undefined() && !value.is_null());
    let kind = error.as_ref().and_then(|value| {
        Reflect::get(value, &JsValue::from_str("kind"))
            .ok()
            .and_then(|kind| kind.as_string())
    });
    let json = error
        .as_ref()
        .and_then(|value| js_sys::JSON::stringify(value).ok())
        .and_then(|text| text.as_string());
    (done, kind, json)
}

/// A follower session whose leader answers every org call's response
/// half with `answer`.
async fn org_terminal_session(answer: TerminalAnswer) -> (MeshSession, Lifecycle) {
    let db = unique("org-terminal-db");
    let scope = unique("org-terminal-scope");
    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        terminal_factory(0x3333, answer),
    )
    .await
    .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    let session = MeshSession::open(opts(&db, &scope, &[], &[]))
        .await
        .expect("session");
    settle().await;
    assert_eq!(session.role(), "follower");
    (session, leader)
}

/// One duplex call whose real terminal is `answer`, consumed via two
/// `stream()` handles and then over-polled. Every observation must be
/// the SAME typed terminal item; the fabricated completion the
/// over-poll used to answer with shows up as a `done` item with no
/// `error` on every consumer after the first.
async fn every_consumer_observes(answer: TerminalAnswer, kind: &str) {
    let (session, leader) = org_terminal_session(answer).await;
    let handle = session
        .call_org_duplex("svc.terminal".into(), org_call_opts())
        .await
        .expect("the duplex call opens");

    let first = handle.stream();
    let second = handle.stream();

    let (done, first_kind, first_json) =
        terminal_item(&first.next().await.expect("the first terminal"));
    assert!(done, "the terminal arrives as a done item");
    assert_eq!(
        first_kind.as_deref(),
        Some(kind),
        "the first consumer sees the typed terminal: {first_json:?}"
    );
    let first_json = first_json.expect("a typed terminal carries its error object");

    // The second consumer — a fresh handle over the same call — must
    // observe the SAME typed terminal. Before the repair this resolved
    // `{ done: true }`: a fabricated `Completed` presenting a
    // revocation or a refusal as a clean end-of-stream.
    let (done, second_kind, second_json) =
        terminal_item(&second.next().await.expect("the second terminal"));
    assert!(done, "the terminal arrives as a done item");
    assert_eq!(
        second_kind.as_deref(),
        Some(kind),
        "the second consumer must observe the latched typed terminal, never a \
         fabricated completion: {second_json:?}"
    );
    assert_eq!(
        second_json.as_deref(),
        Some(first_json.as_str()),
        "and it is the very same terminal"
    );

    // Over-poll past the terminal: the latched terminal again.
    let (done, third_kind, third_json) =
        terminal_item(&second.next().await.expect("the over-poll"));
    assert!(done);
    assert_eq!(
        third_kind.as_deref(),
        Some(kind),
        "an over-poll past the terminal returns the latched terminal, not a \
         fabricated completion: {third_json:?}"
    );
    assert_eq!(third_json.as_deref(), Some(first_json.as_str()));

    session.close();
    leader.close();
    settle().await;
}

/// A call retired as `Retired { Revoked }` reaches both consumers of a
/// duplex call — a security revocation must never present as clean
/// end-of-stream at the JS boundary.
#[wasm_bindgen_test]
async fn a_second_duplex_stream_observes_the_latched_revoked_terminal_not_a_fabricated_completion()
{
    every_consumer_observes(TerminalAnswer::Retired("revoked"), "revoked").await;
}

/// The same for an admission denial (`Refused { AdmissionDenied }`).
#[wasm_bindgen_test]
async fn a_second_duplex_stream_observes_the_latched_admission_denial_not_a_fabricated_completion()
{
    every_consumer_observes(
        TerminalAnswer::Refused(RpcStatus::AdmissionDenied.to_wire(), "denied".into()),
        "admission-denied",
    )
    .await;
}

/// The same for a plain typed refusal (`Refused` at any other status).
#[wasm_bindgen_test]
async fn a_second_duplex_stream_observes_the_latched_refusal_not_a_fabricated_completion() {
    every_consumer_observes(
        TerminalAnswer::Refused(
            RpcStatus::Unauthorized.to_wire(),
            "the service refused the call".into(),
        ),
        "refused",
    )
    .await;
}

/// A cancel is a typed `cancelled` terminal latched locally — the
/// handle's own contract — and every consumer observes it, never the
/// fabricated completion an over-poll used to be answered with. The
/// remote would have said `revoked` here: the cancelling caller's own
/// action is the terminal it observes, exactly as `cancel` documents.
#[wasm_bindgen_test]
async fn a_cancelled_duplex_call_yields_the_cancelled_terminal_to_every_consumer() {
    let (session, leader) = org_terminal_session(TerminalAnswer::Retired("revoked")).await;
    let handle = session
        .call_org_duplex("svc.cancel".into(), org_call_opts())
        .await
        .expect("the duplex call opens");
    handle.cancel();

    for label in ["the cancelling consumer", "a second consumer"] {
        let stream = handle.stream();
        let (done, kind, json) = terminal_item(&stream.next().await.expect("the terminal"));
        assert!(done, "{label} sees a done item");
        assert_eq!(
            kind.as_deref(),
            Some("cancelled"),
            "{label} must observe the typed cancelled terminal, never a \
             fabricated completion: {json:?}"
        );
    }

    session.close();
    leader.close();
    settle().await;
}

// ─────────── the proxied serve accept seam (LEAF-3/6/14) ───────────

/// The leader-side double for the proxied SERVE seam. It answers the
/// register / accept / caller-fetch / request verbs with exactly the
/// envelopes the real relay emits — so what runs under test is the
/// follower's REAL accept loop (`ProxyOrgServe`), and a forged or
/// tampered envelope is one reply away.
#[derive(Clone, Default)]
struct ServeFake {
    /// Every `OrgServeRegister` seen — the retry-storm counter.
    registers: Rc<Cell<u32>>,
    /// Accept doc BODIES to hand out in order (without the `0x03`
    /// tag: the arm wraps them exactly as the real accept arm does).
    accepts: Rc<RefCell<VecDeque<String>>>,
    /// Bridge handle → the projection the leader's own `ServeCall`
    /// state resolves: the LEADER-VERIFIED caller identity.
    verified: Rc<RefCell<HashMap<u64, String>>>,
    /// Request items per bridge handle; EOF once drained.
    requests: Rc<RefCell<HashMap<u64, VecDeque<Bytes>>>>,
    /// When set, every register is refused with it — the PERMANENT
    /// class LEAF-14 must surface typed, not retry.
    refuse_register: Option<ProxyFailure>,
    /// Long-pulls parked and never answered.
    parked: Rc<RefCell<Vec<Replier>>>,
}

impl LeaderBackend for ServeFake {
    fn node_id(&self) -> u64 {
        0x5E2E
    }

    fn perform(&mut self, _from: ProxySide, request: LeaderRequest, reply: Replier) {
        match request {
            LeaderRequest::OrgServeRegister { .. } => {
                self.registers.set(self.registers.get() + 1);
                match &self.refuse_register {
                    Some(failure) => reply.fail(failure.clone()),
                    None => reply.bytes(Bytes::new()),
                }
            }
            LeaderRequest::OrgServeAccept { .. } => match self.accepts.borrow_mut().pop_front() {
                // The producer's own construction.
                Some(doc) => reply.bytes(envelope(ORG_ENVELOPE_ADMITTED, doc.as_bytes())),
                None => self.parked.borrow_mut().push(reply),
            },
            LeaderRequest::OrgServeCaller { call } => match self.verified.borrow().get(&call) {
                Some(json) => reply.text(json.clone()),
                None => reply.fail(ProxyFailure::Typed(LeafError::Session(format!(
                    "no org call for handle {call}"
                )))),
            },
            LeaderRequest::OrgServeRequest { call } => {
                let item = self
                    .requests
                    .borrow_mut()
                    .get_mut(&call)
                    .and_then(|queue| queue.pop_front());
                match item {
                    Some(body) => reply.bytes(envelope(ORG_ENVELOPE_ITEM, &body)),
                    None => reply.bytes(envelope(ORG_ENVELOPE_END, b"")),
                }
            }
            LeaderRequest::OrgServeSend { .. } | LeaderRequest::OrgServeFinish { .. } => {
                reply.bytes(Bytes::new())
            }
            LeaderRequest::OrgServeRetired { .. } => self.parked.borrow_mut().push(reply),
            _ => reply.bytes(Bytes::new()),
        }
    }

    fn shutdown(&mut self, _generation: u64) -> usize {
        let parked = core::mem::take(&mut *self.parked.borrow_mut());
        let count = parked.len();
        drop(parked);
        count
    }
}

/// The projection the leader's own serve state resolves — the one a
/// handler must see (LEAF-6).
fn verified_caller() -> String {
    "{\"entity\":\"0011223344556677\"}".to_string()
}

/// The forged claim an attacker adds to the accept envelope.
const FORGED_CLAIM: &str = "{\"entity\":\"forged\"}";

/// The handler's recorded `caller` arguments — what reached JS.
fn recording_handler(calls: &Rc<RefCell<Vec<String>>>) -> Function {
    let sink = Rc::clone(calls);
    let closure = Closure::wrap(Box::new(move |caller: JsValue, _request: Uint8Array| {
        sink.borrow_mut()
            .push(caller.as_string().unwrap_or_default());
        js_sys::Promise::resolve(&Uint8Array::from(&b"response"[..]))
    })
        as Box<dyn FnMut(JsValue, Uint8Array) -> js_sys::Promise<Uint8Array>>);
    closure.into_js_value().unchecked_into()
}

/// The `ownerOrg`-bearing options a proxied serve registration reads.
fn serve_opts() -> JsValue {
    let object = Object::new();
    put(
        &object,
        "ownerOrg",
        &JsValue::from_str(&format!("{:064x}", 2)),
    );
    object.into()
}

/// A follower session whose leader is `fake`, plus the leader's own
/// lifecycle — the [`org_terminal_session`] shape one seam over.
async fn serve_session(fake: ServeFake) -> (MeshSession, Lifecycle) {
    let db = unique("org-serve-db");
    let scope = unique("org-serve-scope");
    let leader = Lifecycle::open(
        opts(&db, &scope, &[], &[]),
        Rc::new(move |_opts, _sink, _lease| {
            let fake = fake.clone();
            Box::pin(async move {
                let backend: Box<dyn LeaderBackend> = Box::new(fake);
                Ok(backend)
            })
        }),
    )
    .await
    .expect("leader");
    settle().await;
    assert_eq!(leader.role(), Role::Leader);
    let session = MeshSession::open(opts(&db, &scope, &[], &[]))
        .await
        .expect("session");
    settle().await;
    assert_eq!(session.role(), "follower");
    (session, leader)
}

/// LEAF-3, end to end: a well-formed proxied accept — the exact
/// `0x03 ‖ { call }` envelope the real accept arm emits — dispatches
/// EXACTLY ONE handler. Pre-fix `decode_admitted` ran a JSON parse on
/// the WHOLE tagged payload and dropped every well-formed accept: the
/// handler never ran and the caller parked to its deadline.
#[wasm_bindgen_test]
async fn a_well_formed_proxied_accept_dispatches_exactly_one_handler() {
    let fake = ServeFake::default();
    fake.accepts
        .borrow_mut()
        .push_back("{\"call\":\"7\"}".to_string());
    fake.verified.borrow_mut().insert(7, verified_caller());
    fake.requests
        .borrow_mut()
        .insert(7, VecDeque::from([Bytes::from_static(b"request")]));
    let (session, leader) = serve_session(fake).await;
    let calls = Rc::new(RefCell::new(Vec::new()));
    session
        .serve_org(
            "svc.accept-a".to_string(),
            "same-org".to_string(),
            recording_handler(&calls),
            serve_opts(),
        )
        .expect("the registration starts");
    wait_ms(300).await;

    assert_eq!(
        calls.borrow().len(),
        1,
        "one accept envelope dispatches exactly one handler"
    );
    session.close();
    leader.close();
    settle().await;
}

/// LEAF-6, end to end: the handler's `caller` projection is the
/// leader-verified identity even when the accept envelope's claim is
/// forged. Pre-fix the claim was served verbatim — handler-level
/// caller spoofing on the proxy path.
#[wasm_bindgen_test]
async fn a_forged_accept_claim_is_never_served_to_the_handler_as_the_caller() {
    let fake = ServeFake::default();
    // The accept envelope is TAMPERED: the claim says "forged", the
    // leader's own serve state resolves `verified_caller()`.
    fake.accepts
        .borrow_mut()
        .push_back(format!("{{\"call\":\"7\",\"caller\":{FORGED_CLAIM}}}"));
    fake.verified.borrow_mut().insert(7, verified_caller());
    fake.requests
        .borrow_mut()
        .insert(7, VecDeque::from([Bytes::from_static(b"request")]));
    let (session, leader) = serve_session(fake).await;
    let calls = Rc::new(RefCell::new(Vec::new()));
    session
        .serve_org(
            "svc.accept-b".to_string(),
            "same-org".to_string(),
            recording_handler(&calls),
            serve_opts(),
        )
        .expect("the registration starts");
    wait_ms(300).await;

    let seen = calls.borrow().clone();
    assert_eq!(
        seen.len(),
        1,
        "the tampered claim still dispatches the real call — once"
    );
    assert_eq!(
        seen[0],
        verified_caller(),
        "the handler must see the leader-verified identity"
    );
    assert_ne!(
        seen[0], FORGED_CLAIM,
        "pre-fix the envelope claim was served verbatim"
    );
    session.close();
    leader.close();
    settle().await;
}

/// LEAF-14, end to end: a permanent serve-registration refusal (the
/// name is already served) reaches the page as a typed error, once,
/// with no retry storm. Pre-fix the re-declare loop misdiagnosed it
/// as transient and re-registered every 50ms forever, and the page
/// never learned why its service was not live.
#[wasm_bindgen_test]
async fn a_permanent_serve_registration_refusal_reaches_the_page_without_a_retry_storm() {
    let fake = ServeFake {
        refuse_register: Some(ProxyFailure::Typed(LeafError::Session(
            "serve \"svc\": AlreadyServed".to_string(),
        ))),
        ..Default::default()
    };
    let (session, leader) = serve_session(fake.clone()).await;
    let session_events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&session_events);
    let listener = Closure::wrap(Box::new(move |json: JsValue| {
        if let Some(text) = json.as_string() {
            sink.borrow_mut().push(text);
        }
    }) as Box<dyn FnMut(JsValue)>);
    session.on_event(listener.as_ref().unchecked_ref::<Function>().clone());
    listener.forget();

    session
        .serve_org(
            "svc.refused".to_string(),
            "same-org".to_string(),
            recording_handler(&Rc::new(RefCell::new(Vec::new()))),
            serve_opts(),
        )
        .expect("the handle is returned; the refusal is async");
    wait_ms(400).await;

    assert_eq!(
        fake.registers.get(),
        1,
        "one attempt per permanent refusal — pre-fix the loop retried every 50ms"
    );
    let events = session_events.borrow().clone();
    assert!(
        events
            .iter()
            .any(|event| event.contains("serve_registration_refused")
                && event.contains("AlreadyServed")),
        "the typed permanent refusal reaches the page: {events:?}"
    );
    session.close();
    leader.close();
    settle().await;
}
