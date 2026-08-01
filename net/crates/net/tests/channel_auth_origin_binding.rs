//! H3 regression (2026-07-31 channel-auth audit): a channel family whose
//! name encodes the subscriber's identity must admit only the peer that
//! identity belongs to.
//!
//! The motivating case is nRPC's `<service>.replies.<caller_origin>`.
//! Those resolve through a permissive *prefix* config, so pre-fix any
//! mesh peer could hold a live subscription to another caller's reply
//! channel and receive that caller's RPC response bodies whenever the
//! server's direct route missed and the response fell back to roster
//! fan-out.
//!
//! Run: `cargo test --features net --test channel_auth_origin_binding`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{
    ChannelConfig, ChannelConfigRegistry, ChannelId, ChannelName, EntityKeypair, MeshNode,
    MeshNodeConfig, OriginBinding, SocketBufferConfig,
};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x37u8; 32];
const PREFIX: &str = "svc.replies.";

fn test_config() -> MeshNodeConfig {
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

struct Node {
    mesh: Arc<MeshNode>,
    keypair: EntityKeypair,
    registry: Arc<ChannelConfigRegistry>,
}

impl Node {
    /// The reply-channel name this node is legitimately entitled to.
    fn own_reply_channel(&self) -> ChannelName {
        ChannelName::new(&format!(
            "{PREFIX}{:016x}",
            self.keypair.entity_id().origin_hash()
        ))
        .unwrap()
    }
}

async fn build_node() -> Node {
    let keypair = EntityKeypair::generate();
    let mut node = MeshNode::new(keypair.clone(), test_config())
        .await
        .expect("MeshNode::new");
    let registry = Arc::new(ChannelConfigRegistry::new());
    node.set_channel_configs(registry.clone());
    Node {
        mesh: Arc::new(node),
        keypair,
        registry,
    }
}

/// Register the origin-bound reply family on `node`, mirroring what the
/// SDK's `auto_register_rpc_channels` installs.
fn register_bound_prefix(node: &Node) {
    let sentinel = ChannelName::new("svc.replies.prefix").unwrap();
    node.registry.insert_prefix(
        PREFIX,
        ChannelConfig::new(ChannelId::new(sentinel))
            .with_subscriber_origin_binding(OriginBinding::OriginHashHex16),
    );
}

/// `initiator` dials `responder`. Only the initiator pushes a capability
/// announcement, so only the initiator ends up pinned on the responder.
async fn handshake(initiator: &Arc<MeshNode>, responder: &Arc<MeshNode>) {
    let i_id = initiator.node_id();
    let r_id = responder.node_id();
    let r_pub = *responder.public_key();
    let r_addr = responder.local_addr();
    let r_clone = responder.clone();
    let accept = tokio::spawn(async move { r_clone.accept(i_id).await });
    initiator
        .connect(r_addr, &r_pub, r_id)
        .await
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond()
}

/// Both peers handshake to `publisher` and announce, so `publisher`
/// pins both of their entities.
async fn pinned_pair() -> (Node, Node, Node) {
    let publisher = build_node().await;
    let alice = build_node().await;
    let mallory = build_node().await;
    register_bound_prefix(&publisher);

    handshake(&alice.mesh, &publisher.mesh).await;
    handshake(&mallory.mesh, &publisher.mesh).await;
    publisher.mesh.start();
    alice.mesh.start();
    mallory.mesh.start();

    for n in [&alice, &mallory] {
        n.mesh
            .announce_capabilities(CapabilitySet::new())
            .await
            .expect("announce");
    }
    let learned = wait_until(|| {
        publisher
            .mesh
            .peer_entity_id(alice.mesh.node_id())
            .is_some()
            && publisher
                .mesh
                .peer_entity_id(mallory.mesh.node_id())
                .is_some()
    })
    .await;
    assert!(learned, "publisher never pinned both subscriber entities");

    (publisher, alice, mallory)
}

/// The legitimate case: a peer subscribing to the channel naming its own
/// origin is admitted. Guards against "fix H3 by rejecting everyone".
#[tokio::test]
async fn own_reply_channel_subscribe_is_admitted() {
    let (publisher, alice, _mallory) = pinned_pair().await;

    alice
        .mesh
        .subscribe_channel(publisher.mesh.node_id(), alice.own_reply_channel())
        .await
        .expect("a peer must be able to subscribe to its own reply channel");
}

/// The attack: Mallory subscribes to Alice's reply channel. Pre-fix this
/// was admitted (the prefix config had no gates at all) and put Mallory
/// in the roster, so any roster-fallback response for Alice reached her.
#[tokio::test]
async fn other_peers_reply_channel_subscribe_is_rejected() {
    let (publisher, alice, mallory) = pinned_pair().await;

    let victim_channel = alice.own_reply_channel();
    let result = mallory
        .mesh
        .subscribe_channel(publisher.mesh.node_id(), victim_channel.clone())
        .await;

    assert!(
        result.is_err(),
        "a peer must not be able to subscribe to another peer's origin-bound \
         reply channel (H3 cross-caller response disclosure)"
    );

    // And it must not be in the roster either — a wire-level rejection
    // that still added the subscriber would leak on the next fan-out.
    assert!(
        !publisher
            .mesh
            .roster()
            .is_subscribed(mallory.mesh.node_id(), &ChannelId::new(victim_channel)),
        "rejected subscriber must not appear in the roster"
    );
}

/// Mallory may still subscribe to its OWN reply channel — the binding
/// restricts *which* name a peer may claim, it does not deny the family.
#[tokio::test]
async fn binding_still_admits_each_peer_to_its_own_channel() {
    let (publisher, _alice, mallory) = pinned_pair().await;

    mallory
        .mesh
        .subscribe_channel(publisher.mesh.node_id(), mallory.own_reply_channel())
        .await
        .expect("each peer keeps access to the channel naming its own origin");
}

// The "unpinned peer is rejected" rule is NOT covered here, on purpose.
// It is not reachable over the wire: a node announces as it comes up, so
// by the time it can send a Subscribe the publisher has already pinned
// it — an attempt to construct the state by making the subscriber the
// handshake responder (only initiators push an announcement) still
// observed the pin landing before the subscribe. A test that depends on
// losing that race would be flaky in exactly the direction that hides a
// regression.
//
// The rule instead lives as one branch of the pure
// `OriginBinding::authorizes`, unit-tested in
// `channel/config.rs::tests::origin_binding_*`. That is a better home:
// it is deterministic, and it pins the fail-closed decision itself
// rather than an environment that happens to produce it.

/// A name under the bound prefix that is not a well-formed origin is
/// rejected regardless of who asks (no partial / prefix matching on the
/// suffix).
#[tokio::test]
async fn malformed_suffix_is_rejected() {
    let (publisher, alice, _mallory) = pinned_pair().await;

    for suffix in ["deadbeef", "notahexvalueatall", "00000000000000000"] {
        let name = ChannelName::new(&format!("{PREFIX}{suffix}")).unwrap();
        let result = alice
            .mesh
            .subscribe_channel(publisher.mesh.node_id(), name)
            .await;
        assert!(
            result.is_err(),
            "suffix {suffix:?} is not this peer's origin and must be rejected"
        );
    }
}
