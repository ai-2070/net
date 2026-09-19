//! Rust SDK smoke tests for the port-mapping surface
//! (`MeshBuilder::try_port_mapping`).
//!
//! Stage 4b integration. Verifies the builder flag threads
//! through to `MeshNode::start`, without actually exercising
//! the real router I/O (which is covered by the `#[ignore]`d
//! test in `tests/port_mapping_real_router.rs` at the core
//! crate level).
//!
//! **Framing reminder** (plan §5): port mapping is an
//! optimization surface — nodes without `try_port_mapping(true)`
//! still reach every peer via the routed-handshake path. These
//! tests assert that the flag is honored + integrated with the
//! stats surface, not that it's required for any connectivity.

#![cfg(all(feature = "net", feature = "port-mapping"))]

use std::time::Duration;

use net_sdk::mesh::{Mesh, MeshBuilder};

async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// Default `try_port_mapping(false)` — no port-mapping task is
/// spawned, and stats show inactive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_port_mapping_off_by_default() {
    let psk = [0x42u8; 32];
    let mesh = build_mesh(&psk).await;
    let stats = mesh.traversal_stats();
    assert!(!stats.port_mapping_active);
    assert_eq!(stats.port_mapping_external, None);
    assert_eq!(stats.port_mapping_renewals, 0);
}

/// `MeshBuilder::try_port_mapping(true).build()` produces a mesh
/// whose `start()` spawns the port-mapping task. The task
/// attempts real UPnP / NAT-PMP discovery which will most
/// likely time out in a CI environment without a router —
/// the mesh doesn't hang and the stats reflect the inactive
/// state.
///
/// NB: on a developer machine with a live UPnP router, this
/// test will briefly contact that router. That's expected and
/// documented as the "real traffic is fine here" case — the
/// task cleans up via `shutdown()` at the end. Operators who
/// want hermetic tests should build with `--features
/// nat-traversal` (without `port-mapping`), which compiles the
/// flag out of this test file entirely.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_port_mapping_on_doesnt_block_boot() {
    let psk = [0x42u8; 32];
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap()
        .try_port_mapping(true)
        .build()
        .await
        .unwrap();

    let start_t = tokio::time::Instant::now();
    mesh.start();
    let elapsed = start_t.elapsed();

    // start() is non-blocking — spawns tasks and returns.
    assert!(
        elapsed < Duration::from_millis(500),
        "start() should be non-blocking; took {elapsed:?}",
    );

    // Give the port-mapping task a moment to exist (or fail
    // probe). Don't wait out the full 3 s NAT-PMP + UPnP budget;
    // we're testing the wiring, not the outcome.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Stats are readable — doesn't matter whether the install
    // succeeded or is still in-flight. The property under test
    // is "no panic, no hang."
    let _ = mesh.traversal_stats();
}
