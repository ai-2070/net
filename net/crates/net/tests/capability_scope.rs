//! Integration test for tag-based scoped capability discovery.
//!
//! Reserved `scope:*` tags inside the announcer's `CapabilitySet`
//! resolve to a `CapabilityScope` that callers can filter against
//! via `MeshNode::find_nodes_by_filter_scoped`. Enforcement is
//! purely query-side — the wire format and forwarder logic are
//! untouched (see `docs/SCOPED_CAPABILITIES_PLAN.md`).
//!
//! Three nodes:
//! - A tagged `scope:tenant:oem-123`,
//! - B tagged `scope:tenant:corp-acme`,
//! - C unscoped (resolves to `Global`).
//!
//! Verifies:
//! - `ScopeFilter::Tenant("oem-123")` returns A and C, not B
//!   (Global is permissive, B's tenant doesn't match).
//! - `ScopeFilter::Any` returns all three.
//! - `ScopeFilter::GlobalOnly` returns only C.
//!
//! Run: `cargo test --features net --test capability_scope`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::capability::{
    CapabilityFilter, CapabilityRequirement, CapabilitySet, ScopeFilter,
};
use net::adapter::net::{
    EntityKeypair, MeshNode, MeshNodeConfig, SocketBufferConfig, SubnetId, SubnetPolicy, SubnetRule,
};

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
    Arc::new(
        MeshNode::new(keypair, test_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Build a MeshNode pinned to a specific subnet. Used by the
/// `SubnetLocal` test to set up same-subnet vs cross-subnet pairs;
/// the discovery-side scope filter consults `MeshNode.local_subnet`
/// as the caller's subnet and `peer_subnets` as the candidate's.
async fn build_node_in_subnet(subnet: SubnetId) -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    let cfg = test_config().with_subnet(subnet);
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Build a MeshNode with a subnet AND a `SubnetPolicy`. The policy
/// is what makes `peer_subnets` populate on incoming announcements
/// — without it, the dispatch handler skips the
/// `policy.assign(&caps)` call entirely. Used by the P1 regression
/// to exercise the warm-up-permissive branch.
async fn build_node_with_policy(subnet: SubnetId, policy: Arc<SubnetPolicy>) -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    let cfg = test_config().with_subnet(subnet).with_subnet_policy(policy);
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Minimal `SubnetPolicy` that maps `region:<name>` tags to a
/// 1-level subnet id. Mirrors the shape used by
/// `tests/subnet_enforcement.rs::shared_policy`.
fn region_policy() -> Arc<SubnetPolicy> {
    let rule = SubnetRule::new("region:", 0).map("us", 3).map("eu", 4);
    Arc::new(SubnetPolicy::new().add_rule(rule))
}

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

async fn wait_until<F>(node: &Arc<MeshNode>, mut cond: F) -> bool
where
    F: FnMut(&MeshNode) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if cond(node) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    cond(node)
}

#[tokio::test]
async fn tenant_scoped_discovery_filters_unrelated_tenants() {
    // Three providers around an observer D. Each provider tags its
    // capability set differently:
    //
    //   A — scope:tenant:oem-123
    //   B — scope:tenant:corp-acme
    //   C — no scope tag (resolves to Global)
    //
    // D handshakes with each so its capability index sees all three.
    let a = build_node().await;
    let b = build_node().await;
    let c = build_node().await;
    let d = build_node().await;

    handshake(&d, &a).await;
    handshake(&d, &b).await;
    handshake(&d, &c).await;

    // Same capability filter on all three — they only differ in the
    // scope tag. Using "model:llama3-70b" as a discriminator that's
    // common to a GPU pool.
    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("model:llama3-70b")
            .with_tenant_scope("oem-123"),
    )
    .await
    .expect("A announce");
    b.announce_capabilities(
        CapabilitySet::new()
            .add_tag("model:llama3-70b")
            .with_tenant_scope("corp-acme"),
    )
    .await
    .expect("B announce");
    c.announce_capabilities(CapabilitySet::new().add_tag("model:llama3-70b"))
        .await
        .expect("C announce");

    let filter = CapabilityFilter::new().require_tag("model:llama3-70b");
    let a_id = a.node_id();
    let b_id = b.node_id();
    let c_id = c.node_id();

    // First wait for all three announcements to arrive at D under
    // an unfiltered query — the scope filter is a per-call concern,
    // it shouldn't affect propagation.
    let arrived = wait_until(&d, |n| {
        let peers = n.find_nodes_by_filter(&filter);
        peers.contains(&a_id) && peers.contains(&b_id) && peers.contains(&c_id)
    })
    .await;
    assert!(
        arrived,
        "D did not observe all three capability announcements"
    );

    // Tenant("oem-123"): A (matches tenant) + C (Global is
    // permissive). B excluded — its tenant tag doesn't match.
    let oem = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Tenant("oem-123"));
    assert!(oem.contains(&a_id), "tenant:oem-123 must include A");
    assert!(
        oem.contains(&c_id),
        "tenant:oem-123 must include unscoped C (Global is permissive)"
    );
    assert!(
        !oem.contains(&b_id),
        "tenant:oem-123 must exclude B (different tenant)"
    );

    // Tenant("corp-acme"): B + C, not A.
    let acme = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Tenant("corp-acme"));
    assert!(acme.contains(&b_id), "tenant:corp-acme must include B");
    assert!(
        acme.contains(&c_id),
        "tenant:corp-acme must include unscoped C"
    );
    assert!(
        !acme.contains(&a_id),
        "tenant:corp-acme must exclude A (different tenant)"
    );

    // Any: all three (no SubnetLocal candidates here).
    let any = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Any);
    assert!(
        any.contains(&a_id) && any.contains(&b_id) && any.contains(&c_id),
        "ScopeFilter::Any must return all non-SubnetLocal peers; got {:?}",
        any
    );

    // GlobalOnly: just C (the only untagged peer).
    let global = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::GlobalOnly);
    assert!(global.contains(&c_id), "GlobalOnly must include C");
    assert!(
        !global.contains(&a_id) && !global.contains(&b_id),
        "GlobalOnly must exclude tenant-scoped A and B; got {:?}",
        global
    );
}

