//! Regression test for TEST_COVERAGE_PLAN §P1-5 — the
//! three-way-agreement invariant between the failure detector,
//! the routing table, and the capability index when a peer dies.
//!
//! # The gap this test closes
//!
//! Before this test, the `on_failure` callback in `MeshNode::new`
//! cleared:
//!
//! - the routing table (`reroute_policy.on_failure`),
//! - the channel roster (`roster.remove_peer`),
//! - the peer-subnets map (`peer_subnets.remove`),
//! - the peer-entity-ids map (`peer_entity_ids.remove`).
//!
//! But NOT the `capability_index`. That left a rendezvous
//! coordinator able to hand a freshly-failed peer's cached reflex
//! to a `PunchRequest` initiator — the coordinator's lookup at
//! `capability_index.reflex_addr(target)` would succeed even
//! though the failure detector had already marked the target
//! dead. The initiator would then wire keep-alives at a corpse
//! address and fall back after `punch_deadline` expired.
//!
//! # The fix
//!
//! The `on_failure` callback now also calls
//! `capability_index.remove(node_id)` so all four maps agree on
//! "this peer is gone." The three-way-agreement invariant is
//! restored: failure detector ⟺ routing table ⟺ capability
//! index all converge in the same failure-handling tick.
//!
//! # What this test pins
//!
//! Three-node setup A↔R, B↔R. B announces capabilities; R
//! indexes B's reflex. Then we block B's traffic on R's side
//! (partition filter) and wait for R's failure detector to
//! mark B as failed. After failure, R's capability index must
//! NOT still contain B's entry — and a rendezvous
//! `PunchRequest(A→R, target=B)` from A must time out as if B
//! had never announced (coordinator drops silently at the
//! missing-reflex check).
//!
//! Run: `cargo test --features net,nat-traversal --test peer_death_clears_capability_index`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::TraversalError;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

/// Short heartbeat + session-timeout so R's failure detector
/// transitions to Failed in a few seconds. The miss threshold
/// hard-coded on `FailureDetectorConfig` is 3, and
/// `NodeState::check` computes `missed_count = elapsed /
/// timeout`, so Failed fires after 3 × session_timeout of
/// silence. With session_timeout=500 ms the failure window
/// closes at ~1.5 s.
fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(100))
        .with_session_timeout(Duration::from_millis(500))
        .with_handshake(3, Duration::from_secs(2));
    cfg.socket_buffers = SocketBufferConfig {
        send_buffer_size: TEST_BUFFER_SIZE,
        recv_buffer_size: TEST_BUFFER_SIZE,
    };
    cfg
}

async fn build_node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), test_config())
            .await
            .expect("MeshNode::new"),
    )
}

async fn connect_pair(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
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

async fn wait_for<F: Fn() -> bool>(limit: Duration, check: F) -> bool {
    let start = tokio::time::Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capability_index_is_cleared_when_failure_detector_marks_peer_failed() {
    // Topology: A↔R, B↔R, plus an auxiliary X so both A and B
    // have >=2 peers for `reclassify_nat` to complete the sweep
    // and produce a reflex. Without X, B's classifier stays
    // Unknown and the announcement carries `reflex_addr: None`,
    // which would make the test's preconditions unreachable.
    let a = build_node().await;
    let r = build_node().await;
    let b = build_node().await;
    let x = build_node().await;
    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.start();
    r.start();
    b.start();
    x.start();

    // B classifies + announces so R's index picks up B's reflex.
    // A also announces so R's classifier has a second probe target
    // for its own sweep (not strictly needed here, but keeps R's
    // classifier state sensible).
    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    let b_id = b.node_id();
    let b_bind = b.local_addr();

    // Wait for R to index B's reflex — proxy for "announcement
    // has been absorbed by R's capability index."
    assert!(
        wait_for(Duration::from_secs(3), || r.peer_reflex_addr(b_id)
            == Some(b_bind))
        .await,
        "R should index B's announced reflex; got {:?}",
        r.peer_reflex_addr(b_id),
    );

    // Snapshot: B is in R's index, PunchRequests would find a
    // reflex. This is the pre-failure state.
    assert!(
        r.test_capability_fold_has(b_id),
        "precondition: B must be indexed on R before failure",
    );

    // Simulate B failing from R's perspective: drop all traffic
    // between R and B. After a few missed heartbeats R's
    // failure detector considers B stale; `check_all()` then
    // transitions B to Failed and fires the on_failure callback,
    // which must evict B from every derived map — including the
    // capability index (the P1-5 fix).
    let b_bind_for_r = b.local_addr();
    r.block_peer(b_bind_for_r);

    // Wait past 3 × session_timeout so
    // `missed_count = elapsed / timeout >= miss_threshold (3)`,
    // then explicitly drive `check_all()` to run the state
    // transition + `on_failure` callback. Production meshes
    // call `check_all()` from the heartbeat-loop cadence; the
    // test calls it explicitly to keep the assertion
    // deterministic.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let _ = r.failure_detector().check_all();

    let r_for_poll = r.clone();
    let b_id_copy = b_id;
    let evicted = wait_for(Duration::from_secs(2), || {
        !r_for_poll.test_capability_fold_has(b_id_copy)
    })
    .await;
    assert!(
        evicted,
        "R's capability index must evict B after the failure \
         detector fires — three-way-agreement invariant. Without \
         this fix the rendezvous coordinator would still return \
         B's stale reflex to a PunchRequest initiator. R.has(B)={:?}",
        r.test_capability_fold_has(b_id),
    );

    // Stronger assertion: the behavioral consequence. A fires a
    // PunchRequest through R asking to punch to B. Coordinator
    // looks up B's reflex, gets None (index evicted), and sends a
    // typed `PunchReject` so A fails fast with
    // `unknown-target-reflex` instead of burning the full
    // `punch_deadline` (the rendezvous fast-fail contract — see
    // `docs/plans/NAT_TRAVERSAL_V2_PLAN.md`). Same outcome as "B
    // never announced": the reject reason itself proves the index
    // no longer serves B's stale reflex.
    let start = tokio::time::Instant::now();
    let result = a.request_punch(r.node_id(), b_id, a.local_addr()).await;
    let elapsed = start.elapsed();

    match result {
        Err(TraversalError::RendezvousRejected(reason)) => {
            assert_eq!(
                reason, "unknown-target-reflex",
                "the reject reason should prove the eviction",
            );
        }
        other => panic!(
            "expected fast RendezvousRejected(unknown-target-reflex) \
             after peer death (R's index evicted B), got {other:?}",
        ),
    }
    assert!(
        elapsed < Duration::from_secs(4),
        "typed rejection must resolve well inside punch_deadline \
         (Finding 5's fast-fail contract); elapsed {elapsed:?}",
    );
}
