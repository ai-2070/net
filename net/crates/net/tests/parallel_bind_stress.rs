//! Regression test for the ephemeral-port TOCTOU race that used
//! to live in every NAT-traversal test file.
//!
//! # Background
//!
//! The old `find_ports(n)` helper pattern:
//!
//! ```ignore
//! let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
//! ports.push(sock.local_addr().unwrap().port());
//! sockets.push(sock);
//! // ...
//! drop(sockets);
//! tokio::time::sleep(Duration::from_millis(10)).await;
//! ports
//! ```
//!
//! reserved a port, read it, dropped the socket, then later fed
//! the port to `MeshNode::new(addr, psk)` which re-bound. Between
//! the drop and the rebind, any other process (or parallel test)
//! could grab the port, flaking the bind with `EADDRINUSE`. cubic
//! flagged this as P2.
//!
//! # Fix
//!
//! All NAT-traversal test helpers now bind to `127.0.0.1:0` and
//! let the OS pick a free port — no predetermined-port pattern,
//! no reservation window. `local_addr()` reads the actually-bound
//! port after construction.
//!
//! # This test
//!
//! Concurrently spawns `N_NODES` mesh nodes at `127.0.0.1:0` and
//! asserts:
//!
//! 1. Every node binds successfully.
//! 2. The ports are all distinct (the OS really did pick N free
//!    ports rather than handing us the same port twice).
//!
//! Without the fix, running this test alongside itself (or any
//! other test suite with a `find_ports` helper) would eventually
//! produce `EADDRINUSE` flakes. After the fix, parallel binding
//! is race-free by construction.
//!
//! Run: `cargo test --features net --test parallel_bind_stress`

#![cfg(feature = "net")]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

/// Concurrency target. The fast-build CI runs 32 in ~hundreds of
/// ms; under `cargo-llvm-cov`'s `-C instrument-coverage` build
/// every per-node tokio task + UDP socket setup is heavily slowed,
/// and the GitHub runner's file-descriptor / IO budget can't
/// sustain 32 simultaneous `MeshNode::new` calls — bind failures
/// surface as test panics. The TOCTOU-race regression this test
/// guards against doesn't need 32 nodes to be visible; 4
/// concurrent binds already exercise the shared-port window the
/// old `find_ports` helper used to introduce. Scale down when
/// `CARGO_LLVM_COV` indicates we're inside the coverage workflow.
fn n_nodes() -> usize {
    if std::env::var_os("CARGO_LLVM_COV").is_some() {
        4
    } else {
        32
    }
}
const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_mesh_nodes_bind_to_distinct_ports() {
    // Spawn `n` concurrent construction tasks. Each binds to
    // `:0` independently; the OS allocates a distinct port per
    // socket.
    //
    // CRITICAL: each task returns the *node*, not just the port.
    // If we returned only the port (dropping the node — and its
    // UDP socket — at the end of the spawn closure), the OS would
    // be free to hand that same port back to a later task. That
    // is exactly the TOCTOU window this test claims to guard
    // against, so reproducing it inside the test gave a duplicate-
    // port flake (e.g. port 52605 reported twice across N nodes).
    // Holding every node alive in `nodes` until after the
    // distinct-port assertion forces the kernel to keep all
    // ports allocated simultaneously.
    let n = n_nodes();
    let handles: Vec<_> = (0..n)
        .map(|_| {
            tokio::spawn(async move {
                let keypair = EntityKeypair::generate();
                MeshNode::new(keypair, test_config()).await
            })
        })
        .collect();

    let mut nodes: Vec<MeshNode> = Vec::with_capacity(n);
    for (i, h) in handles.into_iter().enumerate() {
        let node = h
            .await
            .expect("spawn task panicked")
            .unwrap_or_else(|e| panic!("node {i}: MeshNode::new failed: {e}"));
        nodes.push(node);
    }

    let ports: Vec<u16> = nodes.iter().map(|n| n.local_addr().port()).collect();

    // Distinct-port property. A HashSet of the collected ports
    // must have exactly `n` entries — any collision means the
    // OS handed us the same port twice while all sockets are
    // simultaneously bound, which shouldn't happen on any platform
    // we support.
    let distinct: HashSet<u16> = ports.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        n,
        "expected {n} distinct ports; got {} ({:?})",
        distinct.len(),
        ports,
    );

    // Sanity: all non-zero (we asked for `:0`; kernel must pick a
    // real port).
    for (i, p) in ports.iter().enumerate() {
        assert_ne!(*p, 0, "node {i} got port 0; kernel didn't allocate");
    }

    // `nodes` drops here, releasing every bound socket at once.
    drop(nodes);
}
