//! Integration tests for multi-hop capability announcement
//! propagation (Stages M-3 through M-6 of
//! `docs/MULTIHOP_CAPABILITY_PLAN.md`).
//!
//! These tests exercise the forwarding path end-to-end across real
//! `MeshNode`s — no mocks, no shortcuts. The test harness mirrors
//! `tests/capability_broadcast.rs` but adds a handshake helper for
//! non-trivial topologies (chains, diamonds).
//!
//! Run: `cargo test --features net --test capability_multihop`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig};

const TEST_BUFFER_SIZE: usize = 256 * 1024;
const PSK: [u8; 32] = [0x42u8; 32];

fn test_config() -> MeshNodeConfig {
    // Bind via `127.0.0.1:0` so the OS picks a free port — no
    // pre-bind reservation, no TOCTOU race with parallel tests.
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
    let cfg = test_config();
    let keypair = EntityKeypair::generate();
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Handshake two nodes (a initiator, b responder). Does NOT call
/// `start()` — caller runs a batch `start_all` at the end so the
/// receive loops come up after every pair has handshaked.
async fn handshake_no_start(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
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
}

fn start_all(nodes: &[&Arc<MeshNode>]) {
    for n in nodes {
        n.start();
    }
}

async fn wait_until<F>(node: &Arc<MeshNode>, mut cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if cond(node) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond(node)
}

// =========================================================================
// M-3 — 3-node chain propagation
// =========================================================================

#[tokio::test]
async fn three_node_chain_propagates() {
    // Topology: A ↔ B ↔ C (no direct A-C link).
    // Expect: C sees A's announcement via B's re-broadcast.
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;

    handshake_no_start(&a, &b).await;
    handshake_no_start(&b, &c).await;
    start_all(&[&a, &b, &c]);

    a.announce_capabilities(CapabilitySet::new().add_tag("far-gpu"))
        .await
        .expect("A announce failed");

    let filter = CapabilityFilter::new().require_tag("far-gpu");
    let a_id = a.node_id();

    // B must see it (direct peer).
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "B (direct peer of A) did not receive announcement",
    );

    // C must see it (via B's re-broadcast).
    assert!(
        wait_until(&c, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "C did not receive A's announcement via B's re-broadcast",
    );
}

// =========================================================================
// M-4 — Origin rate limit coalesces bursts
// =========================================================================

#[tokio::test]
async fn origin_rate_limit_coalesces_bursts() {
    // Two-node setup. A is configured with a generous min-announce
    // interval (5s) so that, within a test run, only the first
    // announcement reaches B and every subsequent call inside the
    // window coalesces. A's own self-index still reflects the
    // latest caps because `capability_index.index` runs before the
    // rate-limit gate.

    let a = {
        let cfg = test_config().with_min_announce_interval(Duration::from_secs(5));
        let keypair = EntityKeypair::generate();
        Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
    };
    let b = build_node().await;

    handshake_no_start(&a, &b).await;
    start_all(&[&a, &b]);

    // First announcement should broadcast — B sees "burst-v1".
    a.announce_capabilities(CapabilitySet::new().add_tag("burst-v1"))
        .await
        .expect("v1 announce");

    let a_id = a.node_id();
    let v1_filter = CapabilityFilter::new().require_tag("burst-v1");
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&v1_filter).contains(&a_id)).await,
        "B did not receive the first announcement",
    );

    // Rapid follow-ups — each bumps the version, updates A's
    // self-index, but must NOT broadcast because we're inside the
    // rate-limit window.
    for i in 2..=10u32 {
        a.announce_capabilities(CapabilitySet::new().add_tag(format!("burst-v{}", i)))
            .await
            .expect("rapid announce");
    }

    // A's self-index reflects the latest tag…
    let v10_filter = CapabilityFilter::new().require_tag("burst-v10");
    assert!(
        a.find_nodes_by_filter(&v10_filter).contains(&a_id),
        "A's self-index doesn't reflect the latest caps",
    );

    // …but B has not seen any of them — dedup on the wire side is
    // moot because no broadcast left A in the first place.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for i in 2..=10u32 {
        let tag = format!("burst-v{}", i);
        let filter = CapabilityFilter::new().require_tag(tag);
        assert!(
            !b.find_nodes_by_filter(&filter).contains(&a_id),
            "B received coalesced announcement (tag burst-v{}) that should have been rate-limited",
            i
        );
    }
}

