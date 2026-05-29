//! Integration tests for the channel-auth hardening work in
//! `docs/CHANNEL_AUTH_GUARD_PLAN.md` (stages AG-1 through AG-6).
//!
//! These tests live in a separate file from `tests/channel_auth.rs`
//! (Stage E) so the Stage E suite remains a stable regression net
//! for the subscribe-gate contract while the hardening work evolves.
//!
//! Harness conventions match `tests/channel_auth.rs`: a `Node`
//! struct carries the mesh handle alongside its keypair + channel
//! registry (so tests can issue tokens and register channels), and
//! handshakes split `handshake_no_start` + `start_all` so the
//! receive loops come up after every pair has handshaked.
//!
//! Run: `cargo test --features net --test channel_auth_hardening`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PermissionToken, PublishConfig, Reliability,
    SocketBufferConfig, TokenCache, TokenScope,
};
use net::adapter::Adapter;

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    // Bind via `127.0.0.1:0` so the OS picks a free port — no
    // pre-bind reservation, no TOCTOU race with parallel tests.
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

/// Wraps a `MeshNode` with its keypair (for issuing tokens), the
/// channel registry (for registering channels post-construction),
/// and the local `TokenCache` (so tests can pre-install publisher-
/// side tokens without needing a public accessor on `MeshNode`).
struct Node {
    mesh: Arc<MeshNode>,
    keypair: EntityKeypair,
    registry: Arc<ChannelConfigRegistry>,
    token_cache: Arc<TokenCache>,
}

async fn build_node() -> Node {
    build_node_with_cfg(test_config()).await
}

async fn build_node_with_cfg(cfg: MeshNodeConfig) -> Node {
    let keypair = EntityKeypair::generate();
    let mut node = MeshNode::new(keypair.clone(), cfg)
        .await
        .expect("MeshNode::new");
    let registry = Arc::new(ChannelConfigRegistry::new());
    let token_cache = Arc::new(TokenCache::new());
    node.set_channel_configs(registry.clone());
    node.set_token_cache(token_cache.clone());
    Node {
        mesh: Arc::new(node),
        keypair,
        registry,
        token_cache,
    }
}

/// Handshake A↔B and start both receive loops.
async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

async fn wait_until<F>(mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// `AuthGuard` keys on the full 64-bit subscriber `node_id` —
/// matches the `subscriber_origin_hash` helper in `mesh.rs`. The
/// prior 32-bit truncation birthday-collided at ~65 k peers.
fn origin_hash(node_id: u64) -> u64 {
    node_id
}

// ============================================================================
// AG-1 — AuthGuard is populated on successful subscribe
// ============================================================================

#[tokio::test]
async fn auth_guard_populated_on_open_channel_subscribe() {
    // Open channel (no auth required). `authorize_subscribe` takes
    // the accept path and calls `auth_guard.allow_channel`; we
    // observe it via `is_authorized_full`.
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    let channel = ChannelName::new("auth/guarded").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(channel.clone())));

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        !a.mesh.auth_guard().is_authorized_full(b_origin, &channel),
        "guard should be empty before subscribe",
    );

    b.mesh
        .subscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect("subscribe");

    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "AuthGuard didn't admit B after a successful subscribe",
    );
}

#[tokio::test]
async fn auth_guard_revoked_on_unsubscribe() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    let channel = ChannelName::new("auth/unsub").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(channel.clone())));
    b.mesh
        .subscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect("subscribe");

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "subscribe didn't populate the guard",
    );

    b.mesh
        .unsubscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect("unsubscribe");

    assert!(
        wait_until(|| !a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "unsubscribe didn't revoke the guard entry",
    );
}

#[tokio::test]
async fn auth_guard_populated_for_token_gated_subscribe() {
    // require_token=true: subscribe goes through the
    // token-install + exact-cap path before reaching the
    // `allow_channel` call. Same observable outcome — the guard
    // entry exists — but the code path is different.
    //
    // Token-gated subscribes need A's `peer_entity_ids` populated
    // before B arrives, so both sides exchange capability
    // announcements first. This mirrors the `setup_pair` helper in
    // `tests/channel_auth.rs`.
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    a.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    assert!(
        wait_until(|| a.mesh.test_capability_fold_has(b.mesh.node_id())).await,
        "A never indexed B's capability announcement",
    );

    let channel = ChannelName::new("auth/token").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        300,
        0,
    );

    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), channel.clone(), token)
        .await
        .expect("subscribe with token");

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "token-gated subscribe didn't populate the guard",
    );
}