#[tokio::test]
async fn find_best_node_scoped_picks_higher_scoring_within_tenant() {
    // Two providers in the same tenant, different VRAM — under
    // a `prefer_more_vram` weight the one with more VRAM should
    // win. Exercises the scored-pick path inside the scope filter,
    // which is a separate code path from `find_nodes_scoped`
    // (does its own per-candidate score + max selection).
    use net::adapter::net::behavior::capability::{GpuInfo, GpuVendor, HardwareCapabilities};

    let a = build_node().await; // 24 GB VRAM
    let b = build_node().await; // 80 GB VRAM
    let d = build_node().await; // observer

    handshake(&d, &a).await;
    handshake(&d, &b).await;

    let hw_24gb =
        HardwareCapabilities::new().with_gpu(GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24));
    let hw_80gb = HardwareCapabilities::new().with_gpu(GpuInfo::new(GpuVendor::Nvidia, "H100", 80));

    a.announce_capabilities(
        CapabilitySet::new()
            .with_hardware(hw_24gb)
            .add_tag("model:llama3-70b")
            .with_tenant_scope("oem-123"),
    )
    .await
    .expect("A announce");
    b.announce_capabilities(
        CapabilitySet::new()
            .with_hardware(hw_80gb)
            .add_tag("model:llama3-70b")
            .with_tenant_scope("oem-123"),
    )
    .await
    .expect("B announce");

    let a_id = a.node_id();
    let b_id = b.node_id();

    // Wait for both announcements to land at D.
    let arrived = wait_until(&d, |n| {
        let filter = CapabilityFilter::new().require_tag("model:llama3-70b");
        let peers = n.find_nodes_by_filter(&filter);
        peers.contains(&a_id) && peers.contains(&b_id)
    })
    .await;
    assert!(arrived, "D did not see both announcements");

    let req =
        CapabilityRequirement::from_filter(CapabilityFilter::new().require_tag("model:llama3-70b"))
            .prefer_vram(1.0);

    // Scoped to oem-123 — both candidates are in scope; B should
    // win on VRAM.
    let winner = d.find_best_node_scoped(&req, &ScopeFilter::Tenant("oem-123"));
    assert_eq!(
        winner,
        Some(b_id),
        "expected B (80 GB VRAM) to win the tenant-scoped scored pick, got {:?}",
        winner
    );

    // Different tenant — no candidates, so no winner.
    let none = d.find_best_node_scoped(&req, &ScopeFilter::Tenant("corp-acme"));
    assert!(
        none.is_none(),
        "expected None for non-matching tenant, got {:?}",
        none
    );
}

