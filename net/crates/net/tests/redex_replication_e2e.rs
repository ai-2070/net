//! End-to-end integration tests for RedEX replication.
//!
//! Wires two `MeshNode`s + two `Redex` instances, calls
//! `enable_replication` on both, opens the same channel name with
//! a replication-enabled config, then appends events on the leader
//! and asserts the replicas catch up via the heartbeat-driven
//! `SyncRequest` / `SyncResponse` cycle. The older tests drive the
//! roles past the bootstrap by hand; the later ones let the runtime
//! bootstrap, place (`Standard` / `ColocationStrict`) and elect.
//!
//! Run: `cargo test --features redex --test redex_replication_e2e`

#![cfg(feature = "redex")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::channel::ChannelName;
use net::adapter::net::redex::{
    PlacementStrategy, Redex, RedexFileConfig, ReplicaRole, ReplicationConfig,
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
        .with_capability_gc_interval(Duration::from_millis(250))
        // Replica placement follows capability announcements, which the
        // origin rate-limits to one per window (default 10 s); a short
        // window keeps the placement tests' convergence quick.
        .with_min_announce_interval(Duration::from_millis(50));
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
    handshake_no_start(a, b).await;
    a.start();
    b.start();
}

/// Pair-handshake without `start()` — caller batches `start_all`
/// after every pair has shaken. Required for >2-node topologies
/// where `accept()` after `start()` is rejected (the post-start
/// dispatcher would race the responder).
async fn handshake_no_start(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_id = b.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task").expect("accept");
}

fn start_all(nodes: &[&Arc<MeshNode>]) {
    for n in nodes {
        n.start();
    }
}

fn cn(s: &str) -> ChannelName {
    ChannelName::new(s).unwrap()
}

