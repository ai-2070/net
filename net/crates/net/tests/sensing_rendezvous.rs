//! SI-0 items 24/25 on the REAL routing path
//! (SENSING_INTEREST_COALESCING_PLAN §4.1, review 6): the
//! provider-free capability interest has a legitimate destination.
//!
//! Line topology A — B — C. Each node computes the sensing leader
//! from ITS OWN pingwave-flooded proximity graph (the shared view)
//! via `sensing_leader` — which delegates to the RedEX election —
//! and all three must agree on B, the proximity center. Both
//! consumers then route an interest payload to that elected leader
//! over ordinary Net routing (routes learned from pingwaves, not
//! configured), and B receives both.
//!
//! The in-process rendezvous suite (`behavior::sensing::rendezvous`)
//! proves the leader-role semantics; this test proves the transport
//! claim — agreement from real graphs, delivery over real routes.
//!
//! Run: `cargo test --features redex --test sensing_rendezvous`

#![cfg(all(feature = "net", feature = "redex"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing::sensing_leader;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};
use net::adapter::Adapter;
use net::event::{batch_process_nonce, Batch, InternalEvent};
use tokio::net::UdpSocket;

const PSK: [u8; 32] = [0x42u8; 32];
const TEST_BUFFER_SIZE: usize = 256 * 1024;

async fn find_ports(n: usize) -> Vec<u16> {
    let mut ports = Vec::with_capacity(n);
    let mut sockets = Vec::with_capacity(n);
    for _ in 0..n {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        ports.push(sock.local_addr().unwrap().port());
        sockets.push(sock);
    }
    drop(sockets);
    ports
}

fn mk_config(addr: SocketAddr) -> MeshNodeConfig {
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_num_shards(2)
        .with_handshake(3, Duration::from_secs(3))
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(10));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

fn interest_batch(tag: &str) -> Batch {
    let events: Vec<InternalEvent> = (0..2)
        .map(|i| {
            InternalEvent::from_value(serde_json::json!({"tag": tag, "index": i}), i as u64, 0)
        })
        .collect();
    Batch {
        shard_id: 0,
        events,
        sequence_start: 0,
        process_nonce: batch_process_nonce(),
    }
}

/// The proximity graph keys nodes by the u64 node id zero-padded to
/// 32 bytes (mesh.rs `node_id_to_graph_id`); the election speaks
/// plain u64.
fn graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// The undirected shared-view RTT closure over one node's proximity
/// graph — the centrality input the rendezvous reads (§4.1).
fn graph_rtt(node: &Arc<MeshNode>) -> impl Fn(u64, u64) -> Option<Duration> + '_ {
    move |a, b| {
        node.proximity_graph()
            .edge_latency(graph_id(a), graph_id(b))
            .or_else(|| {
                node.proximity_graph()
                    .edge_latency(graph_id(b), graph_id(a))
            })
    }
}

#[tokio::test]
async fn nodes_agree_on_the_leader_and_route_interests_to_it() {
    let ports = find_ports(3).await;

    let id_a = EntityKeypair::generate();
    let id_b = EntityKeypair::generate();
    let id_c = EntityKeypair::generate();
    let (nid_a, nid_b, nid_c) = (id_a.node_id(), id_b.node_id(), id_c.node_id());

    let addr_a: SocketAddr = format!("127.0.0.1:{}", ports[0]).parse().unwrap();
    let addr_b: SocketAddr = format!("127.0.0.1:{}", ports[1]).parse().unwrap();
    let addr_c: SocketAddr = format!("127.0.0.1:{}", ports[2]).parse().unwrap();

    let node_a = Arc::new(MeshNode::new(id_a, mk_config(addr_a)).await.unwrap());
    let node_b = Arc::new(MeshNode::new(id_b, mk_config(addr_b)).await.unwrap());
    let node_c = Arc::new(MeshNode::new(id_c, mk_config(addr_c)).await.unwrap());

    let pub_b = *node_b.public_key();

    // Line topology: A ↔ B and B ↔ C only. B is the center.
    let (r1, r2) = tokio::join!(node_b.accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        node_a.connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("B accept A");
    r2.expect("A connect B");
    let (r1, r2) = tokio::join!(node_b.accept(nid_c), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        node_c.connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("B accept C");
    r2.expect("C connect B");

    node_a.start();
    node_b.start();
    node_c.start();

    // Pingwaves flood the topology (session-open event pingwaves +
    // the heartbeat tick): every node must learn all three nodes and
    // both line edges into its OWN proximity graph.
    let members = [nid_a, nid_b, nid_c];
    let graphs_converged = wait_until(
        || {
            [&node_a, &node_b, &node_c].iter().all(|node| {
                let rtt = graph_rtt(node);
                rtt(nid_a, nid_b).is_some() && rtt(nid_b, nid_c).is_some()
            })
        },
        Duration::from_secs(5),
    )
    .await;
    assert!(
        graphs_converged,
        "proximity graphs never converged on the line topology",
    );

    // Every node computes the leader from its OWN graph — and they
    // agree on the center, B (A and C each lack a direct A–C edge,
    // so their centrality scores carry the unknown-edge penalty).
    let healthy = |_: u64| true; // failure-plane integration is SI-5
    let leader_at_a = sensing_leader(&members, graph_rtt(&node_a), healthy);
    let leader_at_b = sensing_leader(&members, graph_rtt(&node_b), healthy);
    let leader_at_c = sensing_leader(&members, graph_rtt(&node_c), healthy);
    assert_eq!(leader_at_a, Some(nid_b), "A must elect the center");
    assert_eq!(leader_at_a, leader_at_b, "B agrees with A");
    assert_eq!(leader_at_a, leader_at_c, "C agrees with A");
    let leader = leader_at_a.unwrap();

    // Both consumers route their provider-free interest payload to
    // the elected leader over routes the pingwave flood installed —
    // nothing configured by hand.
    let routes_ready = wait_until(
        || {
            node_a.router().routing_table().lookup(leader).is_some()
                && node_c.router().routing_table().lookup(leader).is_some()
        },
        Duration::from_secs(5),
    )
    .await;
    assert!(
        routes_ready,
        "pingwave-learned routes to the leader missing"
    );

    node_a
        .send_routed(leader, &interest_batch("interest_from_a"))
        .await
        .expect("A routes its interest to the leader");
    node_c
        .send_routed(leader, &interest_batch("interest_from_c"))
        .await
        .expect("C routes its interest to the leader");

    // The elected leader receives BOTH — the provider-free interest
    // has a real, agreed destination (where the in-process
    // SensingLeader role coalesces them; that half is pinned by the
    // rendezvous unit suite).
    let mut seen_a = false;
    let mut seen_c = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !(seen_a && seen_c) {
        let result = node_b.poll_shard(0, None, 100).await.unwrap();
        for event in &result.events {
            if let Ok(json) = event.parse() {
                match json["tag"].as_str() {
                    Some("interest_from_a") => seen_a = true,
                    Some("interest_from_c") => seen_c = true,
                    _ => {}
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen_a && seen_c,
        "leader missing interests (from A: {seen_a}, from C: {seen_c})",
    );

    node_a.shutdown().await.unwrap();
    node_b.shutdown().await.unwrap();
    node_c.shutdown().await.unwrap();
}