#[tokio::test]
async fn subnet_local_scope_excludes_cross_subnet_peers() {
    // SubnetLocal is the strictest scope: providers tagged
    // `scope:subnet-local` are visible only to peers in the
    // same subnet. Exercises the same-subnet predicate plumbed
    // from `MeshNode::peer_subnets` through the index closure.
    let subnet_x = SubnetId::new(&[3, 7]);
    let subnet_y = SubnetId::new(&[3, 8]);

    let a = build_node_in_subnet(subnet_x).await; // same subnet as observer
    let b = build_node_in_subnet(subnet_y).await; // different subnet
    let d = build_node_in_subnet(subnet_x).await; // observer

    handshake(&d, &a).await;
    handshake(&d, &b).await;

    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("software:photoshop")
            .with_subnet_local_scope(),
    )
    .await
    .expect("A announce");
    b.announce_capabilities(
        CapabilitySet::new()
            .add_tag("software:photoshop")
            .with_subnet_local_scope(),
    )
    .await
    .expect("B announce");

    let a_id = a.node_id();
    let b_id = b.node_id();

    let filter = CapabilityFilter::new().require_tag("software:photoshop");

    // Both announcements arrive (the wire is permissive — scope is
    // a *query* concern). Wait until D's index has indexed them.
    let arrived = wait_until(&d, |n| {
        let peers = n.find_nodes_by_filter(&filter);
        peers.contains(&a_id) && peers.contains(&b_id)
    })
    .await;
    assert!(arrived, "D did not see both announcements");

    // No `local_subnet_policy` is installed on D, so its
    // `peer_subnets` map stays permanently empty —
    // `handle_capability_announcement` only writes that map when
    // a policy is set. Treating "unknown" as "same subnet" in
    // that configuration would silently leak every peer through
    // `SameSubnet` (Cubic P1). The fix: without a policy, unknown
    // means unknown, and unknown is excluded.
    //
    // The raw `find_nodes_by_filter` still returns both A and B
    // (the wire is permissive). Only the scoped variant filters
    // them out at query time.
    let same = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet);
    assert!(
        !same.contains(&a_id) && !same.contains(&b_id),
        "without local_subnet_policy, SameSubnet must NOT admit \
         peers whose subnet hasn't been derived (would leak all \
         peers as same-subnet); got {:?}",
        same
    );

    // The strict invariant we *can* exercise here is that
    // SubnetLocal candidates are excluded from `Any` — that's
    // pure scope-tag resolution, no subnet lookup needed.
    let any = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Any);
    assert!(
        !any.contains(&a_id) && !any.contains(&b_id),
        "SubnetLocal-tagged providers must NOT appear under Any \
         (they explicitly opted out of cross-subnet discovery), got {:?}",
        any
    );

    // And tenant queries must not pick them up either.
    let tenant = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Tenant("anything"));
    assert!(
        !tenant.contains(&a_id) && !tenant.contains(&b_id),
        "SubnetLocal-tagged providers must NOT appear under tenant queries, got {:?}",
        tenant
    );
}