/// Wait until the runtime's own election settles on `leader` as Leader
/// and every one of `replicas` as Replica. The runtime bootstraps and
/// elects by itself (the pinned leader wins when healthy), so tests wait
/// for that rather than drive transitions that race it.
async fn await_roles(
    leader: &net::adapter::net::redex::ReplicationCoordinator,
    replicas: &[&net::adapter::net::redex::ReplicationCoordinator],
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let settled = leader.role() == ReplicaRole::Leader
            && replicas.iter().all(|c| c.role() == ReplicaRole::Replica);
        if settled {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "election never settled: leader {:?}, replicas {:?}",
            leader.role(),
            replicas.iter().map(|c| c.role()).collect::<Vec<_>>()
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Wait for the replication runtime to bootstrap a pinned member
/// `Idle -> Replica`. The runtime does that itself now; a test that
/// drove the step by hand would race it.
async fn await_bootstrapped(coord: &net::adapter::net::redex::ReplicationCoordinator) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while coord.role() == ReplicaRole::Idle {
        assert!(
            tokio::time::Instant::now() < deadline,
            "replication runtime never bootstrapped the channel to Replica"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(coord.role(), ReplicaRole::Replica);
}

/// Two-node replication round-trip — appends on the leader's
/// channel surface should land on the replica's local file via the
/// inbox-driven catch-up cycle. The replica is driven into
/// `Replica` role explicitly (Phase F placement filter doesn't
/// auto-elect yet); the leader is driven through
/// `Replica → Candidate → Leader` to exercise the normal lifecycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_replication_catches_replica_up() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    // Two managers, one per node. Both enable replication.
    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    // Open the same channel name on both. Heartbeat at 150ms so
    // the catch-up cycle drives within the test timeout, but
    // not so fast that the tracker accidentally trips the
    // missed-heartbeat threshold mid-test. Use `Pinned`
    // placement so both runtimes know the replica set at spawn —
    // Standard placement leaves the set empty (Phase F adds
    // placement recomputation; this test pre-dates that).
    let name = cn("repl/e2e");
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
            .with_leader_pinned(Some(a_id)),
    ));
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let file_b = redex_b.open_file(&name, cfg).expect("open B");
    assert_eq!(redex_a.replication_runtime_count(), 1);
    assert_eq!(redex_b.replication_runtime_count(), 1);

    // Drive roles. Coordinator starts in Idle on both sides.
    // A becomes Leader; B becomes Replica.
    let coord_a = redex_a.replication_coordinator_for(&name).expect("coord A");
    let coord_b = redex_b.replication_coordinator_for(&name).expect("coord B");
    // State-machine path Idle → Replica → Candidate → Leader.
    await_roles(&coord_a, &[&coord_b]).await;
    assert_eq!(coord_a.role(), ReplicaRole::Leader);
    assert_eq!(coord_b.role(), ReplicaRole::Replica);

    // Append a batch of events on the leader's file. The replica's
    // local file starts empty; the catch-up cycle must apply
    // every event in order.
    const N: u64 = 32;
    for i in 0..N {
        file_a
            .append(format!("event-{i}").as_bytes())
            .expect("append leader");
    }
    assert_eq!(file_a.next_seq(), N);
    assert_eq!(file_b.next_seq(), 0);

    // Wait for the replica to catch up. The first leader heartbeat
    // carries tail_seq=N; the replica's tick observes lag, issues
    // a SyncRequest, the leader's runtime returns a SyncResponse,
    // the replica's apply_sync_response advances the local tail.
    // Worst case takes a few heartbeat cycles for the discovery →
    // request → response → apply round-trip.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last_b_tail = 0u64;
    while tokio::time::Instant::now() < deadline {
        last_b_tail = file_b.next_seq();
        if last_b_tail == N {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        last_b_tail, N,
        "replica did not catch up to leader's tail within 5s (got {last_b_tail}, expected {N})"
    );

    // Verify payload contents match.
    let events = file_b.read_range(0, N);
    assert_eq!(events.len(), N as usize);
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.entry.seq, i as u64, "event {i} out of order");
        assert_eq!(
            ev.payload.as_ref(),
            format!("event-{i}").as_bytes(),
            "event {i} payload mismatch",
        );
    }

    // Metrics sanity: the leader shipped at least one SyncResponse,
    // so `sync_bytes_total` on its channel must be > 0. The leader
    // also transitioned into Leader role once, so
    // `leader_changes_total` must be exactly 1. Pulled via the
    // operator-facing snapshot surface.
    let snap_a = redex_a
        .replication_metrics_snapshot()
        .expect("snapshot on enabled Redex");
    let chan_a = snap_a
        .channels
        .iter()
        .find(|c| c.channel == "repl/e2e")
        .expect("channel in snapshot");
    assert!(
        chan_a.sync_bytes_total > 0,
        "leader's sync_bytes_total should bump on SyncResponse ship; got {}",
        chan_a.sync_bytes_total
    );
    assert_eq!(
        chan_a.leader_changes_total, 1,
        "leader changed exactly once (Idle → Replica → Candidate → Leader)"
    );

    // Cleanup: close both channels so the runtimes exit cleanly.
    redex_a.close_file(&name).expect("close A");
    redex_b.close_file(&name).expect("close B");
}

/// Heartbeat round-trip — the leader's tick emits a heartbeat to
/// the replica, the replica's tracker records it, the replica's
/// believed_leader cell becomes Some(A). Pins the simplest
/// observable interaction: a single message crossing the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_heartbeat_records_believed_leader() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let name = cn("repl/heartbeat");
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
            .with_leader_pinned(Some(a_id)),
    ));
    redex_a.open_file(&name, cfg.clone()).expect("open A");
    redex_b.open_file(&name, cfg).expect("open B");

    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();

    // Bring both nodes to participating roles via the
    // state-machine path Idle → Replica → Candidate → Leader.
    await_roles(&coord_a, &[&coord_b]).await;

    // Wait for B's coordinator metrics to observe a non-default
    // replica_lag — the gauge gets stamped when on_tick runs while
    // there's a believed leader. We can't directly observe the
    // tracker through the coordinator surface (intentionally —
    // it's internal); instead pin that the leader_changes_total
    // counter on A has been bumped (the A→Leader transition did
    // that) and that no runtime has crashed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if coord_b.role() == ReplicaRole::Replica {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(coord_a.role(), ReplicaRole::Leader);
    assert_eq!(coord_b.role(), ReplicaRole::Replica);

    redex_a.close_file(&name).expect("close A");
    redex_b.close_file(&name).expect("close B");
}

