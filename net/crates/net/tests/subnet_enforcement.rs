//! Integration tests for subnet visibility enforcement on the
//! publish + subscribe paths (Stage D-1 of `SDK_SECURITY_SURFACE_PLAN.md`).
//!
//! The original plan sketched a three-node test with A `[3,7,2]`
//! / B `[3,7,3]` / C `[3,8,1]` and claimed `SubnetLocal` delivers
//! A↔B. That interpretation doesn't match the actual `Visibility`
//! enum — `SubnetLocal` is strict same-subnet (see
//! `channel/config.rs`), so A/B with differing level-2 bytes are
//! NOT partitioned together.
//!
//! This test uses the semantics the core actually implements:
//!   - `SubnetLocal` — exact same `SubnetId` only.
//!   - `ParentVisible` — same subnet, or ancestor/descendant pair.
//!   - `Global` — any peer.
//!
//! Run: `cargo test --features net --test subnet_enforcement`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PublishConfig, Reliability, SocketBufferConfig, SubnetId,
    SubnetPolicy, SubnetRule, Visibility,
};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

/// Build the same `SubnetPolicy` every node uses. Three rules map
/// `region:<x>` / `fleet:<x>` / `unit:<x>` tags onto levels 0–2.
/// Mesh-wide consistency is assumed — mismatched policies lead to
/// asymmetric views of peer subnets (documented in SUBNET_ENFORCEMENT_PLAN).
fn shared_policy() -> Arc<SubnetPolicy> {
    let region = SubnetRule::new("region:", 0).map("us", 3);
    let fleet = SubnetRule::new("fleet:", 1).map("blue", 7).map("green", 8);
    let unit = SubnetRule::new("unit:", 2)
        .map("alpha", 2)
        .map("beta", 3)
        .map("gamma", 1);
    Arc::new(
        SubnetPolicy::new()
            .add_rule(region)
            .add_rule(fleet)
            .add_rule(unit),
    )
}

fn test_config(subnet: SubnetId, policy: Arc<SubnetPolicy>) -> MeshNodeConfig {
    // Bind via `127.0.0.1:0` so the OS picks a free port — no
    // pre-bind reservation, no TOCTOU race with parallel tests.
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_capability_gc_interval(Duration::from_millis(250))
        .with_subnet(subnet)
        .with_subnet_policy(policy);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

/// Build a MeshNode and pre-install a shared channel-config
/// registry so subscribers can be authorized per-channel.
async fn build_node(
    subnet: SubnetId,
    policy: Arc<SubnetPolicy>,
    registry: Arc<ChannelConfigRegistry>,
) -> Arc<MeshNode> {
    let cfg = test_config(subnet, policy);
    let keypair = EntityKeypair::generate();
    let mut node = MeshNode::new(keypair, cfg).await.expect("MeshNode::new");
    node.set_channel_configs(registry);
    Arc::new(node)
}

/// Connect A→B and start both. `start()` is idempotent so calling
/// it again for a second pairing with the same A is fine.
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

fn caps_for(region: &str, fleet: &str, unit: &str) -> CapabilitySet {
    CapabilitySet::new()
        .add_tag(format!("region:{region}"))
        .add_tag(format!("fleet:{fleet}"))
        .add_tag(format!("unit:{unit}"))
}

/// End-to-end three-node test. A and B share exact subnet
/// `[3,7,2]`; C is on `[3,8,1]`. A `SubnetLocal` channel on A
/// must deliver to B and reject C.
#[tokio::test]
async fn subnet_local_partitions_a_b_from_c() {
    let policy = shared_policy();

    let a_registry = Arc::new(ChannelConfigRegistry::new());
    let b_registry = Arc::new(ChannelConfigRegistry::new());
    let c_registry = Arc::new(ChannelConfigRegistry::new());

    // A and B both on exact subnet [3,7,2]; C on [3,8,1].
    let shared_subnet = SubnetId::new(&[3, 7, 2]);
    let a = build_node(shared_subnet, policy.clone(), a_registry.clone()).await;
    let b = build_node(shared_subnet, policy.clone(), b_registry).await;
    let c = build_node(SubnetId::new(&[3, 8, 1]), policy.clone(), c_registry).await;

    // Hub topology — A connects to B and C.
    handshake(&a, &b).await;
    handshake(&a, &c).await;

    // Each node announces caps that round-trip through `policy` to
    // the subnet it was configured with. A + B share unit:alpha so
    // both derive level 2 = 2; C uses unit:gamma for level 2 = 1.
    a.announce_capabilities(caps_for("us", "blue", "alpha"))
        .await
        .expect("A announce");
    b.announce_capabilities(caps_for("us", "blue", "alpha"))
        .await
        .expect("B announce");
    c.announce_capabilities(caps_for("us", "green", "gamma"))
        .await
        .expect("C announce");

    // Wait until A has indexed both peers' announcements (proxy for
    // `peer_subnets` being populated — they're updated in the same
    // handler).
    let b_id = b.node_id();
    let c_id = c.node_id();
    let learned = wait_until(|| {
        a.test_capability_fold_has(b_id) && a.test_capability_fold_has(c_id)
    })
    .await;
    assert!(
        learned,
        "A never indexed both B's and C's capability announcements"
    );

    // Register a SubnetLocal channel on A. `get_by_name` is
    // DashMap-backed, so A's authorize_subscribe will see it
    // synchronously.
    let channel_name = ChannelName::new("lab/metrics").expect("channel name");
    let chan_cfg = ChannelConfig::new(ChannelId::new(channel_name.clone()))
        .with_visibility(Visibility::SubnetLocal);
    a_registry.insert(chan_cfg);

    let a_id = a.node_id();

    // B subscribes — same subnet as A → accepted.
    b.subscribe_channel(a_id, channel_name.clone())
        .await
        .expect("B subscribe should be accepted");

    // C subscribes — different subnet → A's authorize_subscribe
    // rejects with Unauthorized.
    let c_result = c.subscribe_channel(a_id, channel_name.clone()).await;
    assert!(
        c_result.is_err(),
        "C's subscribe should have been rejected under SubnetLocal"
    );

    // A publishes. Only B is on the roster (C was rejected at
    // subscribe), AND the publish filter independently confirms
    // C's subnet would be dropped anyway. attempted == 1.
    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"ok"))
        .await
        .expect("publish");
    assert_eq!(
        report.attempted, 1,
        "only B should be attempted (C filtered)"
    );
    assert_eq!(report.delivered, 1, "B should have received the payload");
}

