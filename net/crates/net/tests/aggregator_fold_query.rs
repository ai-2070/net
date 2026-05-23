//! End-to-end integration test for the Phase C `fold.query`
//! RPC service.
//!
//! Two `MeshNode`s — one hosting an `AggregatorDaemon` with the
//! query service installed, one acting as the querier via
//! `FoldQueryClient`. Pins:
//!
//! 1. Successful round-trip of a `LatestSummary` query.
//! 2. `SummarizeNow` forces a fresh tick on the host side.
//! 3. Unknown-kind requests return a typed `Server` error.
//! 4. Client cache short-circuits a second identical call.
//!
//! Run: `cargo test --features net --test aggregator_fold_query`

#![cfg(feature = "net")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::aggregator::{
    AggregatorConfig, AggregatorDaemon, FoldQueryClient, FoldQueryClientError, FoldQueryError,
};
use net::adapter::net::behavior::fold::capability::{CapabilityFold, CapabilityMembership};
use net::adapter::net::behavior::fold::wire::SignedAnnouncement;
use net::adapter::net::behavior::fold::{EnvelopeMeta, FoldKind, NodeState};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, SubnetId};

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

fn sign_cap(
    kp: &EntityKeypair,
    publisher: u64,
    class: u64,
    state: NodeState,
) -> SignedAnnouncement<CapabilityMembership> {
    SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        class,
        publisher,
        1,
        EnvelopeMeta::default(),
        CapabilityMembership {
            class_hash: class,
            tags: Vec::new(),
            hardware: None,
            state,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
        },
    )
    .expect("sign")
}

/// Build a two-node mesh: `host` runs an AggregatorDaemon with
/// the query service installed; `querier` connects to it. Returns
/// `(host, querier, agg, _serve_handle)`. Hold the serve handle —
/// dropping it tears down the service registration.
async fn build_aggregator_pair() -> (
    Arc<MeshNode>,
    Arc<MeshNode>,
    Arc<AggregatorDaemon>,
    net::adapter::net::mesh_rpc::ServeHandle,
) {
    let host = build_node().await;
    let querier = build_node().await;
    handshake(&host, &querier).await;

    // Prime the host's capability fold with three publishers in
    // mixed states so the summary has visibly non-zero buckets.
    let kp = EntityKeypair::generate();
    let fold = host.capability_fold();
    fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
    fold.apply(sign_cap(&kp, 0xB, 2, NodeState::Idle)).unwrap();
    fold.apply(sign_cap(&kp, 0xC, 3, NodeState::Busy)).unwrap();

    let cfg = AggregatorConfig::new(SubnetId::new(&[3, 7]))
        .with_fold_kind(CapabilityFold::KIND_ID)
        .with_interval(Duration::from_secs(60));
    let agg = Arc::new(AggregatorDaemon::new(cfg, host.clone()).expect("new aggregator"));
    // Run one tick synchronously so the latest-summaries buffer
    // is primed — `LatestSummary` queries read from it directly.
    agg.tick_once();
    let serve_handle = agg
        .install_query_service(&host)
        .expect("install_query_service");

    (host, querier, agg, serve_handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_summary_round_trips_across_handshake() {
    let (host, querier, _agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let summaries = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("query_latest");
    assert_eq!(summaries.len(), 1, "expected one summary row");
    let summary = &summaries[0];
    assert_eq!(summary.fold_kind, CapabilityFold::KIND_ID);
    assert_eq!(summary.source_subnet, SubnetId::new(&[3, 7]));
    let idle = summary
        .buckets
        .iter()
        .find(|(n, _)| n == "idle")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    let busy = summary
        .buckets
        .iter()
        .find(|(n, _)| n == "busy")
        .map(|(_, c)| *c)
        .unwrap_or(0);
    assert_eq!(idle, 2, "two idle publishers primed");
    assert_eq!(busy, 1, "one busy publisher primed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summarize_now_forces_fresh_tick_on_host() {
    let (host, querier, agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let before = agg.generation();
    let _ = client
        .query_summarize_now(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("query_summarize_now");
    let after = agg.generation();
    assert!(
        after > before,
        "generation must advance after SummarizeNow (before={before}, after={after})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_kind_returns_server_error() {
    let (host, querier, _agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let result = client.query_latest(host_node_id, 0xDEAD).await;
    match result {
        Err(FoldQueryClientError::Server(FoldQueryError::UnknownKind { kind })) => {
            assert_eq!(kind, 0xDEAD);
        }
        Err(other) => panic!("expected Server(UnknownKind), got {other:?}"),
        Ok(s) => panic!("expected error, got {s:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_hit_short_circuits_second_identical_call() {
    let (host, querier, agg, _serve) = build_aggregator_pair().await;
    let host_node_id = host.node_id();
    let client = FoldQueryClient::new(querier).with_deadline(Duration::from_secs(2));

    let first = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("first call");
    let gen_after_first = agg.generation();

    let second = client
        .query_latest(host_node_id, CapabilityFold::KIND_ID)
        .await
        .expect("second call");
    let gen_after_second = agg.generation();

    assert_eq!(first, second, "cached call must match");
    assert_eq!(
        gen_after_first, gen_after_second,
        "second call should not have ticked the host's generation (cache hit)"
    );
}