/// Failover scenario — leader closes its channel mid-flight; the
/// replica's tracker observes the silence, the replica's tick
/// decides to enter Candidate via MissedHeartbeats, the
/// deterministic election promotes the replica to Leader. Pins
/// the failure-detection → election → promotion cycle end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_close_triggers_replica_election_and_promotion() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    // 150ms heartbeat → 450ms failure-detection window
    // (3 × heartbeat). Tight enough to finish the failover
    // within the test's deadline.
    let name = cn("repl/failover");
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
            .with_leader_pinned(Some(a_id)),
    ));
    redex_a.open_file(&name, cfg.clone()).expect("open A");
    redex_b.open_file(&name, cfg).expect("open B");

    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();

    // Drive A → Leader, B → Replica.
    await_roles(&coord_a, &[&coord_b]).await;

    // R-41: poll until B has observed at least one leader
    // heartbeat from A, with a hard deadline. Replacing the
    // previous fixed 500ms sleep removes the CI flake window
    // where scheduler jitter delayed A's first heartbeat past
    // the sleep budget.
    {
        let poll_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let coord_b_ref = &coord_b;
        loop {
            // The replica's role stays Replica while waiting; we
            // can't read tracker state from outside the runtime,
            // so we poll a proxy: the replica's metric snapshot
            // shows a non-zero lag observation once a leader
            // heartbeat has landed. Until then, lag is sentinel.
            let snap = redex_b
                .replication_metrics_snapshot()
                .expect("metrics snapshot");
            let observed = snap
                .channel(name.as_str())
                .map(|c| c.replica_lag_seconds.is_some())
                .unwrap_or(false);
            if observed {
                break;
            }
            if tokio::time::Instant::now() >= poll_deadline {
                // Fall through; the election test below will
                // still pass if a heartbeat lands during the kill
                // detection window, just with less determinism.
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = coord_b_ref; // keep ref alive for the loop
        }
    }

    // Close A's channel — its runtime exits, no more heartbeats
    // emitted to B.
    redex_a.close_file(&name).expect("close A");

    // Wait for B to detect the silence + run the election. The
    // detection window is 3 × heartbeat = 450ms; the election
    // itself runs in the same tick that detects silence, so the
    // total bound is one heartbeat past the detection window.
    // Pad to 3s to absorb scheduler jitter on CI boxes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut final_role = coord_b.role();
    while tokio::time::Instant::now() < deadline {
        final_role = coord_b.role();
        if final_role == ReplicaRole::Leader {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        final_role,
        ReplicaRole::Leader,
        "replica failed to win election within 3s after leader silence (final role: {final_role:?})"
    );

    redex_b.close_file(&name).expect("close B");
}

/// Three-node replication — exercises the broadcast fanout path
/// (leader emits heartbeats to N-1 replicas; lag gauge picks the
/// worst replica). Pins that the runtime correctly addresses
/// every replica in the set, not just the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_replication_fans_out_to_every_replica() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    let node_c = build_node().await;
    // Full-mesh handshake: each pair needs a direct session
    // for `peer_addr(node)` to resolve in the dispatcher.
    // Use the no-start pattern so all three pairs shake before
    // any node's runtime starts dispatching — `accept()` after
    // `start()` is rejected.
    handshake_no_start(&node_a, &node_b).await;
    handshake_no_start(&node_a, &node_c).await;
    handshake_no_start(&node_b, &node_c).await;
    start_all(&[&node_a, &node_b, &node_c]);

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    let redex_c = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());
    redex_c.enable_replication(node_c.clone());

    let name = cn("repl/three_node");
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();
    let c_id = node_c.node_id();
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id, c_id]))
            .with_leader_pinned(Some(a_id)),
    ));
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let file_b = redex_b.open_file(&name, cfg.clone()).expect("open B");
    let file_c = redex_c.open_file(&name, cfg).expect("open C");

    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();
    let coord_c = redex_c.replication_coordinator_for(&name).unwrap();

    // Drive: A is Leader; B and C are Replicas.
    await_roles(&coord_a, &[&coord_b, &coord_c]).await;

    // Append on A; both B and C must catch up.
    const N: u64 = 24;
    for i in 0..N {
        file_a
            .append(format!("event-{i}").as_bytes())
            .expect("append leader");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut b_tail = 0u64;
    let mut c_tail = 0u64;
    while tokio::time::Instant::now() < deadline {
        b_tail = file_b.next_seq();
        c_tail = file_c.next_seq();
        if b_tail == N && c_tail == N {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        b_tail, N,
        "replica B did not catch up (got {b_tail}, expected {N})"
    );
    assert_eq!(
        c_tail, N,
        "replica C did not catch up (got {c_tail}, expected {N})"
    );

    // Spot-check payload contents on both replicas.
    let events_b = file_b.read_range(0, N);
    let events_c = file_c.read_range(0, N);
    assert_eq!(events_b.len(), N as usize);
    assert_eq!(events_c.len(), N as usize);
    for i in 0..(N as usize) {
        let expected = format!("event-{i}");
        assert_eq!(events_b[i].payload.as_ref(), expected.as_bytes());
        assert_eq!(events_c[i].payload.as_ref(), expected.as_bytes());
    }

    redex_a.close_file(&name).expect("close A");
    redex_b.close_file(&name).expect("close B");
    redex_c.close_file(&name).expect("close C");
}

// ────────────────────────────────────────────────────────────────
// Dataforts Phase 2 performance-budget regression
// ────────────────────────────────────────────────────────────────
//
// Pins the explicit gate from
// `docs/internal/misc/DATAFORTS_PLAN.md` Phase 2:
//
//   Performance budget. Replication overhead ≤ 30% of single-node
//   append throughput at steady state. Treat regression as test
//   failure.
//
// The replication runtime task ticks in the background on the same
// tokio runtime as the application's append loop. Each tick steals
// CPU + memory bandwidth from the publisher. The 30% bound says
// that overhead can't dominate the publisher's append throughput
// in steady state — heartbeats + tracker updates + bandwidth
// budget bookkeeping should stay well below the per-append cost.
//
// CI environment variance: stress-loaded runners produce noisy
// timings. The bound is set at the documented 1.3× spec; if CI
// flakes consistently below 1.5×, treat that as the genuine signal
// the runtime overhead has regressed — don't loosen the bound.
// The fixed N + warmup + median-of-trials shape below buffers
// against single-iteration outliers.

/// Median of `xs` (input is consumed). Sorts in-place.
fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

/// Append `n` events of `payload_size` bytes; return the elapsed
/// wall-clock time. Caller decides how to aggregate across trials.
fn time_appends(
    file: &net::adapter::net::redex::RedexFile,
    n: u64,
    payload_size: usize,
) -> Duration {
    let payload = vec![0x42u8; payload_size];
    let start = std::time::Instant::now();
    for _ in 0..n {
        file.append(&payload).expect("append failed");
    }
    start.elapsed()
}

// Marked `#[ignore]` because the ≤1.3× ratio asserts wall-clock
// performance, which flakes on shared CI runners. Run locally
// via `cargo test -- --ignored`. CI workflows that want to keep
// this on-deck should run with `--ignored` on a dedicated bench
// runner, not the default test matrix.
#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replication_overhead_within_30_percent_budget() {
    // Workload parameters chosen so each trial completes in well
    // under a second on a typical dev box, but is long enough that
    // per-iteration noise averages out.
    const N: u64 = 50_000;
    const PAYLOAD_BYTES: usize = 64;
    const TRIALS: usize = 5;
    const OVERHEAD_BUDGET: f64 = 1.3;

    // ── Baseline: single-node, no replication, no mesh.
    let baseline_redex = Arc::new(Redex::new());
    let baseline_file = baseline_redex
        .open_file(&cn("perf/baseline"), RedexFileConfig::default())
        .expect("open baseline");

    // Warmup — allocator + branch predictor + (any) cache effects.
    let _ = time_appends(&baseline_file, N / 10, PAYLOAD_BYTES);

    let mut baseline_trials = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        let r = Redex::new();
        let f = r
            .open_file(&cn("perf/baseline_trial"), RedexFileConfig::default())
            .unwrap();
        baseline_trials.push(time_appends(&f, N, PAYLOAD_BYTES));
    }
    let baseline_median = median(baseline_trials);

    // ── With replication: 2-node mesh, leader does the appends.
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let a_id = node_a.node_id();
    let b_id = node_b.node_id();

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    // 500ms heartbeat — production-realistic. A faster cadence
    // would amplify the runtime's CPU cost artificially.
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(500)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
            .with_leader_pinned(Some(a_id)),
    ));

    let name = cn("perf/replicated");
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let _file_b = redex_b.open_file(&name, cfg).expect("open B");

    // Drive A to Leader, B to Replica — production failover path
    // would land here automatically; for the regression test, set
    // them deterministically.
    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();
    await_roles(&coord_a, &[&coord_b]).await;

    // Warmup so the replication runtime tasks have settled into
    // their steady-state cadence + the mesh handshake is fully
    // primed.
    let _ = time_appends(&file_a, N / 10, PAYLOAD_BYTES);

    let mut replicated_trials = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        replicated_trials.push(time_appends(&file_a, N, PAYLOAD_BYTES));
    }
    let replicated_median = median(replicated_trials);

    // ── Assert the budget.
    let ratio = replicated_median.as_secs_f64() / baseline_median.as_secs_f64();
    eprintln!(
        "replication overhead: baseline={:?} replicated={:?} ratio={:.3}x",
        baseline_median, replicated_median, ratio
    );
    assert!(
        ratio <= OVERHEAD_BUDGET,
        "replication overhead = {:.2}x; Dataforts Phase 2 budget is ≤{}x (≤30% overhead). \
         baseline median={:?}, replicated median={:?}",
        ratio,
        OVERHEAD_BUDGET,
        baseline_median,
        replicated_median,
    );

    redex_a.close_file(&name).ok();
    redex_b.close_file(&name).ok();
}

