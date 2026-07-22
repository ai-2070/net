//! OLB-0 §4.3: the node-global sensing-interest lease wiring
//! (`MeshNode::acquire_sensing_interest_lease` /
//! `release_sensing_interest_lease` / `deregister_sensing_interest`).
//!
//! The pure refcount + cadence logic is unit-tested in
//! `behavior::sensing::lease`; these tests prove the NODE wiring drives a
//! real interest-table registration: equivalent acquisitions over one
//! node share exactly one registration, and only the last release tears
//! it down. A single node with a Node(remote) interest exercises the
//! exact-provider leg without a second node or the origin emitter
//! (self-provider) path.
//!
//! Run: `cargo test --features net --test sensing_lease`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode, WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const PSK: [u8; 32] = [0x42u8; 32];
const TTL: Duration = Duration::from_secs(30);
const D: Duration = Duration::from_millis(100);
const STRICT: Duration = Duration::from_millis(50);
const PROVIDER: u64 = 999;

fn spec_for(owner: AudienceScopeCommitment) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("gpu.infer"),
        constraints: CanonicalConstraints::from_entries([("model", "llama-70b")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(PROVIDER),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

/// A single sensing-enabled node. Not started: the heartbeat sweep (the
/// only other row-expiry path) never runs, so any row that disappears in
/// a test was deregistered by the lease, not swept.
async fn sensing_node() -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK).with_sensing_coalescing(true);
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    )
}

/// Two equivalent acquisitions share ONE registration; the first release
/// keeps it live, the last release deregisters.
#[tokio::test]
async fn equivalent_acquires_share_one_registration_and_last_release_deregisters() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root());
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    assert!(node.sensing_table_is_empty(), "starts empty");

    let t1 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D, TTL)
        .expect("first acquire registers");
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "first acquire installs the Local row"
    );

    let t2 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D, TTL)
        .expect("second acquire shares the lease");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::Local],
        "the equivalent second acquire is still one registration"
    );

    node.release_sensing_interest_lease(t1, &spec, PROVIDER, TTL);
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "a surviving holder keeps the registration live"
    );

    node.release_sensing_interest_lease(t2, &spec, PROVIDER, TTL);
    assert!(
        node.sensing_downstreams(&key).is_empty(),
        "the last release deregisters the interest"
    );
    assert!(node.sensing_table_is_empty(), "no rows remain");
}

/// A stricter interval joining an existing lease keeps one registration,
/// and the whole thing still tears down cleanly on the final release.
#[tokio::test]
async fn a_stricter_acquire_reregisters_without_forking_the_registration() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root());
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let loose = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D, TTL)
        .expect("loose acquire");
    let strict = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT, TTL)
        .expect("strict acquire re-registers tighter");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::Local],
        "tightening the cadence is still one registration"
    );

    node.release_sensing_interest_lease(strict, &spec, PROVIDER, TTL);
    node.release_sensing_interest_lease(loose, &spec, PROVIDER, TTL);
    assert!(
        node.sensing_table_is_empty(),
        "both holders gone — interest deregistered"
    );
}

/// Releasing a ticket twice is a harmless no-op (the SDK guard's drop
/// must be idempotent against an explicit close).
#[tokio::test]
async fn double_release_is_idempotent() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root());
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let ticket = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D, TTL)
        .expect("acquire");
    node.release_sensing_interest_lease(ticket, &spec, PROVIDER, TTL);
    assert!(node.sensing_downstreams(&key).is_empty());
    // Second release of the same ticket does nothing and must not panic.
    node.release_sensing_interest_lease(ticket, &spec, PROVIDER, TTL);
    assert!(node.sensing_table_is_empty());
}

/// With the plane disabled, an explicit deregister is a no-op rather than
/// an error or panic.
#[tokio::test]
async fn deregister_is_a_noop_when_the_plane_is_disabled() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = MeshNodeConfig::new(addr, PSK); // sensing coalescing left OFF
    let node = Arc::new(
        MeshNode::new(EntityKeypair::generate(), cfg)
            .await
            .expect("MeshNode::new"),
    );
    let spec = spec_for(node.sensing_local_root());
    node.deregister_sensing_interest(&spec, PROVIDER);
    assert!(node.sensing_table_is_empty());
}
