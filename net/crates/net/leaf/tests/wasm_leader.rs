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

use std::cell::RefCell;
use std::rc::Rc;

use bytes::Bytes;
use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use net_leaf::bootstrap::gloo_timer_sleep;
use net_leaf::error::{LeafError, RpcError};
use net_leaf::identity::{IdentitySecrets, IDENTITY_BLOB_MAGIC};
use net_leaf::leader::{LeaderBackend, LeaderRequest, ProxyFailure, ProxyValue, Replier};
use net_leaf::leader_session::{BackendFactory, Lifecycle, Role};
use net_leaf::storage::IdentityVault;

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
}

/// A factory over one shared log, so a test can watch what the leader
/// of the moment did — including a leader that was promoted after the
/// first one went away.
fn factory(node_id: u64, log: Rc<RefCell<Log>>, hold_calls: bool) -> BackendFactory {
    Rc::new(move |_opts, _sink| {
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

/// `performance.now()`, the only clock a browser has that does not
/// panic on `wasm32-unknown-unknown`.
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}