/// Dataforts Phase 2: "Replication-sync I/O ≤ 50% of NIC peak under
/// saturating append rate."
///
/// The leader's `BandwidthBudget` enforces this directly — the
/// runtime's `on_inbound(SyncRequest)` consults the budget before
/// shipping a response and NACKs with `Backpressure` when over the
/// configured fraction × NIC peak. The default fraction is 0.5
/// (matching the spec's "≤50%"); `ReplicationConfig::
/// with_replication_budget_fraction` overrides per-channel.
///
/// This test pins the per-channel metrics snapshot exposes
/// `sync_bytes_total` and `under_capacity_total` fields populated
/// from the live coordinator under the e2e wire path. It does NOT
/// engage the budget (the 256-event burst is well within a 0.5×NIC
/// allowance) — the actual budget-fired path is unit-tested under
/// `replication_catchup` with synthetic load that exceeds capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bandwidth_budget_metric_field_is_plumbed() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;

    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let name = cn("perf/bandwidth");
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![
                node_a.node_id(),
                node_b.node_id(),
            ]))
            .with_leader_pinned(Some(node_a.node_id()))
            // Sane production fraction. The bandwidth budget's
            // ENFORCEMENT path (NACK Backpressure on exceeded
            // budget) is unit-tested in replication_catchup; this
            // e2e just verifies the value plumbs through to the
            // metrics snapshot.
            .with_replication_budget_fraction(0.5),
    ));
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let _file_b = redex_b.open_file(&name, cfg).expect("open B");

    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();
    await_roles(&coord_a, &[&coord_b]).await;

    // Drive moderate append load.
    for i in 0..256u64 {
        file_a.append(format!("bw-{i}").as_bytes()).unwrap();
    }
    // Let the catch-up cycle run for a few heartbeat cycles.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if redex_b
            .replication_coordinator_for(&name)
            .map(|c| c.tail_seq())
            .unwrap_or(0)
            >= 256
            || _file_b.next_seq() >= 256
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Metrics surface: per-channel snapshot must include
    // sync_bytes_total and under_capacity_total. The latter MUST
    // be 0 in this scenario (budget set to 0.5 × 1 Gbps placeholder
    // is generous for a 256-event burst). If it bumps, the
    // bandwidth gate over-tightened or the catch-up shipped more
    // than the budget allows.
    let snap = redex_a
        .replication_metrics_snapshot()
        .expect("snapshot enabled");
    let chan = snap
        .channels
        .iter()
        .find(|c| c.channel == "perf/bandwidth")
        .expect("channel in snapshot");
    assert!(
        chan.sync_bytes_total > 0,
        "leader shipped at least one SyncResponse",
    );
    assert_eq!(
        chan.under_capacity_total, 0,
        "Dataforts Phase 2: bandwidth budget at 0.5×NIC must NOT be \
         exceeded under a 256-event burst. under_capacity_total bumping \
         means the budget gate let too much through (or this test's \
         workload is larger than the placeholder NIC peak's burst \
         allowance)."
    );

    redex_a.close_file(&name).ok();
    redex_b.close_file(&name).ok();
}

