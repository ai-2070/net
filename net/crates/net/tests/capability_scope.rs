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

/// Build a MeshNode pinned to a specific subnet, with NO
/// `SubnetPolicy`. Used by the `SubnetLocal` tests to set up
/// same-subnet vs cross-subnet pairs.
///
/// The discovery-side scope filter reads `MeshNode.local_subnet` as the
/// caller's subnet and derives each candidate's from the tags on the
/// fold entry the query selected, by running the local policy over
/// them. With no policy there is nothing to resolve those tags with, so
/// every non-self candidate is unresolvable and `SameSubnet` excludes
/// it — which is what the without-policy tests below assert.
async fn build_node_in_subnet(subnet: SubnetId) -> Arc<MeshNode> {
    let keypair = EntityKeypair::generate();
    let cfg = test_config().with_subnet(subnet);
    Arc::new(MeshNode::new(keypair, cfg).await.expect("MeshNode::new"))
}

/// Build a MeshNode with a subnet AND a `SubnetPolicy`.
///
/// The policy is what lets scoped discovery resolve a candidate at all:
/// `find_nodes_by_filter_scoped` runs it over the tags of the fold entry
/// the query selected. It also gates the `peer_subnets` write in
/// `handle_capability_announcement`, but that map is no longer read by
/// the discovery path — only by the channel publish / subscribe paths.
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
    // `scope:subnet-local` are visible only to peers in the same
    // subnet. Exercises the same-subnet predicate the bridge invokes
    // under `ScopeFilter::SameSubnet` — which resolves the candidate
    // from the selected fold entry's tags, not from `peer_subnets`.
    // These nodes carry no policy, so no candidate resolves and only
    // the local node survives the filter.
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

    // No `local_subnet_policy` is installed on D, so there is nothing
    // to resolve a candidate's tags with and every non-self candidate
    // stays unknown. Treating "unknown" as "same subnet" in that
    // configuration would silently leak every peer through
    // `SameSubnet` (Cubic P1). The rule: without a policy, unknown
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
// Regression: `SameSubnet` unresolved-peer leaks
// ============================================================================
//
// Two rounds of the same defect.
//
// P1 (Cubic): `find_nodes_by_filter_scoped(SameSubnet)` returned
// `true` for unknown peer subnets unconditionally. Without a
// `local_subnet_policy`, `peer_subnets` stays empty, so every peer
// registered as "unknown" and the closure leaked all of them. That fix
// made the permissive branch conditional on a policy being installed.
//
// The security audit found the branch still open in the configuration
// it was narrowed to: `peer_subnets` is only written for
// `signature_verified && hop_count == 0`, so a peer learned through a
// relay is unresolvable forever, and the policy-installed warm-up
// admitted it. Multi-hop is how peers are normally learned, so the
// strictest scope leaked by default.
//
// The subnet is now derived from the origin's own tags on the indexed
// announcement using the local policy, which resolves forwarded peers
// to their real subnet instead of guessing. The tests below pin both
// directions — a cross-subnet forwarded peer is excluded, a
// same-subnet one is admitted — so neither "admit unresolved" nor
// "exclude unresolved" can pass the pair.

#[tokio::test]
async fn same_subnet_without_policy_excludes_unresolved_peers() {
    // No policy installed → no candidate's subnet can be resolved. A
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
async fn same_subnet_resolves_forwarded_peers_from_the_fold() {
    // Multi-hop subnet resolution. `peer_subnets` is written only for
    // `signature_verified && hop_count == 0` — correctly, since on a
    // forwarded announcement `from_node` is the relay, not the origin.
    // The old code compensated with a warm-up branch that admitted
    // EVERY unresolved candidate whenever a policy was installed. That
    // window never closed for forwarded-only peers, so `SameSubnet`
    // returned peers from arbitrary subnets.
    //
    // The subnet is now derived from the origin's own tags on the
    // indexed announcement, using the local policy, so a forwarded peer
    // resolves to its real subnet.
    //
    // Topology: A — B — D, no direct session between A and D, so A
    // reaches D only through B's forward and is never in D's
    // `peer_subnets`.
    //
    // A (`region:eu`) is really in [4]; D is in [3]. A must be
    // EXCLUDED. The companion test covers the same-subnet direction, so
    // that "exclude everything unresolved" cannot pass both.
    let policy = region_policy();
    let a = build_node_with_policy(SubnetId::new(&[4]), policy.clone()).await; // region:eu
    let b = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // relay
    let d = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // observer

    handshake(&a, &b).await;
    handshake(&b, &d).await;

    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("region:eu") // policy → subnet [4]
            .add_tag("multihop-canary"),
    )
    .await
    .expect("A announce");

    let filter = CapabilityFilter::new().require_tag("multihop-canary");
    let a_id = a.node_id();

    // The forwarded announcement must land at D first — the scope
    // filter is a query concern, propagation is unchanged.
    let arrived = wait_until(&d, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(
        arrived,
        "forwarded announcement did not land at D — multi-hop \
         forwarding regressed?"
    );

    let same = d.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet);
    assert!(
        !same.contains(&a_id),
        "A is really in subnet [4], D is in [3] — a forwarded-only peer \
         must NOT be admitted under SameSubnet just because its subnet \
         wasn't in peer_subnets; got {:?}",
        same
    );

    // A directly-connected peer is not treated differently. B handshook
    // with D, so it DOES have a `peer_subnets` entry — but that map is
    // no longer consulted by scoped discovery, so B resolves from its
    // own announced `region:us` tag through the fold, by the same code
    // path as the forwarded peer above. That equivalence is the point;
    // `resolution_does_not_depend_on_whether_a_sidecar_entry_exists`
    // pins it directly.
    b.announce_capabilities(
        CapabilitySet::new()
            .add_tag("region:us") // policy → subnet [3], same as D
            .add_tag("multihop-canary"),
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
        "B is in [3] like D and announced `region:us`, so it must appear \
         under SameSubnet — resolved from its own tags via the fold, the \
         same way the forwarded peer is"
    );
}