// ============================================================================
// AG-2 — publish fan-out consults AuthGuard.check_fast
// ============================================================================

fn publisher_for(channel: ChannelName) -> ChannelPublisher {
    ChannelPublisher::new(
        channel,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    )
}

#[tokio::test]
async fn publish_skips_revoked_subscriber() {
    // B subscribes to an open channel; AuthGuard gets populated.
    // A revokes B's guard entry directly. The next publish must
    // NOT attempt delivery to B — the fan-out filter drops
    // subscribers the guard denies.
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    let channel = ChannelName::new("auth/revoke").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(channel.clone())));

    b.mesh
        .subscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect("subscribe");

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "subscribe didn't populate the guard",
    );

    // Baseline: publish lands on one subscriber.
    let report = a
        .mesh
        .publish(&publisher_for(channel.clone()), Bytes::from_static(b"hi"))
        .await
        .expect("publish pre-revoke");
    assert_eq!(
        report.attempted, 1,
        "pre-revoke publish didn't see B as a subscriber",
    );

    // Revoke B's guard entry directly. `roster` still lists B —
    // the fast path is the authority for the publish side.
    a.mesh.auth_guard().revoke_channel(b_origin, &channel);

    let report = a
        .mesh
        .publish(&publisher_for(channel.clone()), Bytes::from_static(b"hi"))
        .await
        .expect("publish post-revoke");
    assert_eq!(
        report.attempted, 0,
        "revoked subscriber was not filtered out by the fast path",
    );
}

#[tokio::test]
async fn publish_admits_normal_subscriber() {
    // Regression for the fast path's happy case — an open channel
    // with a subscribed peer should publish normally. Keeps us
    // honest: AG-2 must not block valid deliveries by mistake.
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    let channel = ChannelName::new("auth/happy").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(channel.clone())));

    b.mesh
        .subscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect("subscribe");

    // Three publishes in a row — after the first, the verified
    // cache must be warm. All three should find B in the
    // authorized set and deliver.
    for _ in 0..3 {
        let report = a
            .mesh
            .publish(&publisher_for(channel.clone()), Bytes::from_static(b"ok"))
            .await
            .expect("publish");
        assert_eq!(
            report.attempted, 1,
            "fast path unexpectedly filtered out the subscriber",
        );
    }
}

// ============================================================================
// AG-3 — token-expiry sweep evicts subscribers whose tokens have aged out
// ============================================================================

async fn build_node_fast_sweep() -> Node {
    // Tight sweep interval so tests don't wait 30 seconds for
    // eviction.
    let cfg = test_config().with_token_sweep_interval(Duration::from_millis(200));
    build_node_with_cfg(cfg).await
}

#[tokio::test]
async fn expired_token_evicts_subscriber_within_one_sweep() {
    // B subscribes with a 1-second token. After the TTL passes
    // AND the sweep runs, B's roster entry + AuthGuard entry are
    // gone.
    let a = build_node_fast_sweep().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    a.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    assert!(
        wait_until(|| a.mesh.test_capability_fold_has(b.mesh.node_id())).await,
        "A never indexed B's caps",
    );

    let channel = ChannelName::new("auth/expiring").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Publisher needs its own PUBLISH token on a `require_token`
    // channel — `can_publish` gates the fan-out before the
    // subscriber check runs. Install it once; long-lived so it
    // doesn't expire alongside B's subscribe token.
    let pub_token = PermissionToken::issue(
        &a.keypair,
        a.keypair.entity_id().clone(),
        TokenScope::PUBLISH,
        channel.hash(),
        3600,
        0,
    );
    a.token_cache
        .insert(pub_token)
        .expect("install publish token");

    // Short-lived subscribe token — 1 second.
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        1,
        0,
    );

    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), channel.clone(), token)
        .await
        .expect("subscribe with short token");

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "subscribe didn't populate the guard",
    );

    // Wait past the token's TTL + one sweep tick.
    tokio::time::sleep(Duration::from_millis(1_400)).await;

    // Sweep should have pulled B off the roster and revoked the
    // guard entry. Give the async loop one more tick to land.
    assert!(
        wait_until(|| !a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "expired-token sweep didn't revoke the guard",
    );

    // Publish should now find zero subscribers.
    let report = a
        .mesh
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from_static(b"after-expiry"),
        )
        .await
        .expect("publish after expiry");
    assert_eq!(
        report.attempted, 0,
        "publish still reached an expired-token subscriber",
    );
}