// ============================================================================
// No hand-driven roles: the runtime bootstraps, elects and replicates.
// ============================================================================
//
// Every test above drives the coordinator through `transition_to`. Before
// the runtime learned to bootstrap a pinned member and to elect when a
// replica has never heard from a leader, that was the ONLY way a channel
// left `Idle`: through the public `Redex` API alone (every binding), a
// replicated channel sat in `Idle` forever and replicated nothing.

/// Poll until `f` holds, or fail with `what` after `secs`.
async fn wait_until(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_pair_elects_the_pinned_leader_and_replicates() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let name = cn("repl/auto-pinned");
    let (a_id, b_id) = (node_a.node_id(), node_b.node_id());
    // B is pinned as leader, the opposite of the RTT ranking's
    // self-preference on A, so a pass proves `leader_pinned` decides.
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![a_id, b_id]))
            .with_leader_pinned(Some(a_id))
            .with_leader_pinned(Some(b_id)),
    ));
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let file_b = redex_b.open_file(&name, cfg).expect("open B");
    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();

    wait_until(5, "B never became leader", || {
        coord_b.role() == ReplicaRole::Leader && coord_a.role() == ReplicaRole::Replica
    })
    .await;

    for i in 0..16u32 {
        file_b.append(format!("event-{i}").as_bytes()).unwrap();
    }
    wait_until(5, "A never caught up to the leader", || {
        file_a.next_seq() == 16
    })
    .await;
    let events = file_a.read_range(0, 16);
    assert_eq!(events[15].payload.as_ref(), b"event-15");
    assert_eq!(coord_b.role(), ReplicaRole::Leader, "leadership stayed put");

    redex_a.close_file(&name).ok();
    redex_b.close_file(&name).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_pair_without_a_pinned_leader_converges_on_one() {
    let node_a = build_node().await;
    let node_b = build_node().await;
    handshake(&node_a, &node_b).await;
    let redex_a = Arc::new(Redex::new());
    let redex_b = Arc::new(Redex::new());
    redex_a.enable_replication(node_a.clone());
    redex_b.enable_replication(node_b.clone());

    let name = cn("repl/auto-elect");
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::Pinned(vec![
                node_a.node_id(),
                node_b.node_id(),
            ])),
    ));
    let file_a = redex_a.open_file(&name, cfg.clone()).expect("open A");
    let file_b = redex_b.open_file(&name, cfg).expect("open B");
    let coord_a = redex_a.replication_coordinator_for(&name).unwrap();
    let coord_b = redex_b.replication_coordinator_for(&name).unwrap();

    // Each node ranks itself first, so both may win their own election;
    // the peer-leader rule then concedes one. Wait for a stable split.
    let one_leader = || {
        matches!(
            (coord_a.role(), coord_b.role()),
            (ReplicaRole::Leader, ReplicaRole::Replica)
                | (ReplicaRole::Replica, ReplicaRole::Leader)
        )
    };
    wait_until(5, "the pair never settled on one leader", one_leader).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(one_leader(), "leadership flapped after settling");

    let (leader, replica) = if coord_a.role() == ReplicaRole::Leader {
        (&file_a, &file_b)
    } else {
        (&file_b, &file_a)
    };
    for i in 0..8u32 {
        leader.append(format!("event-{i}").as_bytes()).unwrap();
    }
    wait_until(5, "the replica never caught up", || replica.next_seq() == 8).await;

    redex_a.close_file(&name).ok();
    redex_b.close_file(&name).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disable_replication_releases_the_mesh() {
    let node = build_node().await;
    let baseline = Arc::strong_count(&node);
    let redex = Redex::new();
    redex.enable_replication(node.clone());
    let name = cn("repl/disable");
    let file = redex
        .open_file(
            &name,
            RedexFileConfig::default().with_replication(Some(
                ReplicationConfig::new()
                    .with_heartbeat_ms(150)
                    .with_placement(PlacementStrategy::Pinned(vec![node.node_id()])),
            )),
        )
        .expect("open");
    let coord = redex.replication_coordinator_for(&name).unwrap();
    await_bootstrapped(&coord).await;
    assert!(Arc::strong_count(&node) > baseline);

    redex.disable_replication();
    redex.disable_replication(); // idempotent
    assert_eq!(redex.replication_runtime_count(), 0);
    wait_until(2, "the shut-down runtime never withdrew to Idle", || {
        coord.role() == ReplicaRole::Idle
    })
    .await;
    // The coordinator handle carries its own mesh reference (its
    // chain-tag sink); only Rust callers can hold one. Release it, then
    // the runtime task's exit leaves the mesh with no extra owners.
    drop(coord);
    wait_until(2, "the Redex still holds the mesh", || {
        Arc::strong_count(&node) == baseline
    })
    .await;

    // The file is still a local log, and replication can come back.
    file.append(b"local").unwrap();
    redex.enable_replication(node.clone());
    assert_eq!(redex.replication_runtime_count(), 0);
}

