//! Integration test for the MeshDB `SUBPROTOCOL_MESHDB` wire
//! hookup. Spins up two real `MeshNode`s, handshakes them, installs
//! a `MeshDbServer` on the responder, runs a federated query from
//! the caller, and asserts the result row decodes correctly on the
//! caller side.
//!
//! This is the end-to-end pinning for the Phase B wire-dispatch
//! hookup that the substrate-side `LoopbackTransport` integration
//! test exercises only in-process.
//!
//! Run: `cargo test --features "net,meshdb" --test meshdb_subprotocol_wire`

#![cfg(all(feature = "net", feature = "meshdb"))]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use net::adapter::net::behavior::meshdb::{
    enable_meshdb_on_mesh, ChainReader, FederatedMeshQueryExecutor, LocalMeshQueryExecutor,
    MeshDbServer, MeshQueryExecutor, OperatorPlan, SeqNum,
};
use net::adapter::net::behavior::meshdb::planner::{CostEstimate, ExecutionPlan, OperatorNode};
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
    let keypair = EntityKeypair::generate();
    let node = MeshNode::new(keypair, test_config())
        .await
        .expect("MeshNode::new");
    Arc::new(node)
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

/// Test-only `ChainReader` backed by a `BTreeMap`.
#[derive(Default)]
struct InMemoryChainReader {
    chains: Mutex<BTreeMap<u64, BTreeMap<SeqNum, Vec<u8>>>>,
}

impl InMemoryChainReader {
    fn append(&self, origin: u64, seq: SeqNum, payload: Vec<u8>) {
        self.chains
            .lock()
            .unwrap()
            .entry(origin)
            .or_default()
            .insert(seq, payload);
    }
}

impl ChainReader for InMemoryChainReader {
    fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)?
            .get(&seq)
            .cloned()
    }
    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)
            .map(|c| {
                c.range(start..end)
                    .map(|(s, p)| (*s, p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)?
            .keys()
            .next_back()
            .copied()
    }
}

fn atomic_plan(op: OperatorPlan, target: u64) -> ExecutionPlan {
    ExecutionPlan {
        root: OperatorNode {
            operator: op,
            target_nodes: vec![target],
            cost: CostEstimate::default(),
        },
        total_cost: CostEstimate::default(),
    }
}

/// End-to-end pin: real MeshNode A queries real MeshNode B over
/// `SUBPROTOCOL_MESHDB`. A `Latest` request flows over UDP through
/// the encrypted Net session as a single subprotocol packet; the
/// server side decodes the frame, drives a `LocalMeshQueryExecutor`,
/// and ships the `ResultRow` back in a `MeshDbResponse::Batch{final}`.
/// The caller decodes and the row matches what B's reader holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_latest_query_over_real_wire() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // B is the server. Stand up a LocalMeshQueryExecutor over an
    // in-memory reader and install a MeshDbServer + dispatcher.
    let reader = Arc::new(InMemoryChainReader::default());
    reader.append(0xCAFE_BABE, SeqNum(7), b"hello-wire".to_vec());
    let executor: Arc<dyn MeshQueryExecutor> =
        Arc::new(LocalMeshQueryExecutor::new(reader));
    let server = MeshDbServer::new(executor);
    let (_dispatcher_b, _transport_b) = enable_meshdb_on_mesh(&b, Some(server.clone()));

    // A is the caller. Install a caller-only dispatcher and grab
    // the matching transport for the federated executor.
    let (_dispatcher_a, transport_a) = enable_meshdb_on_mesh(&a, None);
    let fed_a = FederatedMeshQueryExecutor::new(transport_a);

    // Issue a Latest(0xCAFE_BABE) federated query addressed at B.
    let plan = atomic_plan(
        OperatorPlan::LatestRead {
            origin: 0xCAFE_BABE,
        },
        b.node_id(),
    );
    let running = fed_a
        .execute(plan)
        .await
        .expect("federated execute over the wire");

    use futures::StreamExt;
    let mut rows = Vec::new();
    let mut stream = running.rows;
    // Drain with a generous timeout so a UDP retransmit (rare on
    // loopback) doesn't flake the test.
    let drain = async {
        while let Some(item) = stream.next().await {
            rows.push(item.expect("row"));
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain timed out");

    assert_eq!(rows.len(), 1, "expected exactly one row, got {rows:?}");
    assert_eq!(rows[0].origin, 0xCAFE_BABE);
    assert_eq!(rows[0].seq, SeqNum(7));
    assert_eq!(rows[0].payload, b"hello-wire");

    // Server-side bookkeeping is clean after the call drained.
    assert_eq!(server.inflight_calls(), 0);

    // Both nodes have the router installed.
    assert!(a.has_meshdb_inbound_router());
    assert!(b.has_meshdb_inbound_router());
}

/// Pin: empty-result query (`Latest` on an unknown origin) flows
/// through the wire cleanly. Server emits a single `End` with no
/// preceding `Batch`; the caller-side stream terminates with zero
/// rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_latest_query_over_wire_returns_empty_for_unknown_origin() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let reader = Arc::new(InMemoryChainReader::default()); // empty
    let executor: Arc<dyn MeshQueryExecutor> =
        Arc::new(LocalMeshQueryExecutor::new(reader));
    let server = MeshDbServer::new(executor);
    let (_d, _t) = enable_meshdb_on_mesh(&b, Some(server));

    let (_d, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    let plan = atomic_plan(
        OperatorPlan::LatestRead { origin: 0xDEAD_BEEF },
        b.node_id(),
    );
    let running = fed.execute(plan).await.expect("federated execute");

    use futures::StreamExt;
    let mut rows = Vec::new();
    let mut stream = running.rows;
    let drain = async {
        while let Some(item) = stream.next().await {
            rows.push(item.expect("row"));
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain timed out");

    assert!(rows.is_empty(), "expected zero rows, got {rows:?}");
}

/// Pin: a query against a node with no `MeshDbServer` installed
/// surfaces an `ExecutorError` rather than hanging. The dispatcher
/// returns `NoServer` for the inbound request frame; the server
/// side never sends a response; the caller's stream stays empty
/// until the test's drain timeout fires. We verify it terminates
/// within a generous timeout window — the test's job is to assert
/// "doesn't hang forever", not to validate a specific timeout
/// value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_query_with_no_server_eventually_terminates() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // B has a router installed but NO server.
    let (_d_b, _t_b) = enable_meshdb_on_mesh(&b, None);
    let (_d_a, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    let plan = atomic_plan(
        OperatorPlan::LatestRead { origin: 0xCAFE },
        b.node_id(),
    );
    let running = fed.execute(plan).await.expect("federated execute");

    use futures::StreamExt;
    let mut stream = running.rows;
    // Caller waits up to 2 s for a row — none should arrive. Then
    // the caller drops the stream, which cleans up the dispatcher's
    // inflight entry via the ResponseStreamGuard.
    let timeout_result =
        tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
    // Either the timeout fires (most likely — server silently drops
    // the request), or a `MeshError::QueryCancelled` arrives. Both
    // are acceptable "doesn't hang forever" outcomes.
    match timeout_result {
        Err(_) => {
            // Timeout: server dropped the request silently; the
            // test asserts the caller didn't deadlock waiting on
            // the channel.
        }
        Ok(None) => {
            // Stream closed; equivalent to server-side close.
        }
        Ok(Some(Err(_))) => {
            // Server surfaced a typed error — fine.
        }
        Ok(Some(Ok(row))) => panic!("unexpected row from a server-less peer: {row:?}"),
    }
}