/// `ParentVisible` lets A and a descendant `[3,7,2,1]` see each
/// other but excludes a sibling at a different level-1 prefix.
#[tokio::test]
async fn parent_visible_admits_descendant_rejects_sibling() {
    let policy = shared_policy();
    let a_registry = Arc::new(ChannelConfigRegistry::new());
    let b_registry = Arc::new(ChannelConfigRegistry::new());
    let c_registry = Arc::new(ChannelConfigRegistry::new());

    let a = build_node(
        SubnetId::new(&[3, 7, 2]),
        policy.clone(),
        a_registry.clone(),
    )
    .await;
    // Descendant: shares A's first three levels, adds one more.
    let descendant = build_node(SubnetId::new(&[3, 7, 2, 5]), policy.clone(), b_registry).await;
    // Sibling at level 1 — breaks the ancestor chain.
    let sibling = build_node(SubnetId::new(&[3, 9, 1]), policy.clone(), c_registry).await;

    handshake(&a, &descendant).await;
    handshake(&a, &sibling).await;

    a.announce_capabilities(caps_for("us", "blue", "alpha"))
        .await
        .expect("A announce");
    descendant
        .announce_capabilities(caps_for("us", "blue", "alpha"))
        .await
        .expect("desc announce");
    sibling
        .announce_capabilities(caps_for("us", "green", "gamma"))
        .await
        .expect("sibling announce");

    let desc_id = descendant.node_id();
    let sib_id = sibling.node_id();
    let learned = wait_until(|| {
        a.test_capability_fold_has(desc_id) && a.test_capability_fold_has(sib_id)
    })
    .await;
    assert!(learned, "A did not learn both peers' announcements");

    let channel_name = ChannelName::new("lab/parent").expect("channel name");
    a_registry.insert(
        ChannelConfig::new(ChannelId::new(channel_name.clone()))
            .with_visibility(Visibility::ParentVisible),
    );

    let a_id = a.node_id();
    // Descendant: A is an ancestor of `[3,7,2,5]` → accepted.
    descendant
        .subscribe_channel(a_id, channel_name.clone())
        .await
        .expect("descendant subscribe accepted");
    // Sibling at level 1 has no ancestor relationship to A → rejected.
    let sibling_result = sibling.subscribe_channel(a_id, channel_name.clone()).await;
    assert!(
        sibling_result.is_err(),
        "sibling subscribe should have been rejected under ParentVisible"
    );

    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"pv"))
        .await
        .expect("publish");
    assert_eq!(report.attempted, 1, "only descendant should be attempted");
    assert_eq!(report.delivered, 1);
}