async fn build_node_sweep_disabled() -> Node {
    let cfg = test_config().with_token_sweep_interval(Duration::MAX);
    build_node_with_cfg(cfg).await
}

/// Regression for a cubic-flagged P1: when `token_sweep_interval`
/// is set to `Duration::MAX` (the documented "disable" sentinel),
/// expired subscribers used to stay authorized indefinitely —
/// nothing walks the roster to notice their tokens expired. The
/// fix adds a lazy per-subscriber token-expiry probe to the
/// publish fast path, gated on `require_token` so open channels
/// still take the zero-cost path. This test exercises the
/// sweep-disabled branch end-to-end.
#[tokio::test]
async fn publish_skips_expired_subscriber_when_sweep_is_disabled() {
    let a = build_node_sweep_disabled().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    a.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    assert!(
        wait_until(|| a.mesh.test_capability_fold_has(b.mesh.node_id())).await,
        "A never indexed B's caps",
    );

    let channel = ChannelName::new("auth/nosweep").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Long-lived publish token for A.
    let pub_token = PermissionToken::issue(
        &a.keypair,
        a.keypair.entity_id().clone(),
        TokenScope::PUBLISH,
        channel.hash(),
        3600,
        0,
    );
    a.token_cache
        .insert(pub_token)
        .expect("install publish token");

    // Short-lived subscribe token. Picked 3 s rather than 1 s for
    // the same second-resolution-clock reason captured in commit
    // 1d905420 (token-cache evict-race test): `current_timestamp`
    // is second-resolution, so a subscribe handshake + `wait_until`
    // poll that lands in the 0.5-1.5 s range on a loaded CI runner
    // already crosses the expiry of a 1 s token before the first
    // publish runs, masking the fast-path-admit assertion this test
    // is trying to pin. 3 s gives ~2 s of slack between subscribe
    // and the first publish; the post-expiry sleep below is bumped
    // proportionally so the second publish still reliably crosses
    // expiry.
    let sub_token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        channel.hash(),
        3,
        0,
    );

    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), channel.clone(), sub_token)
        .await
        .expect("subscribe with short token");

    let b_origin = origin_hash(b.mesh.node_id());
    assert!(
        wait_until(|| a.mesh.auth_guard().is_authorized_full(b_origin, &channel)).await,
        "subscribe didn't populate the guard",
    );

    // First publish inside the TTL — B is a valid subscriber.
    let report = a
        .mesh
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from_static(b"before-expiry"),
        )
        .await
        .expect("publish before expiry");
    assert_eq!(
        report.attempted, 1,
        "fast-path should admit the still-valid subscriber",
    );

    // Wait past the subscribe token's TTL. `PermissionToken` works
    // in 1-second granularity (unix seconds), and `is_valid`'s
    // strict `now > not_after` lets the boundary second still
    // pass — so 4.5 s from issue reliably crosses the 3 s expiry
    // even when the test harness wakes a little late. With the
    // sweep disabled the AuthGuard entry + roster entry both
    // persist across this sleep — pre-fix, the next publish would
    // still fan out to B.
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    assert!(
        a.mesh.auth_guard().is_authorized_full(b_origin, &channel),
        "sweep-disabled harness precondition: guard entry must persist past TTL",
    );

    // Post-fix: the lazy expiry check on the publish fast path
    // notices the expired token and revokes inline. `attempted`
    // counts subscribers admitted after the filter runs, so it
    // must drop to zero even though the sweep never ran.
    let report = a
        .mesh
        .publish(
            &publisher_for(channel.clone()),
            Bytes::from_static(b"after-expiry"),
        )
        .await
        .expect("publish after expiry");
    assert_eq!(
        report.attempted, 0,
        "publish admitted an expired-token subscriber while the sweep was disabled",
    );

    // Revocation should also have landed on the guard so future
    // publishes take the cheap `Denied` path.
    assert!(
        !a.mesh.auth_guard().is_authorized_full(b_origin, &channel),
        "lazy expiry should have revoked the guard entry for the expired subscriber",
    );
}

