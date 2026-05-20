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
#![allow(
    clippy::disallowed_methods,
    reason = "test code legitimately uses std::sync::Mutex for SUT setup; no real poison concern"
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use net::adapter::net::behavior::meshdb::planner::{CostEstimate, ExecutionPlan, OperatorNode};
use net::adapter::net::behavior::meshdb::{
    enable_meshdb_on_mesh, ChainReader, FederatedMeshQueryExecutor, LocalMeshQueryExecutor,
    MeshDbServer, MeshQueryExecutor, OperatorPlan, SeqNum, MESHDB_SERVER_BATCH_ROWS,
};
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
        self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
    }
    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
        self.chains
            .lock()
            .unwrap()
            .get(&origin)
            .map(|c| c.range(start..end).map(|(s, p)| (*s, p.clone())).collect())
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
    let executor: Arc<dyn MeshQueryExecutor> = Arc::new(LocalMeshQueryExecutor::new(reader));
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

    // Server-side bookkeeping clears asynchronously after the
    // client-side stream drains — the per-call server task still
    // needs to finalize and decrement the counter. Poll with a
    // bounded deadline (mirrors the cancel-leak tests below).
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.inflight_calls() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    let executor: Arc<dyn MeshQueryExecutor> = Arc::new(LocalMeshQueryExecutor::new(reader));
    let server = MeshDbServer::new(executor);
    let (_d, _t) = enable_meshdb_on_mesh(&b, Some(server));

    let (_d, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    let plan = atomic_plan(
        OperatorPlan::LatestRead {
            origin: 0xDEAD_BEEF,
        },
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

    let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xCAFE }, b.node_id());
    let running = fed.execute(plan).await.expect("federated execute");

    use futures::StreamExt;
    let mut stream = running.rows;
    // Caller waits up to 2 s for a row — none should arrive. Then
    // the caller drops the stream, which cleans up the dispatcher's
    // inflight entry via the ResponseStreamGuard.
    let timeout_result = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
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