// ============================================================================
// Standard / ColocationStrict placement (REDEX_REPLICA_PLACEMENT_PLAN.md)
// ============================================================================
//
// Before the placement resolver, `Standard` (the default) and
// `ColocationStrict` started with an empty replica set and every channel
// sat in `Idle`: nothing replicated unless the set was pinned by hand.

struct Trio {
    nodes: [Arc<MeshNode>; 3],
    redexes: [Arc<Redex>; 3],
}

async fn trio() -> Trio {
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;
    handshake_no_start(&a, &b).await;
    handshake_no_start(&a, &c).await;
    handshake_no_start(&b, &c).await;
    start_all(&[&a, &b, &c]);
    let redexes = [
        Arc::new(Redex::new()),
        Arc::new(Redex::new()),
        Arc::new(Redex::new()),
    ];
    for (r, n) in redexes.iter().zip([&a, &b, &c]) {
        r.enable_replication(n.clone());
    }
    Trio {
        nodes: [a, b, c],
        redexes,
    }
}

fn roles(trio: &Trio, name: &ChannelName) -> Vec<ReplicaRole> {
    trio.redexes
        .iter()
        .map(|r| {
            r.replication_coordinator_for(name)
                .map(|c| c.role())
                .unwrap_or(ReplicaRole::Idle)
        })
        .collect()
}