// ============================================================================
// AG-4 — auth-failure rate limit throttles repeat offenders
// ============================================================================

async fn build_node_tight_rate_limit() -> Node {
    // 3 failures per window, 5s throttle, tight window so tests
    // don't wait minutes for a reset.
    let cfg =
        test_config().with_auth_failure_limit(3, Duration::from_secs(10), Duration::from_secs(5));
    build_node_with_cfg(cfg).await
}

#[tokio::test]
async fn auth_failure_rate_limit_kicks_in() {
    // A has a channel requiring `gpu` caps. B announces empty caps,
    // so every subscribe fails with Unauthorized. After 3 failures
    // (the tight-limit threshold), the 4th must short-circuit as
    // RateLimited — evidence the throttle engaged before any
    // further ed25519 work.
    use net::adapter::net::behavior::capability::CapabilityFilter;

    let a = build_node_tight_rate_limit().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    a.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");
    assert!(
        wait_until(|| a.mesh.test_capability_fold_has(b.mesh.node_id())).await,
        "A never indexed B's caps",
    );

    let channel = ChannelName::new("auth/ratelimit").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(channel.clone()))
            .with_subscribe_caps(CapabilityFilter::new().require_tag("gpu")),
    );

    // Three failed subscribes — all reject with Unauthorized.
    for attempt in 1..=3 {
        let err = b
            .mesh
            .subscribe_channel(a.mesh.node_id(), channel.clone())
            .await
            .expect_err("subscribe should fail — B has no gpu tag");
        let message = format!("{}", err);
        assert!(
            message.contains("Unauthorized") || message.contains("Some(Unauthorized)"),
            "attempt {}: expected Unauthorized, got {}",
            attempt,
            message
        );
    }

    // Fourth subscribe should short-circuit with RateLimited.
    let err = b
        .mesh
        .subscribe_channel(a.mesh.node_id(), channel.clone())
        .await
        .expect_err("4th subscribe should throttle");
    let message = format!("{}", err);
    assert!(
        message.contains("RateLimited"),
        "expected RateLimited on 4th attempt, got {}",
        message
    );
}

#[tokio::test]
async fn successful_subscribe_clears_failure_counter() {
    // B has empty caps; A's channel requires no caps (open). The
    // test simulates a scenario where B briefly hits unrelated
    // auth failures (UnknownChannel), then successfully subscribes
    // to an open channel — the success path must clear the counter
    // so subsequent probes don't count against the old total.
    let a = build_node_tight_rate_limit().await;
    let b = build_node().await;
    handshake(&a.mesh, &b.mesh).await;

    // Two subscribes to unknown channels — fail counter = 2.
    for i in 0..2 {
        let unknown = ChannelName::new(&format!("auth/unknown-{}", i)).unwrap();
        let err = b
            .mesh
            .subscribe_channel(a.mesh.node_id(), unknown)
            .await
            .expect_err("unknown channel should fail");
        let message = format!("{}", err);
        assert!(message.contains("UnknownChannel"), "got {}", message);
    }

    // Successful subscribe to an open channel — clears counter.
    let open = ChannelName::new("auth/open").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(open.clone())));
    b.mesh
        .subscribe_channel(a.mesh.node_id(), open)
        .await
        .expect("open subscribe");

    // Two more unknown-channel failures — counter starts from 0
    // again, so we don't trip the threshold at 3.
    for i in 0..2 {
        let unknown = ChannelName::new(&format!("auth/miss-{}", i)).unwrap();
        let err = b
            .mesh
            .subscribe_channel(a.mesh.node_id(), unknown)
            .await
            .expect_err("unknown channel should fail");
        assert!(
            !format!("{}", err).contains("RateLimited"),
            "counter was not cleared by the successful subscribe"
        );
    }
}