/// Regression: with no `SubnetPolicy`, the mesh falls back to
/// `SubnetId::GLOBAL` for every peer, so `SubnetLocal` channels
/// still deliver as if there were no partitioning.
#[tokio::test]
async fn without_policy_subnet_local_delivers_everywhere() {
    let a_registry = Arc::new(ChannelConfigRegistry::new());
    let b_registry = Arc::new(ChannelConfigRegistry::new());

    // No policy, default subnet (GLOBAL) on both nodes.
    let a_cfg = test_config_no_policy();
    let b_cfg = test_config_no_policy();
    let mut a_owned = MeshNode::new(EntityKeypair::generate(), a_cfg)
        .await
        .unwrap();
    a_owned.set_channel_configs(a_registry.clone());
    let a = Arc::new(a_owned);
    let mut b_owned = MeshNode::new(EntityKeypair::generate(), b_cfg)
        .await
        .unwrap();
    b_owned.set_channel_configs(b_registry);
    let b = Arc::new(b_owned);

    handshake(&a, &b).await;

    let channel_name = ChannelName::new("lab/noisy").expect("channel name");
    a_registry.insert(
        ChannelConfig::new(ChannelId::new(channel_name.clone()))
            .with_visibility(Visibility::SubnetLocal),
    );

    // Both peers are GLOBAL → same_subnet(GLOBAL, GLOBAL) is true →
    // subscribe accepted.
    b.subscribe_channel(a.node_id(), channel_name.clone())
        .await
        .expect("B subscribe accepted");

    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.delivered, 1);
}

fn test_config_no_policy() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

// ────────────────────────────────────────────────────────────────────
// default_visibility knob — pins the fail-open (Global) back-compat
// default and the fail-closed (SubnetLocal) operator opt-in. Pre-flight
// .unwrap_or audit (FAILURE_PATH_HARDENING_PLAN) replaced a hard-coded
// `Visibility::Global` with `config.default_visibility`; these tests
// pin both paths so a future refactor can't silently flip the default.
// ────────────────────────────────────────────────────────────────────

/// Build a node on a given subnet with an explicit
/// `default_visibility`, no policy attached. Peer subnets then
/// stay at `SubnetId::GLOBAL` (no policy → no derivation), which
/// is exactly the `unregistered-channel-meets-unknown-peer-subnet`
/// case the knob targets.
async fn build_node_with_default_visibility(
    subnet: SubnetId,
    default_visibility: Visibility,
) -> (Arc<MeshNode>, Arc<ChannelConfigRegistry>) {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(2))
        .with_subnet(subnet)
        .with_default_visibility(default_visibility);
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    let mut node = MeshNode::new(EntityKeypair::generate(), cfg).await.unwrap();
    let registry = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(registry.clone());
    (Arc::new(node), registry)
}

/// Register a channel on A, let B subscribe through the
/// membership handshake, then remove A's registry entry. The
/// roster retains B, but a subsequent publish finds no config
/// for the channel and falls back to `default_visibility`.
/// This is the exact path the knob guards — a channel that was
/// configured at deployment but later dropped from the registry
/// (or never registered on a node that publishes across a
/// replicated registry).
async fn subscribe_then_unregister(
    a: &Arc<MeshNode>,
    a_reg: &Arc<ChannelConfigRegistry>,
    b: &Arc<MeshNode>,
    channel_name: &ChannelName,
) {
    a_reg.insert(
        ChannelConfig::new(ChannelId::new(channel_name.clone()))
            .with_visibility(Visibility::Global),
    );
    b.subscribe_channel(a.node_id(), channel_name.clone())
        .await
        .expect("B subscribe");
    // Now the roster has B. Drop A's entry so the next publish
    // hits the cfg_snapshot=None fallback.
    a_reg.remove_by_name(channel_name.as_str());
}

/// Default behavior: `default_visibility = Global`. A publish on
/// a channel whose registry entry is missing at publish time
/// still delivers cross-subnet — fail-open preserves back-compat.
#[tokio::test]
async fn default_visibility_global_delivers_across_subnets_on_unregistered_publish() {
    let a_subnet = SubnetId::new(&[3, 7, 2]);
    let b_subnet = SubnetId::new(&[3, 8, 1]);
    let (a, a_reg) = build_node_with_default_visibility(a_subnet, Visibility::Global).await;
    let (b, _b_reg) = build_node_with_default_visibility(b_subnet, Visibility::Global).await;

    handshake(&a, &b).await;

    let channel_name = ChannelName::new("lab/unregistered-global").expect("channel name");
    subscribe_then_unregister(&a, &a_reg, &b, &channel_name).await;

    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"hi"))
        .await
        .expect("publish");
    assert_eq!(
        report.delivered, 1,
        "default_visibility=Global must admit cross-subnet delivery \
         when the channel config is missing at publish time (back-compat default)",
    );
}