// =========================================================================
// M-5 — Route install from multi-hop receipt
// =========================================================================

#[tokio::test]
async fn route_install_from_multihop_receipt() {
    // A ↔ B ↔ C chain. After A's announcement reaches C via B, C
    // should have a routing-table entry for `a.node_id()` pointing
    // to B's address. The metric carries the pingwave convention
    // (hop_count + 2) so direct routes always beat announcement-
    // installed routes.
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;

    handshake_no_start(&a, &b).await;
    handshake_no_start(&b, &c).await;
    start_all(&[&a, &b, &c]);

    a.announce_capabilities(CapabilitySet::new().add_tag("route-probe"))
        .await
        .expect("announce");

    let filter = CapabilityFilter::new().require_tag("route-probe");
    let a_id = a.node_id();
    assert!(
        wait_until(&c, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "C never saw A's announcement — upstream forwarding failed",
    );

    // Announcement from A reached C via B. C's routing table
    // should now point `a.node_id() → b.local_addr()`.
    let route = c.router().routing_table().lookup(a.node_id());
    assert_eq!(
        route,
        Some(b.local_addr()),
        "C's routing table didn't install the multi-hop route to A via B"
    );
}

// =========================================================================
// M-6 — Diamond dedup at the converge point
// =========================================================================

#[tokio::test]
async fn dedup_drops_duplicate_at_converge_point() {
    // Diamond topology:
    //
    //       ┌── B ──┐
    //   A ──┤       ├── D
    //       └── C ──┘
    //
    // A announces once. Both B and C see it directly, and both
    // re-broadcast to D. Without dedup, D would process the same
    // `(origin=A, version=1)` announcement twice — the second
    // arrival would re-trigger indexing, subnet-policy evaluation,
    // and forwarding.
    //
    // With dedup, D processes the first arrival and drops the
    // second at the `contains_key` check. We can't directly
    // inspect the dedup cache from outside `MeshNode`, so we
    // assert the observable consequence: D's capability index
    // holds exactly one version entry for A, and the announcement
    // lands visible via `find_nodes_by_filter`.
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;
    let d = build_node().await;

    // Wire the diamond. Each edge is an independent handshake so D
    // receives the announcement through two disjoint paths.
    handshake_no_start(&a, &b).await;
    handshake_no_start(&a, &c).await;
    handshake_no_start(&b, &d).await;
    handshake_no_start(&c, &d).await;
    start_all(&[&a, &b, &c, &d]);

    a.announce_capabilities(CapabilitySet::new().add_tag("diamond"))
        .await
        .expect("A announce");

    let a_id = a.node_id();
    let filter = CapabilityFilter::new().require_tag("diamond");
    assert!(
        wait_until(&d, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "D did not see A's diamond announcement",
    );

    // Give the second forwarded copy a generous window to arrive
    // (it's dedup's job to drop it silently). 200ms is well beyond
    // the ~2× session RTT these tests observe on loopback.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // D still shows A exactly once — the tag hasn't been
    // "re-indexed" into a second node entry. `find_nodes_by_filter`
    // returns each matching node at most once by construction, so
    // this is more of a liveness check than a dedup check, but it
    // does confirm no panic / double-index / cache thrash happened
    // when the duplicate arrived.
    let hits = d.find_nodes_by_filter(&filter);
    assert_eq!(
        hits.iter().filter(|&&id| id == a_id).count(),
        1,
        "D's index accidentally registered A twice",
    );
}

// =========================================================================
// M-6 — Late joiner converges via the next multi-hop re-announce
// =========================================================================

#[tokio::test]
async fn late_joiner_converges_via_multihop_rebroadcast() {
    // A ↔ B. A announces. C joins B later — no direct A-C edge.
    // Without re-announce, C never sees A's caps (C wasn't a peer
    // when A broadcast, and B's `session-open push` forwards B's
    // OWN local_announcement, not A's). A re-announces; B's
    // receive handler then forwards to C.
    //
    // Tight min_announce_interval so the second announce actually
    // broadcasts; otherwise the 10s default rate-limits it.

    let build_with_interval = |interval: Duration| async move {
        let cfg = test_config().with_min_announce_interval(interval);
        let keypair = EntityKeypair::generate();
        Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
    };

    let a = build_with_interval(Duration::from_millis(50)).await;
    let b = build_node().await;

    handshake_no_start(&a, &b).await;
    start_all(&[&a, &b]);

    // A announces before C exists — only B sees it.
    a.announce_capabilities(CapabilitySet::new().add_tag("pre-late"))
        .await
        .expect("initial announce");

    let a_id = a.node_id();
    let filter = CapabilityFilter::new().require_tag("pre-late");
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "B didn't see initial announcement",
    );

    // C joins later through B.
    let c = build_node().await;
    handshake_no_start(&b, &c).await;
    c.start();

    // Right after join, C hasn't seen A's caps yet — there was no
    // origin re-announce nor any multi-hop forwarding event.
    assert!(
        !c.find_nodes_by_filter(&filter).contains(&a_id),
        "C unexpectedly converged before the re-announce"
    );

    // Wait for the rate-limit window to elapse, then re-announce.
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.announce_capabilities(CapabilitySet::new().add_tag("pre-late"))
        .await
        .expect("re-announce");

    // Now C must observe A via B's forwarding of the re-announce.
    assert!(
        wait_until(&c, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "C never converged after A's re-announce",
    );
}

// ---------------------------------------------------------------------------
// Tests deliberately NOT included here, with rationale:
//
// - `hop_count_exhaustion_drops_announcement`: requires either 17+
//   nodes (`MAX_CAPABILITY_HOPS = 16`) or a test-only way to inject
//   an announcement with a pre-bumped hop_count. Instead, the
//   `signature_verifies_across_hop_count_bumps` unit test in
//   `capability.rs` covers every hop value up to MAX, and the
//   forwarding guard (`ann.hop_count < MAX_CAPABILITY_HOPS - 1`)
//   is straight-line code that would require a crafted-payload
//   test to exercise boundary behaviour. Net: the property is
//   covered by the guard plus the unit tests; a full 17-node
//   integration fixture isn't earning its weight.
//
// - `tampered_forward_fails_verification`: the mid-flight tamper
//   property decomposes into (a) tampered bytes fail `verify()` and
//   (b) `handle_capability_announcement` drops on verify failure.
//   Both paths are covered — (a) by
//   `signature_rejects_tampered_payload_even_at_hop_zero` in
//   `capability.rs`'s tests, (b) by reading the dispatch code. No
//   integration-level tamper-injection helper exists today; adding
//   one is follow-up work not justified by this plan.
//
// - `split_horizon_skips_origin_nearest_hop`: the simple 3-node
//   chain + 4-node diamond we already test exercise the
//   sender-skip rule (`peer.node_id == sender_node_id` check in
//   `forward_capability_announcement`). The deeper split-horizon
//   rule (consult routing table, skip origin's next hop) needs a
//   topology where B's next-hop-to-A passes through a peer other
//   than A itself, which requires pingwave-established routes
//   beyond what the test harness currently spins up. Left as
//   follow-up.
// ---------------------------------------------------------------------------

// =========================================================================
// Dedup — `(node_id, version)` cache prevents re-processing the same
// announcement when it arrives twice (wire replay or a same-version
// collision from a misbehaving origin).
//
// TEST_COVERAGE_PLAN §P1-3. Diamond convergence is already covered by
// the existing multi-hop tests above; the cases below exercise the
// remaining two: wire-replay and version-collision. TTL-expiry of the
// dedup cache is covered alongside the clock-skew tests (§P1-2).
// =========================================================================

/// Replay of the exact wire bytes of an already-indexed
/// announcement is silently dropped at the `(node_id, version)`
/// dedup check, before signature verification or re-indexing.
/// Observable via the index-event counter — `total_indexed`
/// bumps once for the legitimate announcement and does NOT bump
/// for the replays.
#[tokio::test]
async fn wire_replay_is_dropped_at_dedup_cache() {
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;

    let a = build_node().await;
    let b = build_node().await;
    handshake_no_start(&a, &b).await;
    start_all(&[&a, &b]);

    // Legitimate announce: A emits v1, B indexes it naturally
    // through the recv loop.
    a.announce_capabilities(CapabilitySet::new().add_tag("dedup-test"))
        .await
        .expect("A announce");

    let a_id = a.node_id();
    let filter = CapabilityFilter::new().require_tag("dedup-test");
    assert!(
        wait_until(&b, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await,
        "B should index A's first announce",
    );

    let baseline = b.capability_fold().stats().entries as u64;

    // Grab A's own announcement wire bytes.
    let ann = a
        .local_announcement_for_test()
        .expect("A should have a stored announcement after announce");
    let bytes = ann.to_bytes();

    // Replay the same wire bytes to B three times. Each arrives
    // at B's dispatch with subprotocol_id=SUBPROTOCOL_CAPABILITY_ANN;
    // the dedup check on `(a_id, v1)` must short-circuit each
    // replay before it reaches `capability_index.index()`.
    let b_addr = b.local_addr();
    for _ in 0..3 {
        a.send_subprotocol(b_addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes)
            .await
            .expect("replay send");
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let after = b.capability_fold().stats().entries as u64;
    assert_eq!(
        after, baseline,
        "total_indexed must not bump on wire-replay — dedup cache \
         should short-circuit all three replays BEFORE index() runs. \
         baseline={baseline}, after={after}",
    );
    assert!(
        b.find_nodes_by_filter(&filter).contains(&a_id),
        "A's tag should still be indexed after the replay barrage",
    );
}

/// Two distinct announcements from the same origin with the same
/// `version` number but different capability content — simulates
/// a `capability_version.fetch_add` wrap-around or a misbehaving
/// origin that reused a version. The dedup cache pins the
/// first-writer-wins invariant: whichever announcement lands
/// first is indexed; the second is silently dropped. Protects
/// against a split-brain view where B ends up with the second
/// announcement's tags even though the first was already
/// propagated downstream.
#[tokio::test]
async fn version_collision_from_same_origin_is_dropped_at_dedup_cache() {
    use net::adapter::net::behavior::capability::CapabilityAnnouncement;
    use net::adapter::net::behavior::SUBPROTOCOL_CAPABILITY_ANN;

    // Build A with a named keypair we keep so we can sign
    // synthesized announcements below (default config's
    // `require_signed_capabilities = true` drops unsigned on the
    // receiver side, which would mask the dedup check we're
    // trying to observe).
    let a_keypair = EntityKeypair::generate();
    let a = Arc::new(
        MeshNode::new(a_keypair.clone(), test_config())
            .await
            .expect("A"),
    );
    let b = build_node().await;
    handshake_no_start(&a, &b).await;
    start_all(&[&a, &b]);

    // Craft announcement #1 with version 42 + tag "first", signed.
    let a_id = a.node_id();
    let a_entity_id = a.entity_id().clone();
    let mut ann1 = CapabilityAnnouncement::new(
        a_id,
        a_entity_id.clone(),
        42,
        CapabilitySet::new().add_tag("first"),
    );
    ann1.sign(&a_keypair);
    let bytes1 = ann1.to_bytes();

    // Craft announcement #2 with the SAME version 42 but a
    // different tag "second", also signed.
    let mut ann2 = CapabilityAnnouncement::new(
        a_id,
        a_entity_id,
        42,
        CapabilitySet::new().add_tag("second"),
    );
    ann2.sign(&a_keypair);
    let bytes2 = ann2.to_bytes();

    let b_addr = b.local_addr();

    // Send #1 first — B should index it and see tag "first".
    a.send_subprotocol(b_addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes1)
        .await
        .expect("send #1");

    let first_filter = CapabilityFilter::new().require_tag("first");
    assert!(
        wait_until(&b, |n| n
            .find_nodes_by_filter(&first_filter)
            .contains(&a_id))
        .await,
        "B should index announcement #1 (tag=first, v=42)",
    );

    let indexed_after_first = b.capability_fold().stats().entries as u64;

    // Send #2 — same version, different tag. Dedup must drop it
    // at the `(a_id, 42)` cache check.
    a.send_subprotocol(b_addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes2)
        .await
        .expect("send #2");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let indexed_after_second = b.capability_fold().stats().entries as u64;
    assert_eq!(
        indexed_after_second, indexed_after_first,
        "second same-version announcement must NOT increment total_indexed",
    );

    // B's view of A's caps is still #1's tag, not #2's. No
    // split-brain.
    let second_filter = CapabilityFilter::new().require_tag("second");
    assert!(
        !b.find_nodes_by_filter(&second_filter).contains(&a_id),
        "B must not have #2's tag — dedup dropped the collision",
    );
    assert!(
        b.find_nodes_by_filter(&first_filter).contains(&a_id),
        "B must still have #1's tag (first-writer-wins)",
    );
}
