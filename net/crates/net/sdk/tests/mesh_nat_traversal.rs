//! Rust SDK smoke test for the NAT-traversal surface
//! (`Mesh::nat_type`, `reflex_addr`, `probe_reflex`,
//! `reclassify_nat`, `connect_direct`, `traversal_stats`).
//!
//! Exercises the end-to-end flow from the SDK's perspective:
//! meshes build, classify, announce caps, run reflex probes,
//! and resolve `connect_direct` with stats bumped. Mirror of
//! the core-crate integration suite but against the idiomatic
//! SDK wrappers.
//!
//! **Framing reminder** (plan §5): NAT traversal is an
//! optimization, not a connectivity requirement. These tests
//! assert stats + classification semantics; they do NOT assert
//! the routed-handshake path is "broken" when traversal is
//! disabled — it always works.

#![cfg(all(feature = "net", feature = "nat-traversal"))]

use std::time::Duration;

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::traversal::classify::NatClass;
use net_sdk::error::SdkError;
use net_sdk::mesh::{Mesh, MeshBuilder};

async fn build_mesh(psk: &[u8; 32]) -> Mesh {
    MeshBuilder::new("127.0.0.1:0", psk)
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn connect_pair(a: &Mesh, b: &Mesh) {
    let pub_b = *b.inner().public_key();
    let nid_b = b.inner().node_id();
    let nid_a = a.inner().node_id();
    let addr_b = b.inner().local_addr();
    let (r1, r2) = tokio::join!(b.inner().accept(nid_a), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.inner().connect(addr_b, &pub_b, nid_b).await
    });
    r1.expect("accept");
    r2.expect("connect");
}

/// Freshly built meshes classify as `Unknown` and report no
/// reflex address. Background classification doesn't run until
/// `start()` and ≥2 peers are connected.
///
/// Also the SDK's stats-shape parity pin (Stage 5): the full
/// snapshot — punch outcomes, derived failures, the three
/// failure-cause counters, upgrade activity, and port-mapping
/// state — boots to zero/empty. Every field is asserted by name,
/// so a core-snapshot field that vanishes (or a binding that
/// stops forwarding one) fails to compile / fails here. The
/// Node, Python, and Go bindings mirror this same shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_classification_state_is_unknown() {
    let psk = [0x42u8; 32];
    let a = build_mesh(&psk).await;

    assert_eq!(a.nat_type(), NatClass::Unknown);
    assert!(a.reflex_addr().is_none());

    let stats = a.traversal_stats();
    assert_eq!(stats.punches_attempted, 0);
    assert_eq!(stats.punches_succeeded, 0);
    assert_eq!(stats.punches_failed, 0);
    assert_eq!(stats.relay_fallbacks, 0);
    assert_eq!(stats.punch_timeouts, 0);
    assert_eq!(stats.punch_rejections, 0);
    assert_eq!(stats.rendezvous_no_relay, 0);
    assert_eq!(stats.upgrades_attempted, 0);
    assert_eq!(stats.upgrades_succeeded, 0);
    assert_eq!(stats.upgrades_deferred_busy, 0);
    assert!(!stats.port_mapping_active);
    assert!(stats.port_mapping_external.is_none());
    assert_eq!(stats.port_mapping_renewals, 0);
}

/// Two-peer reflex probe end-to-end via the SDK surface:
/// `mesh_a.probe_reflex(b.node_id())` returns A's bind address
/// (localhost → reflex == bind).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_reflex_returns_source_address() {
    let psk = [0x42u8; 32];
    let a = build_mesh(&psk).await;
    let b = build_mesh(&psk).await;
    connect_pair(&a, &b).await;
    a.inner().start();
    b.inner().start();

    let a_bind = a.local_addr();
    let observed = a.probe_reflex(b.node_id()).await.expect("probe_reflex");
    assert_eq!(observed, a_bind, "localhost: reflex equals bind");
}

/// `probe_reflex` against an unknown peer surfaces as
/// `SdkError::Traversal { kind: "peer-not-reachable", .. }` —
/// the stable discriminator every binding exposes. Exercises
/// the `TraversalError → SdkError` conversion path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_reflex_unknown_peer_surfaces_stable_kind() {
    let psk = [0x42u8; 32];
    let a = build_mesh(&psk).await;
    a.inner().start();

    let err = a
        .probe_reflex(0xDEAD_BEEF_FEED_CAFE)
        .await
        .expect_err("unknown peer should fail");
    match err {
        SdkError::Traversal { kind, .. } => {
            assert_eq!(
                kind, "peer-not-reachable",
                "stable kind discriminator for cross-binding parity",
            );
        }
        other => panic!("expected SdkError::Traversal, got {other:?}"),
    }
}

