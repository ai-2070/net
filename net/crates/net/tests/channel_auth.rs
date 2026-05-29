//! Integration tests for channel authentication enforcement
//! (Stage E of `SDK_SECURITY_SURFACE_PLAN.md`). Exercises the
//! cap-filter + token paths end-to-end through the publish /
//! subscribe hot paths.
//!
//! Run: `cargo test --features net --test channel_auth`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, OnFailure, PermissionToken, PublishConfig, Reliability,
    SocketBufferConfig, TokenCache, TokenScope,
};

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

/// A test node bundle: the mesh + its keypair (so tests can issue
/// tokens signed by this node) + the channel registry. The node's
/// `TokenCache` is installed on the mesh during construction but
/// not surfaced here — tests don't need to poke it directly.
struct Node {
    mesh: Arc<MeshNode>,
    keypair: EntityKeypair,
    registry: Arc<ChannelConfigRegistry>,
}

async fn build_node() -> Node {
    let keypair = EntityKeypair::generate();
    let cfg = test_config();
    let mut node = MeshNode::new(keypair.clone(), cfg)
        .await
        .expect("MeshNode::new");
    let registry = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(registry.clone());
    node.set_token_cache(Arc::new(TokenCache::new()));
    Node {
        mesh: Arc::new(node),
        keypair,
        registry,
    }
}

/// Handshake A↔B without starting either node.
async fn handshake_no_start(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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

/// Handshake + start + both nodes announce capabilities so the
/// publisher's peer_entity_ids has the subscriber's EntityId before
/// any subscribe attempt.
async fn setup_pair(a_caps: CapabilitySet, b_caps: CapabilitySet) -> (Node, Node) {
    let a = build_node().await;
    let b = build_node().await;
    handshake_no_start(&a.mesh, &b.mesh).await;
    a.mesh.start();
    b.mesh.start();

    a.mesh
        .announce_capabilities(a_caps)
        .await
        .expect("A announce");
    b.mesh
        .announce_capabilities(b_caps)
        .await
        .expect("B announce");

    // Wait until A sees B's entity via the capability index — same
    // dispatch populates peer_entity_ids.
    let b_id = b.mesh.node_id();
    let learned = wait_until(|| a.mesh.test_capability_fold_has(b_id)).await;
    assert!(learned, "A never indexed B's capability announcement");

    (a, b)
}

#[tokio::test]
async fn subscribe_denied_by_cap_filter() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/gpu").unwrap();
    let filter = CapabilityFilter::new().require_tag("gpu");
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())).with_subscribe_caps(filter));

    let result = b.mesh.subscribe_channel(a.mesh.node_id(), name).await;
    assert!(
        result.is_err(),
        "subscribe should have been denied for missing subscribe_caps"
    );
}

#[tokio::test]
async fn subscribe_denied_by_missing_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/secret").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // B subscribes with no token.
    let result = b.mesh.subscribe_channel(a.mesh.node_id(), name).await;
    assert!(
        result.is_err(),
        "subscribe should have been denied for missing token"
    );
}

#[tokio::test]
async fn subscribe_accepted_with_valid_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/signed").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Publisher issues a SUBSCRIBE token for B's entity. Duration
    // is generous so the test isn't timing-sensitive.
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    b.mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await
        .expect("subscribe should be accepted with a valid token");
}

#[tokio::test]
async fn subscribe_rejected_with_expired_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/short").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // 1-second token, then sleep past `not_after`.
    // (duration_secs == 0 is now rejected; mint with the minimum
    // valid TTL and wait it out.)
    let token = PermissionToken::issue(
        &a.keypair,
        b.keypair.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        1,
        0,
    );
    // Let now() tick past `not_after`.
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(result.is_err(), "expired token should not authorize");
}

