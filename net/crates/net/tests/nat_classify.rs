//! Integration tests for stage 2 of `docs/NAT_TRAVERSAL_PLAN.md`:
//! NAT-type classification + reflex-address piggyback on
//! `CapabilityAnnouncement`.
//!
//! Stage 1 (the reflex-probe subprotocol) is exercised in
//! `tests/reflex_probe.rs`; this file builds on top of that to verify
//! the classification FSM populates `MeshNode`'s atomic
//! `nat_class` + `reflex_addr` fields, and that the resulting
//! `nat:*` capability tag reaches a peer via the existing
//! capability-broadcast path.
//!
//! # Properties under test
//!
//! - **Classification populates on demand.** Calling
//!   [`MeshNode::reclassify_nat`] with ≥2 connected peers on
//!   localhost yields `NatClass::Open` (reflex == bind) and a
//!   populated `reflex_addr()`.
//! - **`nat:*` tag rides the broadcast.** After reclassification,
//!   the next `announce_capabilities` emits the `nat:*` tag, and a
//!   peer can find the announcer via
//!   `find_nodes_by_filter(require_tag("nat:open"))`.
//! - **Fewer than 2 peers leaves state at `Unknown`.** Running
//!   reclassification with a lone peer is a no-op, preserving the
//!   pre-classification `Unknown` state.
//! - **Background classify loop seeds state without explicit
//!   upcall.** `spawn_nat_classify_loop` fires the first sweep
//!   once ≥2 peers are reachable.
//!
//! Run: `cargo test --features net,nat-traversal --test nat_classify`

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::traversal::classify::NatClass;
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

/// Bind via `127.0.0.1:0` so the OS picks a free port — no
/// pre-bind reservation window, no TOCTOU race with parallel
/// tests.
fn test_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    // Handshake budget is deliberately on the generous end of the
    // repo's idiom (3 attempts × 3 s, vs the 3 × 2 s some suites use).
    // The coverage workflow runs these under `-C instrument-coverage`,
    // which slows every basic block several-fold; the spawned `accept`
    // task can then miss a tighter 2 s window and trip the
    // `connect().expect(...)` in the three-node helpers. The wider
    // window costs nothing on the happy path — a successful handshake
    // returns immediately.
    let mut cfg = MeshNodeConfig::new(addr, PSK)
        .with_heartbeat_interval(Duration::from_millis(200))
        .with_session_timeout(Duration::from_secs(5))
        .with_handshake(3, Duration::from_secs(3));
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

/// Three-node setup: A in the center, B + C as its peers. Runs
/// both handshakes before starting the receive loops so a running
/// A doesn't conflict with a second inbound accept. Returns the
/// three started nodes.
async fn three_node_star() -> (Arc<MeshNode>, Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;

    // A ↔ B handshake
    {
        let a_id = a.node_id();
        let b_pub = *b.public_key();
        let b_addr = b.local_addr();
        let b_id = b.node_id();
        let b_clone = b.clone();
        let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
        a.connect(b_addr, &b_pub, b_id)
            .await
            .expect("connect A→B failed");
        accept
            .await
            .expect("accept B panicked")
            .expect("accept B failed");
    }

    // A ↔ C handshake
    {
        let a_id = a.node_id();
        let c_pub = *c.public_key();
        let c_addr = c.local_addr();
        let c_id = c.node_id();
        let c_clone = c.clone();
        let accept = tokio::spawn(async move { c_clone.accept(a_id).await });
        a.connect(c_addr, &c_pub, c_id)
            .await
            .expect("connect A→C failed");
        accept
            .await
            .expect("accept C panicked")
            .expect("accept C failed");
    }

    a.start();
    b.start();
    c.start();

    (a, b, c)
}

/// Two-node variant of the star helper. Same order-of-operations
/// (handshake then start), kept as its own helper to keep the test
/// bodies readable.
async fn two_node_pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = build_node().await;
    let b = build_node().await;
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id)
        .await
        .expect("connect A→B failed");
    accept
        .await
        .expect("accept B panicked")
        .expect("accept B failed");
    a.start();
    b.start();
    (a, b)
}

/// A manual reclassification with two connected peers on localhost
/// should yield `Open` — reflex equals bind, no NAT in the path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reclassify_on_localhost_is_open() {
    let (a, _b, _c) = three_node_star().await;

    // Pre-classification: atomic still at the Unknown default.
    assert_eq!(
        a.nat_class(),
        NatClass::Unknown,
        "pre-sweep classification should be Unknown",
    );
    assert!(
        a.reflex_addr().is_none(),
        "reflex_addr should be None before the first sweep",
    );

    a.reclassify_nat().await;

    assert_eq!(
        a.nat_class(),
        NatClass::Open,
        "localhost loopback: reflex equals bind → Open",
    );
    assert_eq!(
        a.reflex_addr(),
        Some(a.local_addr()),
        "reflex_addr should equal the bind addr on localhost",
    );
}

/// A peer receiving an announcement from a classified node can find
/// it via `find_nodes_by_filter` on the `nat:*` tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nat_tag_propagates_through_capability_broadcast() {
    let (a, b, _c) = three_node_star().await;

    a.reclassify_nat().await;
    assert_eq!(a.nat_class(), NatClass::Open);

    // Empty caps — the announce path should still synthesize the
    // `nat:open` tag from the classifier state.
    a.announce_capabilities(CapabilitySet::new())
        .await
        .expect("announce_capabilities");

    // Give the broadcast a few hundred ms to reach B and land in
    // its capability index.
    let filter = CapabilityFilter::new().require_tag("nat:open");
    let mut found = false;
    for _ in 0..30 {
        let peers = b.find_nodes_by_filter(&filter);
        if peers.contains(&a.node_id()) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        found,
        "B should see A's `nat:open` tag within 3s; got peers: {:?}",
        b.find_nodes_by_filter(&filter),
    );
}

/// Running `reclassify_nat` with a lone peer leaves the node at
/// `Unknown`. The FSM needs at least two probes to distinguish
/// Cone from Symmetric; one probe is treated as "unclassified" and
/// must not flip the atomic into a bogus state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reclassify_with_single_peer_stays_unknown() {
    let (a, _b) = two_node_pair().await;

    a.reclassify_nat().await;
    assert_eq!(
        a.nat_class(),
        NatClass::Unknown,
        "one peer is insufficient for classification",
    );
    assert!(
        a.reflex_addr().is_none(),
        "reflex_addr should stay None when classification didn't run",
    );
}

/// The background classify loop (spawned separately from `start`)
/// should fire the first sweep automatically once ≥2 peers are
/// connected. No explicit `reclassify_nat` call required.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_classify_loop_seeds_state() {
    let (a, _b, _c) = three_node_star().await;

    // The loop polls every 200 ms for ≥2 peers, so the first sweep
    // fires within that window after the handshakes finish.
    let handle = a.spawn_nat_classify_loop();

    // Wait up to 3 s for the sweep to land.
    let mut classified = false;
    for _ in 0..30 {
        if a.nat_class() == NatClass::Open {
            classified = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        classified,
        "background loop should classify within 3s; got {:?}",
        a.nat_class(),
    );
    handle.abort();
}