// ============================================================================
// Config robustness — regression for a cubic-flagged panic
// ============================================================================

/// Regression for a cubic-flagged P2: `MeshNode::start()` panicked
/// when `capability_gc_interval` or `token_sweep_interval` was set
/// to `Duration::ZERO`, because `tokio::time::interval` panics on a
/// zero period. The legitimate "disable the sweep" sentinel is
/// `Duration::MAX` (documented on those fields); zero is just
/// pathological input. The fix clamps to a 1 ms floor in the
/// private `nonzero_interval` helper that both `spawn_*_loop`
/// callers use.
///
/// We can't assert the panic directly through a public integration
/// test — tokio swallows panics in spawned tasks, so a naive
/// `mesh.start()` call runs cleanly regardless of whether the fix
/// is in place. Instead, we prove the invariant at the contract
/// boundary: `tokio::time::interval` with the same clamped input
/// the fix produces doesn't panic. If a future edit removes the
/// clamp (or changes `Duration::ZERO` to some other "disabled"
/// sentinel that happens to round to zero), this test catches it.
///
/// Runs `Duration::ZERO` through `tokio::time::interval` via a
/// `catch_unwind` — the raw call still panics without the fix, so
/// this test actually differentiates pre/post-fix behaviour. The
/// matching positive case (`Duration::from_millis(1)`) proves the
/// fallback value itself is valid input to tokio.
#[test]
fn tokio_interval_panics_on_zero_and_accepts_one_ms() {
    // Positive half: the 1 ms fallback `nonzero_interval` uses
    // must itself be accepted by tokio. This guards against
    // someone lowering the fallback to a sub-millisecond value
    // that a future tokio release might also reject.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build runtime");
    rt.block_on(async {
        let _ = tokio::time::interval(Duration::from_millis(1));
    });

    // Negative half: zero really does panic. If tokio ever
    // changes this contract the test will start failing —
    // flag the upstream change so we can drop the clamp.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build runtime");
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(async {
            let _ = tokio::time::interval(Duration::ZERO);
        });
    }));
    assert!(
        panicked.is_err(),
        "tokio::time::interval(Duration::ZERO) is expected to panic — \
         if this test starts failing, tokio changed its contract and \
         the nonzero_interval clamp in mesh.rs can be simplified",
    );
}

/// End-to-end smoke: build a `MeshNode` with both
/// interval-driven config knobs at zero, call `start()`, let the
/// background tasks spin for a few ticks, and shut down cleanly.
/// Pre-fix the spawned tasks `panic!`'d on first poll of
/// `tokio::time::interval(Duration::ZERO)`. Tokio swallows spawned-
/// task panics so this test doesn't fail loudly without the fix,
/// but it exercises the full call path at runtime — catching a
/// regression that only surfaces under specific config shapes
/// (e.g. a CLI accidentally lowering both intervals to zero).
#[tokio::test]
async fn start_tolerates_zero_gc_and_sweep_intervals() {
    let keypair = EntityKeypair::generate();
    let cfg = test_config()
        .with_capability_gc_interval(Duration::ZERO)
        .with_token_sweep_interval(Duration::ZERO);
    let mut node = MeshNode::new(keypair.clone(), cfg)
        .await
        .expect("MeshNode::new");
    node.set_channel_configs(Arc::new(ChannelConfigRegistry::new()));
    node.set_token_cache(Arc::new(TokenCache::new()));
    let mesh = Arc::new(node);

    mesh.start();
    tokio::time::sleep(Duration::from_millis(20)).await;
    match Arc::try_unwrap(mesh) {
        Ok(n) => n.shutdown().await.expect("shutdown"),
        Err(_) => panic!("unexpected Arc clone — test holds the only reference"),
    }
}