#[tokio::test]
async fn region_scope_filters_to_matching_region() {
    // A provider tagged for `eu-west` is visible to a region-scoped
    // query for `eu-west` and to permissive queries; not to a query
    // for `us-east`. Untagged providers (Global) stay visible across
    // both region queries by design.
    let a = build_node().await; // scope:region:eu-west
    let b = build_node().await; // scope:region:us-east
    let c = build_node().await; // untagged → Global
    let d = build_node().await; // observer

    handshake(&d, &a).await;
    handshake(&d, &b).await;
    handshake(&d, &c).await;

    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("relay-capable")
            .with_region_scope("eu-west"),
    )
    .await
    .expect("A announce");
    b.announce_capabilities(
        CapabilitySet::new()
            .add_tag("relay-capable")
            .with_region_scope("us-east"),
    )
    .await
    .expect("B announce");
    c.announce_capabilities(CapabilitySet::new().add_tag("relay-capable"))
        .await
        .expect("C announce");

    let filter = CapabilityFilter::new().require_tag("relay-capable");
    let a_id = a.node_id();
    let b_id = b.node_id();
    let c_id = c.node_id();

    let arrived = wait_until(&d, |n| {
        let peers = n.find_nodes_by_filter(&filter);
        peers.contains(&a_id) && peers.contains(&b_id) && peers.contains(&c_id)
    })
    .await;
    assert!(arrived, "D did not see all three announcements");

    // Region("eu-west"): A (matches) + C (Global is permissive).
    let eu = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Region("eu-west"));
    assert!(eu.contains(&a_id), "region:eu-west must include A");
    assert!(
        eu.contains(&c_id),
        "region:eu-west must include unscoped C (Global is permissive)"
    );
    assert!(
        !eu.contains(&b_id),
        "region:eu-west must exclude B (different region)"
    );

    // Region("us-east"): B + C, not A.
    let us = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Region("us-east"));
    assert!(us.contains(&b_id), "region:us-east must include B");
    assert!(us.contains(&c_id), "region:us-east must include unscoped C");
    assert!(
        !us.contains(&a_id),
        "region:us-east must exclude A (different region)"
    );

    // Tenant queries cross-cut regions: a tenant filter matches
    // Global and tenant-tagged peers, but not region-tagged peers
    // (different scope arm). A and B are excluded; C remains.
    let tenant_only = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Tenant("anything"));
    assert!(
        tenant_only.contains(&c_id),
        "tenant query must still include Global C"
    );
    assert!(
        !tenant_only.contains(&a_id) && !tenant_only.contains(&b_id),
        "tenant query must not return region-only peers, got {:?}",
        tenant_only
    );
}

// ============================================================================
// Regression: P1 (Cubic) — `SameSubnet` permissive default leak
// ============================================================================
//
// `find_nodes_by_filter_scoped(SameSubnet)` previously returned
// `true` for unknown peer subnets unconditionally. When a node
// runs without `local_subnet_policy`, `peer_subnets` stays empty,
// so every peer registered as "unknown" — and the closure leaked
// every peer through `SameSubnet`. Fix: warm-up permissive only
// when a policy is installed (`peer_subnets` *might* eventually
// resolve the unknown). Without a policy, unknown is excluded.

#[tokio::test]
async fn same_subnet_without_policy_excludes_unresolved_peers() {
    // No policy installed → `peer_subnets` cannot populate. A
    // cross-subnet peer announcing into the index must NOT be
    // returned by `SameSubnet`, regardless of whether its
    // capability tags match the filter.
    let me = build_node_in_subnet(SubnetId::new(&[3, 7])).await;
    let other = build_node_in_subnet(SubnetId::new(&[3, 8])).await;

    handshake(&me, &other).await;

    // `me` announces too so it self-indexes and we can verify
    // the local-node-always-returned branch survives the fix.
    me.announce_capabilities(CapabilitySet::new().add_tag("gpu"))
        .await
        .expect("me announce");
    other
        .announce_capabilities(CapabilitySet::new().add_tag("gpu"))
        .await
        .expect("other announce");

    let filter = CapabilityFilter::new().require_tag("gpu");
    let me_id = me.node_id();
    let other_id = other.node_id();

    // Sanity: the unscoped query sees both.
    let arrived = wait_until(&me, |n| {
        let peers = n.find_nodes_by_filter(&filter);
        peers.contains(&me_id) && peers.contains(&other_id)
    })
    .await;
    assert!(arrived, "me did not index both announcements");

    // SameSubnet without a policy: own id is admitted (the
    // closure short-circuits on `nid == local_node_id`); the
    // cross-subnet peer is excluded because its subnet never
    // resolves on a policy-less mesh (Cubic P1).
    let same = me.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet);
    assert!(
        same.contains(&me_id),
        "self must be returned under SameSubnet regardless of policy \
         (own node is same-subnet by definition); got {:?}",
        same
    );
    assert!(
        !same.contains(&other_id),
        "P1 regression: SameSubnet without local_subnet_policy must \
         not return peers whose subnet hasn't been derived (got {:?})",
        same
    );
}