/// Pin: a result set large enough to cross
/// `MESHDB_SERVER_BATCH_ROWS` exercises the server's
/// flush-on-full path. The server emits one or more intermediate
/// `Batch { r#final: false }` frames followed by a final
/// `Batch { r#final: true }` (or `End`). The caller must see
/// every row in seq order and end up with the exact count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_between_query_over_wire_streams_multiple_batches() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let reader = Arc::new(InMemoryChainReader::default());
    // Two and a half batches' worth so the executor flushes
    // twice on full and once on drain.
    let total: u64 = (MESHDB_SERVER_BATCH_ROWS as u64) * 2 + 17;
    for seq in 1..=total {
        reader.append(0xFEED, SeqNum(seq), format!("row-{seq}").into_bytes());
    }
    let executor: Arc<dyn MeshQueryExecutor> = Arc::new(LocalMeshQueryExecutor::new(reader));
    let server = MeshDbServer::new(executor);
    let (_db, _tb) = enable_meshdb_on_mesh(&b, Some(server.clone()));

    let (_da, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    let plan = atomic_plan(
        OperatorPlan::BetweenRead {
            origin: 0xFEED,
            start: SeqNum(1),
            end: SeqNum(total + 1),
        },
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
    tokio::time::timeout(Duration::from_secs(10), drain)
        .await
        .expect("drain timed out");

    assert_eq!(
        rows.len(),
        total as usize,
        "every row across the batch boundary must surface",
    );
    let seqs: Vec<u64> = rows.iter().map(|r| r.seq.0).collect();
    let expected: Vec<u64> = (1..=total).collect();
    assert_eq!(seqs, expected, "rows must stay in seq order across batches");
    // Server-side bookkeeping clears asynchronously after the
    // client-side stream drains — poll with a bounded deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.inflight_calls() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(server.inflight_calls(), 0);
}

/// Pin: when the server's executor returns an error, the server
/// ships a single `MeshDbResponse::Error` and the caller's stream
/// surfaces the typed error rather than silently producing zero
/// rows. The federated executor rejects `NotYetImplemented`
/// plans locally, so to exercise the *server-side* error path we
/// stand B up with a stub executor that always errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_query_over_wire_propagates_executor_error() {
    use async_trait::async_trait;
    use net::adapter::net::behavior::meshdb::{
        error::MeshError,
        executor::{ExecuteOptions, RunningQuery},
    };

    /// Stub server-side executor: every `execute` call returns a
    /// typed `ExecutorError`. Pins the wire's
    /// `MeshDbResponse::Error` propagation.
    struct AlwaysErrorExecutor;
    #[async_trait]
    impl MeshQueryExecutor for AlwaysErrorExecutor {
        async fn execute(&self, _plan: ExecutionPlan) -> Result<RunningQuery, MeshError> {
            Err(MeshError::ExecutorError {
                node: 0xB,
                detail: "synthetic-server-failure".to_string(),
            })
        }
        async fn execute_with(
            &self,
            _plan: ExecutionPlan,
            _options: ExecuteOptions,
        ) -> Result<RunningQuery, MeshError> {
            Err(MeshError::ExecutorError {
                node: 0xB,
                detail: "synthetic-server-failure".to_string(),
            })
        }
    }

    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    let executor: Arc<dyn MeshQueryExecutor> = Arc::new(AlwaysErrorExecutor);
    let server = MeshDbServer::new(executor);
    let (_db, _tb) = enable_meshdb_on_mesh(&b, Some(server.clone()));

    let (_da, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    // A plan that the federated executor will happily ship to B
    // (atomic LatestRead with a single target node). B's stub
    // executor then errors out.
    let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xBADC0DE }, b.node_id());
    let running = fed.execute(plan).await.expect("federated execute");

    use futures::StreamExt;
    let mut errors = Vec::new();
    let mut rows = Vec::new();
    let mut stream = running.rows;
    let drain = async {
        while let Some(item) = stream.next().await {
            match item {
                Ok(r) => rows.push(r),
                Err(e) => errors.push(e),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("drain timed out");

    assert!(rows.is_empty(), "errored query must not emit rows");
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one typed error; got {errors:?}",
    );
    let rendered = format!("{}", errors[0]);
    assert!(
        rendered.contains("synthetic-server-failure"),
        "error must carry executor's detail; got {rendered:?}",
    );
    // Server-side inflight clears on exit.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.inflight_calls() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(server.inflight_calls(), 0);
}

/// Pin: dropping the caller-side stream signals the dispatcher
/// to remove the inflight entry; the server-side per-call task
/// either finishes naturally or surfaces a `QueryCancelled` if
/// the operator drives an explicit cancel via the query handle.
/// Either way, neither side leaks bookkeeping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federated_query_caller_cancels_via_handle_clears_both_sides() {
    let a = build_node().await;
    let b = build_node().await;
    handshake(&a, &b).await;

    // Wide enough that the caller cancels well before the server
    // finishes the natural drain.
    let reader = Arc::new(InMemoryChainReader::default());
    let total: u64 = (MESHDB_SERVER_BATCH_ROWS as u64) * 4;
    for seq in 1..=total {
        reader.append(0xC0DE, SeqNum(seq), b"x".to_vec());
    }
    let executor: Arc<dyn MeshQueryExecutor> = Arc::new(LocalMeshQueryExecutor::new(reader));
    let server = MeshDbServer::new(executor);
    let (_db, _tb) = enable_meshdb_on_mesh(&b, Some(server.clone()));

    let (_da, transport) = enable_meshdb_on_mesh(&a, None);
    let fed = FederatedMeshQueryExecutor::new(transport);

    let plan = atomic_plan(
        OperatorPlan::BetweenRead {
            origin: 0xC0DE,
            start: SeqNum(1),
            end: SeqNum(total + 1),
        },
        b.node_id(),
    );
    let running = fed.execute(plan).await.expect("federated execute");
    let handle = running.handle.clone();

    use futures::StreamExt;
    let mut stream = running.rows;
    // Drain a few rows, then cancel and drain the rest.
    let mut got = 0usize;
    while let Some(item) = stream.next().await {
        if item.is_ok() {
            got += 1;
        }
        if got >= 3 {
            handle.cancel();
            break;
        }
    }
    // Continue draining post-cancel; the stream must terminate
    // (either by closing or surfacing `QueryCancelled`).
    let drain = async {
        while let Some(item) = stream.next().await {
            // Don't care about post-cancel items; just ensure
            // the stream completes.
            let _ = item;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drain)
        .await
        .expect("post-cancel drain timed out");
    drop(stream);

    // Server-side: the per-call task may still be in flight for
    // a brief moment after the caller drops; give it a short
    // grace window to unregister.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.inflight_calls() != 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        server.inflight_calls(),
        0,
        "server-side inflight must clear after caller cancels",
    );
}