/// Fail-closed: `default_visibility = SubnetLocal`. Same
/// subscribe-then-drop setup, but A is configured so a missing
/// registry entry confines publishes to the local subnet. B —
/// on a different subnet — is filtered out, guarding against
/// accidental cross-subnet leakage when a channel config is
/// lost or never propagated.
#[tokio::test]
async fn default_visibility_subnet_local_filters_unregistered_publish_cross_subnet() {
    let a_subnet = SubnetId::new(&[3, 7, 2]);
    let b_subnet = SubnetId::new(&[3, 8, 1]);
    let (a, a_reg) = build_node_with_default_visibility(a_subnet, Visibility::SubnetLocal).await;
    let (b, _b_reg) = build_node_with_default_visibility(b_subnet, Visibility::Global).await;

    handshake(&a, &b).await;

    let channel_name = ChannelName::new("lab/unregistered-strict").expect("channel name");
    subscribe_then_unregister(&a, &a_reg, &b, &channel_name).await;

    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"secret"))
        .await
        .expect("publish");
    // A's local subnet is [3,7,2]; B (no policy) is indexed as
    // `SubnetId::GLOBAL` from A's view. `SubnetLocal` requires
    // exact equality, so B is filtered before the send loop.
    // Filtered subscribers don't count as attempted or
    // delivered — they're policy decisions, not failures.
    assert_eq!(
        report.delivered, 0,
        "default_visibility=SubnetLocal must filter cross-subnet delivery \
         for a channel whose config is missing at publish time; got delivered={}",
        report.delivered,
    );
    assert_eq!(
        report.attempted, 0,
        "policy-filtered subscribers must not count as attempted",
    );
}

/// Back-compat: an explicit registry entry always wins, even when
/// `default_visibility=SubnetLocal`. A channel that's been
/// registered with `Visibility::Global` must still deliver
/// cross-subnet — the knob is only a fallback for UNREGISTERED
/// channels.
#[tokio::test]
async fn registered_visibility_overrides_default_visibility_knob() {
    let a_subnet = SubnetId::new(&[3, 7, 2]);
    let b_subnet = SubnetId::new(&[3, 8, 1]);
    let (a, a_reg) = build_node_with_default_visibility(a_subnet, Visibility::SubnetLocal).await;
    let (b, _b_reg) = build_node_with_default_visibility(b_subnet, Visibility::Global).await;

    handshake(&a, &b).await;

    let channel_name = ChannelName::new("lab/explicit").expect("channel name");
    // Registered with Global explicitly — this must win over the
    // SubnetLocal default.
    a_reg.insert(
        ChannelConfig::new(ChannelId::new(channel_name.clone()))
            .with_visibility(Visibility::Global),
    );

    b.subscribe_channel(a.node_id(), channel_name.clone())
        .await
        .expect("B subscribe");

    let publisher = ChannelPublisher::new(
        channel_name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .publish(&publisher, bytes::Bytes::from_static(b"open"))
        .await
        .expect("publish");
    assert_eq!(
        report.delivered, 1,
        "an explicit registry entry with Visibility::Global must win over \
         default_visibility=SubnetLocal — the knob is a fallback, not a floor",
    );
}

/// SubnetId geometry sanity — the invariants the enforcement
/// paths lean on. Co-located here so a refactor of the core
/// helpers surfaces a failure in the same test file as the
/// enforcement that depends on them.
#[tokio::test]
async fn subnet_geometry_invariants() {
    let a = SubnetId::new(&[3, 7, 2]);
    let b_exact = SubnetId::new(&[3, 7, 2]);
    let diff_level2 = SubnetId::new(&[3, 7, 3]);
    let diff_level1 = SubnetId::new(&[3, 8, 1]);
    let descendant = SubnetId::new(&[3, 7, 2, 5]);

    // is_same_subnet is exact equality.
    assert!(a.is_same_subnet(b_exact));
    assert!(!a.is_same_subnet(diff_level2));

    // is_ancestor_of walks the prefix — [3,7,2] is ancestor of
    // [3,7,2,5] but not of [3,7,3] (siblings) or [3,8,1].
    assert!(a.is_ancestor_of(descendant));
    assert!(!a.is_ancestor_of(diff_level2));
    assert!(!a.is_ancestor_of(diff_level1));

    // Global is every subnet's ancestor (zero-mask matches everything).
    assert!(SubnetId::GLOBAL.is_ancestor_of(a));
    assert!(SubnetId::GLOBAL.is_ancestor_of(diff_level1));
}