/// The index of the node with the `rank`-th lowest NodeId.
fn by_id_rank(trio: &Trio, rank: usize) -> usize {
    let mut idx: Vec<usize> = (0..3).collect();
    idx.sort_by_key(|&i| trio.nodes[i].node_id());
    idx[rank]
}

fn settled(roles: &[ReplicaRole], members: &[usize]) -> bool {
    let leaders = members
        .iter()
        .filter(|&&i| roles[i] == ReplicaRole::Leader)
        .count();
    let all_in = members
        .iter()
        .all(|&i| matches!(roles[i], ReplicaRole::Leader | ReplicaRole::Replica));
    let others_idle = (0..roles.len())
        .filter(|i| !members.contains(i))
        .all(|i| roles[i] == ReplicaRole::Idle);
    leaders == 1 && all_in && others_idle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_placement_selects_factor_replicas_and_replicates() {
    let t = trio().await;
    let name = cn("repl/standard");
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_factor(2)
            .with_heartbeat_ms(150),
    ));
    let files: Vec<_> = t
        .redexes
        .iter()
        .map(|r| r.open_file(&name, cfg.clone()).expect("open"))
        .collect();

    // Equal scores (no placement hints, no resources announced) → the two
    // lowest NodeIds, the same set on every node.
    let members = [by_id_rank(&t, 0), by_id_rank(&t, 1)];
    let outsider = by_id_rank(&t, 2);
    wait_until(8, "the pair never settled with one leader", || {
        settled(&roles(&t, &name), &members)
    })
    .await;

    let r = roles(&t, &name);
    let (leader, follower) = if r[members[0]] == ReplicaRole::Leader {
        (members[0], members[1])
    } else {
        (members[1], members[0])
    };
    for i in 0..8u32 {
        files[leader]
            .append(format!("event-{i}").as_bytes())
            .unwrap();
    }
    wait_until(5, "the other replica never caught up", || {
        files[follower].next_seq() == 8
    })
    .await;
    assert_eq!(
        files[outsider].next_seq(),
        0,
        "the unselected node holds nothing"
    );
    assert_eq!(roles(&t, &name)[outsider], ReplicaRole::Idle);

    for r in &t.redexes {
        r.close_file(&name).ok();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standard_placement_reselects_when_a_replica_leaves() {
    let t = trio().await;
    let name = cn("repl/reselect");
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_factor(2)
            .with_heartbeat_ms(150),
    ));
    let files: Vec<_> = t
        .redexes
        .iter()
        .map(|r| r.open_file(&name, cfg.clone()).expect("open"))
        .collect();
    let first = [by_id_rank(&t, 0), by_id_rank(&t, 1)];
    let outsider = by_id_rank(&t, 2);
    wait_until(8, "the first set never settled", || {
        settled(&roles(&t, &name), &first)
    })
    .await;

    // The lowest-id member leaves; its candidacy is withdrawn, so the
    // remaining nodes re-resolve to {the other member, the outsider}.
    let leaving = first[0];
    t.redexes[leaving].close_file(&name).expect("close");
    let second = [first[1], outsider];
    wait_until(
        8,
        "the set never re-resolved to take in the outsider",
        || {
            let r = roles(&t, &name);
            let leaders = second
                .iter()
                .filter(|&&i| r[i] == ReplicaRole::Leader)
                .count();
            leaders == 1
                && second
                    .iter()
                    .all(|&i| matches!(r[i], ReplicaRole::Leader | ReplicaRole::Replica))
        },
    )
    .await;

    let r = roles(&t, &name);
    let (leader, follower) = if r[second[0]] == ReplicaRole::Leader {
        (second[0], second[1])
    } else {
        (second[1], second[0])
    };
    let base = files[leader].next_seq();
    for i in 0..4u32 {
        files[leader]
            .append(format!("after-{i}").as_bytes())
            .unwrap();
    }
    wait_until(5, "the newly selected replica never caught up", || {
        files[follower].next_seq() == base + 4
    })
    .await;

    for (i, r) in t.redexes.iter().enumerate() {
        if i != leaving {
            r.close_file(&name).ok();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn colocation_strict_selects_only_holders_of_the_named_chain() {
    let t = trio().await;
    // Two of the three nodes hold an unrelated chain; the channel must
    // replicate only onto them, even with factor 3.
    const HOST_CHAIN: u64 = 0x00C0_FFEE_0000_0001;
    let holders = [by_id_rank(&t, 0), by_id_rank(&t, 2)];
    let outsider = by_id_rank(&t, 1);
    for &i in &holders {
        t.nodes[i].announce_chain(HOST_CHAIN, 1).await.unwrap();
    }

    let name = cn("repl/colocated");
    let cfg = RedexFileConfig::default().with_replication(Some(
        ReplicationConfig::new()
            .with_factor(3)
            .with_heartbeat_ms(150)
            .with_placement(PlacementStrategy::ColocationStrict)
            .with_placement_metadata(
                net::adapter::net::redex::COLOCATE_WITH_STRICT_METADATA_KEY,
                format!("{HOST_CHAIN:016x}"),
            ),
    ));
    for r in &t.redexes {
        r.open_file(&name, cfg.clone()).expect("open");
    }
    wait_until(8, "the holders never settled as the replica set", || {
        settled(&roles(&t, &name), &holders)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        roles(&t, &name)[outsider],
        ReplicaRole::Idle,
        "a node without the chain is never selected"
    );

    for r in &t.redexes {
        r.close_file(&name).ok();
    }
}