/// The other direction of the same fix: a forwarded-only peer that IS in
/// the caller's subnet must still be returned. Without this, "exclude
/// every unresolved candidate" would pass the exclusion test while
/// silently making `SameSubnet` useless across more than one hop.
#[tokio::test]
async fn same_subnet_admits_forwarded_peer_in_the_same_subnet() {
    let policy = region_policy();
    // A is in [3] — the same subnet as the observer — but reaches it
    // only through the relay, so it is absent from `peer_subnets`.
    let a = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // region:us
    let b = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // relay
    let d = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await; // observer

    handshake(&a, &b).await;
    handshake(&b, &d).await;

    a.announce_capabilities(
        CapabilitySet::new()
            .add_tag("region:us") // policy → subnet [3], same as D
            .add_tag("multihop-sibling"),
    )
    .await
    .expect("A announce");

    let filter = CapabilityFilter::new().require_tag("multihop-sibling");
    let a_id = a.node_id();

    let arrived = wait_until(&d, |n| n.find_nodes_by_filter(&filter).contains(&a_id)).await;
    assert!(arrived, "forwarded announcement did not land at D");

    let admitted = wait_until(&d, |n| {
        n.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet)
            .contains(&a_id)
    })
    .await;
    assert!(
        admitted,
        "A is really in subnet [3] like D and reached it only via a \
         relay — it must resolve from its own announced tags rather \
         than being dropped as unresolvable"
    );
}

/// A `scope:subnet-local` provider learned only through a relay must not
/// be visible to a cross-subnet caller. This is the leak the warm-up
/// branch produced on the strictest scope — the one an operator reaches
/// for precisely when they want isolation.
#[tokio::test]
async fn subnet_local_provider_is_not_visible_cross_subnet_via_relay() {
    let policy = region_policy();
    let provider = build_node_with_policy(SubnetId::new(&[4]), policy.clone()).await; // region:eu
    let relay = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await;
    let observer = build_node_with_policy(SubnetId::new(&[3]), policy.clone()).await;

    handshake(&provider, &relay).await;
    handshake(&relay, &observer).await;

    provider
        .announce_capabilities(
            CapabilitySet::new()
                .add_tag("region:eu") // policy → subnet [4]
                .add_tag("software:photoshop")
                .with_subnet_local_scope(),
        )
        .await
        .expect("provider announce");

    let filter = CapabilityFilter::new().require_tag("software:photoshop");
    let provider_id = provider.node_id();

    let arrived = wait_until(&observer, |n| {
        n.find_nodes_by_filter(&filter).contains(&provider_id)
    })
    .await;
    assert!(arrived, "forwarded announcement did not reach the observer");

    // The observer is in [3], the provider in [4]. SubnetLocal requires
    // same-subnet, and the observer can now derive the provider's real
    // subnet from the forwarded announcement's own tags.
    let same = observer.find_nodes_by_filter_scoped(&filter, &ScopeFilter::SameSubnet);
    assert!(
        !same.contains(&provider_id),
        "a scope:subnet-local provider in another subnet must not be \
         visible to a cross-subnet observer that learned it via a relay; \
         got {:?}",
        same
    );

    // And it stays excluded from the permissive filters, as before.
    let any = observer.find_nodes_by_filter_scoped(&filter, &ScopeFilter::Any);
    assert!(
        !any.contains(&provider_id),
        "SubnetLocal must stay excluded from Any; got {:?}",
        any
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