/// Three-peer reclassify → `NatClass::Open` on localhost. After
/// an explicit `reclassify_nat`, `reflex_addr()` is populated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reclassify_populates_open_on_localhost() {
    let psk = [0x42u8; 32];
    let a = build_mesh(&psk).await;
    let b = build_mesh(&psk).await;
    let c = build_mesh(&psk).await;

    // Build a triangle so A has ≥2 peers (needed for the
    // two-probe classification rule).
    connect_pair(&a, &b).await;
    connect_pair(&a, &c).await;
    connect_pair(&b, &c).await;
    a.inner().start();
    b.inner().start();
    c.inner().start();

    a.reclassify_nat().await;

    assert_eq!(
        a.nat_type(),
        NatClass::Open,
        "localhost loopback: reflex equals bind → Open",
    );
    assert_eq!(
        a.reflex_addr(),
        Some(a.local_addr()),
        "reflex matches local bind on localhost",
    );
}

/// `MeshBuilder::reflex_override` pins the public reflex at
/// construction. No probes fire; `nat_type()` reports `Open`
/// and `reflex_addr()` returns the pinned address from the
/// moment the mesh is built.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builder_reflex_override_forces_open() {
    let psk = [0x42u8; 32];
    let external: std::net::SocketAddr = "203.0.113.42:9001".parse().unwrap();
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap()
        .reflex_override(external)
        .build()
        .await
        .unwrap();

    assert_eq!(
        mesh.nat_type(),
        NatClass::Open,
        "override should force Open at construction",
    );
    assert_eq!(
        mesh.reflex_addr(),
        Some(external),
        "reflex_addr should reflect the override",
    );
}

/// Runtime `set_reflex_override` / `clear_reflex_override` via
/// the SDK wrapper. Installs, verifies, and clears an override
/// mid-session without needing the builder path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_reflex_override_via_sdk() {
    let psk = [0x42u8; 32];
    let mesh = MeshBuilder::new("127.0.0.1:0", &psk)
        .unwrap()
        .build()
        .await
        .unwrap();

    // Pre-override: the plain classifier path hasn't run, so
    // Unknown / None.
    assert_eq!(mesh.nat_type(), NatClass::Unknown);
    assert!(mesh.reflex_addr().is_none());

    let external: std::net::SocketAddr = "203.0.113.99:4242".parse().unwrap();
    mesh.set_reflex_override(external);

    assert_eq!(
        mesh.nat_type(),
        NatClass::Open,
        "runtime override should flip nat_type to Open",
    );
    assert_eq!(
        mesh.reflex_addr(),
        Some(external),
        "reflex_addr should reflect the runtime override",
    );

    mesh.clear_reflex_override();

    assert_eq!(
        mesh.nat_type(),
        NatClass::Unknown,
        "clear should reset nat_type",
    );
    assert!(
        mesh.reflex_addr().is_none(),
        "clear should reset reflex_addr to None",
    );

    // clear is idempotent — second clear is a no-op.
    mesh.clear_reflex_override();
    assert_eq!(mesh.nat_type(), NatClass::Unknown);
    assert!(mesh.reflex_addr().is_none());
}

/// `connect_direct` end-to-end via the SDK: Open×Open pair
/// picks the Direct action, succeeds on the direct
/// handshake against the peer's advertised reflex, and leaves
/// both `punches_attempted` + `relay_fallbacks` at zero (per
/// the substrate's documented stats semantics — a successful
/// direct connect is not a relay fallback).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_direct_open_pair_via_sdk() {
    let psk = [0x42u8; 32];
    let a = build_mesh(&psk).await;
    let r = build_mesh(&psk).await;
    let b = build_mesh(&psk).await;
    let x = build_mesh(&psk).await;

    // Four-node topology — A+B need ≥2 peers each for their
    // classification sweep, so X + R together cover that; R
    // serves as the coordinator; A and B intentionally do NOT
    // directly connect (that's the connect_direct shape).
    connect_pair(&a, &r).await;
    connect_pair(&b, &r).await;
    connect_pair(&a, &x).await;
    connect_pair(&b, &x).await;
    connect_pair(&r, &x).await;
    a.inner().start();
    r.inner().start();
    b.inner().start();
    x.inner().start();

    a.reclassify_nat().await;
    b.reclassify_nat().await;
    a.inner()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("A announce");
    b.inner()
        .announce_capabilities(CapabilitySet::new())
        .await
        .expect("B announce");

    // Wait for A to see B's reflex in its capability index.
    let b_id = b.node_id();
    let b_bind = b.local_addr();
    let mut ready = false;
    for _ in 0..30 {
        if a.inner().peer_reflex_addr(b_id) == Some(b_bind) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "A should see B's reflex");

    let before = a.traversal_stats();
    let b_pub = *b.inner().public_key();
    a.connect_direct(b_id, &b_pub, r.node_id())
        .await
        .expect("connect_direct");

    let after = a.traversal_stats();
    assert_eq!(
        after.punches_attempted, before.punches_attempted,
        "Open × Open should not attempt a punch",
    );
    // `record_relay_fallback` fires only when the direct
    // handshake fails and the routed-table fallback succeeds —
    // see `adapter/net/mesh.rs:~7641-7693`. A successful direct
    // connect (the happy path this test exercises) leaves the
    // counter unchanged.
    assert_eq!(
        after.relay_fallbacks, before.relay_fallbacks,
        "successful Direct path is not a relay fallback",
    );
}