#[tokio::test]
async fn subscribe_rejected_with_wrong_subject_token() {
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/wrong").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Token issued for a THIRD entity, not B.
    let bystander = EntityKeypair::generate();
    let token = PermissionToken::issue(
        &a.keypair,
        bystander.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    // B presents it. The chain roots at A (a channel root), but its
    // leaf subject is `bystander`, not B (the presenter), so the
    // leaf-binding check in `TokenChain::verify_authorizes` rejects it.
    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(
        result.is_err(),
        "token issued for a different subject must not authorize B"
    );
}

#[tokio::test]
async fn rejected_subscribe_retains_no_chain_and_no_cache_entry() {
    // An unauthorized subscribe must leave the publisher holding no
    // state for the rejected peer: not in the shared `TokenCache` (the
    // original DoS vector — self-signed tokens spammed into the cache
    // before the ACL check) and, post root-anchoring, not in
    // `subscriber_chains` either (a retained chain would be re-checked
    // by the sweep / publish path and is a memory-growth vector under
    // rejected-subscribe spam). The chain store is the live guard now:
    // the shared cache is no longer written on the subscribe path at
    // all, so asserting only on it is vacuous.
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/leak").unwrap();
    a.registry.insert(
        ChannelConfig::new(ChannelId::new(name.clone()))
            .with_token_roots(vec![a.keypair.entity_id().clone()]),
    );

    // Pre-test: publisher retains nothing.
    let shared_cache = a
        .mesh
        .token_cache()
        .cloned()
        .expect("publisher should have a shared token cache");
    assert_eq!(shared_cache.len(), 0, "precondition: empty cache");
    assert_eq!(
        a.mesh.subscriber_chain_count(),
        0,
        "precondition: no retained chains"
    );

    // B signs a token intended for a THIRD bystander entity, not
    // itself. The token is signature-valid but unauthorized for B.
    let bystander = EntityKeypair::generate();
    let token = PermissionToken::issue(
        &a.keypair,
        bystander.entity_id().clone(),
        TokenScope::SUBSCRIBE,
        name.hash(),
        300,
        0,
    );

    let result = b
        .mesh
        .subscribe_channel_with_token(a.mesh.node_id(), name, token)
        .await;
    assert!(result.is_err(), "unauthorized subscribe must be rejected");

    // Post-test: a rejected subscribe must retain no chain (the live
    // regression guard) and still touch nothing in the shared cache.
    assert_eq!(
        a.mesh.subscriber_chain_count(),
        0,
        "rejected subscribe must not retain a token chain"
    );
    assert_eq!(
        shared_cache.len(),
        0,
        "rejected subscribe must not populate the shared token cache"
    );
}

#[tokio::test]
async fn publish_denied_by_own_cap_filter() {
    let (a, b) = setup_pair(
        CapabilitySet::new(), // A has NO `admin` tag
        CapabilitySet::new(),
    )
    .await;

    let name = ChannelName::new("lab/admin-only").unwrap();
    let filter = CapabilityFilter::new().require_tag("admin");
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())).with_publish_caps(filter));

    // Suppress unused-variable warning; test just needs `b` alive.
    let _ = b;

    let publisher = ChannelPublisher::new(
        name.clone(),
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let result = a.mesh.publish(&publisher, Bytes::from_static(b"x")).await;
    assert!(
        result.is_err(),
        "publisher without required caps must not publish"
    );
}

#[tokio::test]
async fn unauth_channel_accepts_everyone() {
    // Backwards-compat regression: no subscribe_caps, no
    // publish_caps, no require_token → open channel.
    let (a, b) = setup_pair(CapabilitySet::new(), CapabilitySet::new()).await;

    let name = ChannelName::new("lab/open").unwrap();
    a.registry
        .insert(ChannelConfig::new(ChannelId::new(name.clone())));

    b.mesh
        .subscribe_channel(a.mesh.node_id(), name.clone())
        .await
        .expect("open channel must accept any subscriber");

    let publisher = ChannelPublisher::new(
        name,
        PublishConfig {
            reliability: Reliability::FireAndForget,
            on_failure: OnFailure::BestEffort,
            max_inflight: 16,
        },
    );
    let report = a
        .mesh
        .publish(&publisher, Bytes::from_static(b"hello"))
        .await
        .expect("open-channel publish");
    assert_eq!(report.attempted, 1);
    assert_eq!(report.delivered, 1);
}

#[tokio::test]
async fn tampered_announcement_signature_rejected() {
    use net::adapter::net::behavior::capability::CapabilityAnnouncement;

    // Direct regression for the CapabilityAnnouncement sign/verify
    // round-trip (E-1). No mesh, pure data-structure test.
    let kp = EntityKeypair::generate();
    let mut ann = CapabilityAnnouncement::new(
        kp.node_id(),
        kp.entity_id().clone(),
        1,
        CapabilitySet::new().add_tag("ok"),
    );
    ann.sign(&kp);
    assert!(ann.verify().is_ok(), "fresh signature must verify");

    // Tamper: flip a byte in the capability set (fields are
    // JSON-serialized inside the signed region, so a tag swap
    // invalidates the signature).
    let mut tampered = ann.clone();
    tampered.capabilities = CapabilitySet::new().add_tag("tampered");
    assert!(
        tampered.verify().is_err(),
        "tampered announcement must fail verification"
    );
}