#[tokio::test]
async fn same_subnet_with_policy_admits_unresolved_peers_via_warm_up() {
    // Genuine warm-up regression: exercises the
    // `None => policy_installed` branch in the SameSubnet
    // closure on `MeshNode::find_nodes_by_filter_scoped`.
    //
    // The dispatch handler at `mesh.rs::handle_capability_announcement`
    // gates the `peer_subnets.insert(from_node, ...)` call on
    // `signature_verified && ann.hop_count == 0`. Forwarded
    // announcements (`hop_count > 0`) skip that insert but still
    // index the announcement — leaving an indexed candidate
    // whose subnet is unknown to the receiver. That's the
    // warm-up window the closure's `None` branch handles.
    //
    // Topology: A — B — D, no direct session between A and D.
    //   - A announces with `region:eu` → A's policy assigns
    //     subnet [4].
    //   - B receives directly (hop_count=0), populates its own
    //     peer_subnets[A] = [4], indexes A, then forwards to D
    //     with hop_count=1.
    //   - D receives forwarded (hop_count=1), skips the
    //     peer_subnets.insert(A) gate, indexes A. Result: D's
    //     index has A but D's peer_subnets has only B.
    //
    // D is in subnet [3]. A is *actually* in [4] per the policy.
    // If D's SameSubnet closure ran the policy-installed warm-up
    // admit, A is returned (the warm-up admits unknowns). If the
    // closure ran the strict path, A is excluded. We assert A is
    // returned — that's the only way the `None` branch could
    // have produced this result.
    let policy = region_policy();
    let a = build_node_with_policy(SubnetId::new(&[4]), policy.clone()).await; // region:eu
    let b = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // region:us
    let d = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // observer

    // A↔B, B↔D — but NOT A↔D. Without a direct session, A's
    // announcement reaches D only via B's forward.
    handshake(&a, &b).await;
    handshake(&b, &d).await;

    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("region:eu") // policy → subnet [4]
            .add_tag("warm-up-canary"),
    )
    .await
    .expect("A announce");

    let filter = CapabilityFilter::new().require_tag("warm-up-canary");
    let a_id = a.node_id();

    // Wait for the forwarded announcement to land at D. (B
    // re-broadcasts on receipt; the indexing tick is quick but
    // the test harness uses 200ms heartbeats so we allow up to
    // 2s via wait_until.)
    let arrived = wait_until(&d, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(
        arrived,
        "forwarded announcement from A did not land at D — \
         multi-hop forwarding regressed?"
    );

    // The load-bearing assertion: D returns A under SameSubnet
    // even though A is *actually* in subnet [4] (different from
    // D's [3]). The only way this can be true is if D's
    // peer_subnets does NOT contain A (so the closure hits the
    // `None` branch) AND `policy_installed` is true (so the
    // branch returns admit). That's the warm-up regression
    // path covered.
    let same = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet);
    assert!(
        same.contains(&a_id),
        "policy-installed warm-up branch must admit unresolved \
         (forwarded-only) peers under SameSubnet; got {:?}. If \
         this assertion fails the closure's `None` branch is \
         running strict — the real-world warm-up window for \
         late-arriving direct announcements would now exclude \
         peers it shouldn't.",
        same
    );

    // Sanity: a B (whose subnet IS resolved in D's peer_subnets
    // because B handshook with D directly and announced) is also
    // included — this confirms the `Some(s) => s == my_subnet`
    // path still works alongside the warm-up branch.
    b.announce_capabilities(
        CapabilitySet::new()
            .add_tag("region:us") // policy → subnet [3], same as D
            .add_tag("warm-up-canary"),
    )
    .await
    .expect("B announce");
    let b_id = b.node_id();
    let arrived = wait_until(&d, |n| {
        n.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet)
            .contains(&b_id)
    })
    .await;
    assert!(
        arrived,
        "B (resolved same-subnet peer) must also appear under SameSubnet"
    );
}

// ============================================================================
// Note on P2 (Cubic) — Tenants / Regions empty-string sanitization
// ============================================================================
//
// The P2 regression lives at the binding boundary (Node /
// Python / C ABI), not in the Rust core: `matches_scope` takes
// a borrowed `&[&str]` and has no JSON-input shape to sanitize.
// The fix drops empty entries inside the binding-side
// `scope_filter_from_*` converters before constructing the
// owned filter.
//
// Regression coverage lives in the language test suites:
//   - TypeScript: `sdk-ts/test/capabilities.test.ts`
//   - Python:     `bindings/python/tests/test_capabilities.py`
//   - Go:         `bindings/go/net/capabilities_test.go`
//
// (Go transitively covers the C ABI since it consumes the same
// `net_mesh_find_nodes_scoped` symbol.)
