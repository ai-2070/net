//! End-to-end integration tests for the greedy-LRU dataforts
//! runtime (DATAFORTS_PLAN § Phase 1).
//!
//! Wires two `MeshNode`s + two `Redex` instances. Node A is the
//! publisher; Node B has greedy enabled and acts as the
//! speculative cache. The test asserts that events published on
//! the channel from A land in B's greedy cache via the substrate's
//! standard-event inbound dispatch hook — without any explicit
//! subscription on B's local channels, just the mesh-level
//! observation path.
//!
//! Run: `cargo test --features dataforts-greedy --test dataforts_greedy_e2e`

#![cfg(feature = "dataforts-greedy")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::placement::IntentRegistry;
use net::adapter::net::channel::{ChannelName, ChannelPublisher, PublishConfig};
use net::adapter::net::dataforts::{
    synthesize_cache_channel_name, GreedyConfig, IntentMatchPolicy,
};
use net::adapter::net::redex::Redex;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
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

async fn build_node() -> Arc<MeshNode> {
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

async fn handshake(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect");
    accept.await.expect("accept task").expect("accept");
    a.start();
    b.start();
    let _ = a_id;
    let _ = b_id;
}

fn cn(s: &str) -> ChannelName {
    ChannelName::new(s).unwrap()
}

/// B enables greedy with empty-scope / disabled-intent admission
/// (admit everything). A publishes 16 events to a subscribed
/// channel. B's greedy runtime must cache the channel under the
/// synthesized `dataforts/greedy/<hash>` name and the cache must
/// reflect the appended bytes within a bounded poll.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn greedy_caches_observed_events_published_from_peer() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    // Node A is the publisher and doesn't need a Redex install
    // for this test — only B's greedy runtime is under inspection.
    let redex_b = Arc::new(Redex::new());

    // B configures greedy permissively — empty scopes admit any
    // chain, intent disabled, default colocation.
    let cfg = GreedyConfig::default()
        .with_intent_match(IntentMatchPolicy::Disabled);
    redex_b
        .enable_greedy_dataforts(
            node_b.clone(),
            cfg,
            Arc::new(CapabilitySet::default()),
            IntentRegistry::new(),
        )
        .expect("enable greedy");
    assert!(node_b.has_greedy_observer(), "observer install must land");

    // B subscribes to A's channel so the publish fan-out reaches B.
    // The greedy hook then fires on B's inbound dispatch path
    // alongside the application's tail.
    let name = cn("dataforts/test/greedy-e2e");
    node_b
        .subscribe_channel(node_a.node_id(), name.clone())
        .await
        .expect("subscribe");

    // A publishes 16 events.
    let publisher = ChannelPublisher::new(name.clone(), PublishConfig::default());
    const N: u64 = 16;
    for i in 0..N {
        let payload = Bytes::from(format!("event-{i}"));
        node_a
            .publish(&publisher, payload)
            .await
            .expect("publish");
    }

    // Poll for B's greedy runtime to absorb the events. The cache
    // channel is named via the synthesized hash convention.
    let runtime = redex_b.greedy_runtime().expect("runtime installed");
    let synth = synthesize_cache_channel_name(name.hash());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if runtime.contains(&synth) && runtime.cached_bytes() >= N {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        runtime.contains(&synth),
        "greedy runtime must cache the observed channel (synth name = {})",
        synth.as_str()
    );
    assert!(
        runtime.cached_bytes() >= N,
        "cache must reflect at least N bytes (got {})",
        runtime.cached_bytes()
    );

    // Metric surface — cache_hits_total wouldn't bump because the
    // hook fires on every inbound event; the bytes_resident gauge
    // and channel-count are the observable ones.
    let snap = runtime.metrics().snapshot();
    let chan = snap
        .channels
        .iter()
        .find(|c| c.channel == synth.as_str())
        .expect("snapshot must list the synth channel");
    assert!(
        chan.bytes_resident > 0,
        "bytes_resident gauge must reflect appended bytes; got {}",
        chan.bytes_resident,
    );

    // Cleanup.
    redex_b.disable_greedy_dataforts();
    assert!(
        !node_b.has_greedy_observer(),
        "disable must un-install the observer on the mesh"
    );
}

/// B enables greedy with a scope filter that the publisher's
/// channel won't satisfy. The observer must reject every event,
/// no cache files open, and the cluster's
/// `admit_rejected_scope_total` counter bumps once per event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn greedy_scope_filter_rejects_off_scope_events() {
    use net::adapter::net::dataforts::ScopeLabel;

    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    // Node A doesn't need a Redex for greedy — it's the
    // publisher, not the observer. Keep the test lean.
    let redex_b = Arc::new(Redex::new());

    let cfg = GreedyConfig::default()
        .with_scopes(vec![ScopeLabel::new("industrial")])
        .with_intent_match(IntentMatchPolicy::Disabled);
    redex_b
        .enable_greedy_dataforts(
            node_b.clone(),
            cfg,
            Arc::new(CapabilitySet::default()),
            IntentRegistry::new(),
        )
        .expect("enable greedy");

    // The chain_caps the runtime sees today is the default-empty
    // CapabilitySet (the mesh hook doesn't resolve chain_caps yet
    // — see the TODO in process_local_packet's greedy hook).
    // Empty chain_caps + non-empty configured scopes → reject on
    // every inbound event.

    let name = cn("dataforts/test/wrong-scope");
    node_b
        .subscribe_channel(node_a.node_id(), name.clone())
        .await
        .expect("subscribe");
    let publisher = ChannelPublisher::new(name.clone(), PublishConfig::default());
    const N: u64 = 4;
    for i in 0..N {
        node_a
            .publish(&publisher, Bytes::from(format!("event-{i}")))
            .await
            .expect("publish");
    }

    // Wait for the scope-reject metric to climb. The greedy hook
    // is fire-and-forget (tokio::spawn per event), so we poll the
    // cluster counter rather than a synchronous read.
    let runtime = redex_b.greedy_runtime().expect("runtime installed");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let snap = runtime.metrics().snapshot();
        if snap.cluster.admit_rejected_scope_total >= N {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let snap = runtime.metrics().snapshot();
    assert!(
        snap.cluster.admit_rejected_scope_total >= N,
        "admit_rejected_scope_total must bump per event; got {}",
        snap.cluster.admit_rejected_scope_total
    );
    assert_eq!(
        runtime.cached_channel_count(),
        0,
        "no cache file should be created when admission rejects"
    );

    redex_b.disable_greedy_dataforts();
}
