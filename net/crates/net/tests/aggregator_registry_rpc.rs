//! End-to-end integration test for the slice-7
//! `aggregator.registry` RPC service.
//!
//! Two `MeshNode`s — one hosting an `AggregatorRegistry` with
//! the registry service installed, one acting as the operator
//! via `RegistryClient`. Pins:
//!
//! 1. Successful round-trip of a `List` request across the
//!    handshake.
//! 2. Empty registry returns an empty `Groups` reply (not an
//!    error).
//! 3. Per-replica health flows through the wire.
//!
//! Run: `cargo test --features net --test aggregator_registry_rpc`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::aggregator::{
    AggregatorConfig, AggregatorDaemon, AggregatorRegistry, RegistryClient,
};
use net::adapter::net::behavior::fold::capability::CapabilityFold;
use net::adapter::net::behavior::fold::FoldKind;
use net::adapter::net::behavior::lifecycle::LifecycleGroup;
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, SubnetId,
};

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
    let keypair = EntityKeypair::generate();
    let node = MeshNode::new(keypair, test_config())
        .await
        .expect("MeshNode::new");
    Arc::new(node)
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
        .expect("connect failed");
    accept
        .await
        .expect("accept task panicked")
        .expect("accept failed");
    a.start();
    b.start();
}

/// Build a two-node mesh: `host` runs an `AggregatorRegistry`
/// with two registered groups + the registry service installed;
/// `querier` connects to it. Returns
/// `(host, querier, registry, _serve_handle, group_names)`.
async fn build_registry_pair(
) -> (Arc<MeshNode>, Arc<MeshNode>, Arc<AggregatorRegistry>, net::adapter::net::mesh_rpc::ServeHandle, Vec<&'static str>) {
    let host = build_node().await;
    let querier = build_node().await;
    handshake(&host, &querier).await;

    let registry = Arc::new(AggregatorRegistry::new());

    // Spawn two groups; the registry exposes them via `entries()`
    // sorted by name, so the wire reply lands in lex order.
    for name in &["alpha", "beta"] {
        let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(50));
        let cfg_clone = cfg.clone();
        let host_clone = host.clone();
        let group = LifecycleGroup::<AggregatorDaemon>::spawn(2, [0xABu8; 32], move |_idx| {
            Arc::new(AggregatorDaemon::new(cfg_clone.clone(), host_clone.clone()).expect("new"))
        })
        .await
        .expect("spawn group");
        registry.register(*name, group).expect("register");
    }

    let serve_handle = registry
        .install_registry_service(&host)
        .expect("install_registry_service");

    (host, querier, registry, serve_handle, vec!["alpha", "beta"])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_round_trips_two_registered_groups_across_handshake() {
    let (host, querier, _registry, _serve, expected_names) = build_registry_pair().await;
    let host_node_id = host.node_id();
    let client = RegistryClient::new(querier).with_deadline(Duration::from_secs(2));

    let groups = client.list(host_node_id).await.expect("list");
    let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, expected_names);
    for g in &groups {
        assert_eq!(g.replicas.len(), 2);
        for r in &g.replicas {
            // Healthy at snapshot time — interval just started,
            // 3 × interval hasn't elapsed.
            assert!(r.healthy);
            assert!(r.diagnostic.is_none());
            // No placement (these groups used the placement-free
            // `spawn`).
            assert!(r.placement_node_id.is_none());
        }
    }

    // Drive the registry's groups to shutdown so the test
    // teardown doesn't dangle.
    for name in expected_names {
        let g = _registry.unregister(name).await.expect("unregister");
        g.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_against_empty_registry_returns_empty_groups() {
    // Build a host with the service installed but no groups
    // registered. The querier should get an empty `Groups`
    // reply, not an error.
    let host = build_node().await;
    let querier = build_node().await;
    handshake(&host, &querier).await;
    let registry = Arc::new(AggregatorRegistry::new());
    let _serve = registry
        .install_registry_service(&host)
        .expect("install_registry_service");

    let client = RegistryClient::new(querier).with_deadline(Duration::from_secs(2));
    let groups = client.list(host.node_id()).await.expect("list");
    assert!(groups.is_empty());
}
