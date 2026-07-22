//! OLB-0 §4.3: the node-global sensing-interest lease wiring
//! (`MeshNode::acquire_sensing_interest_lease` /
//! `release_sensing_interest_lease` / `deregister_sensing_interest`).
//!
//! The pure refcount + cadence logic is unit-tested in
//! `behavior::sensing::lease`. These are NODE-level registry/refcount
//! witnesses over a single node: they prove the wiring drives a real
//! interest-table registration and that refused registrations do not wedge
//! the lease. They are NOT wire witnesses — the external delivery/ordering
//! proofs (two-node upstream registration, cadence tighten/relax, upstream
//! deregistration) live in `sensing_lease_wire.rs`.
//!
//! Run: `cargo test --features net --test sensing_lease`

#![cfg(feature = "net")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode, SensingLeaseKey,
    WorkLatencyEnvelope,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig, SensingRegistrationError};

const PSK: [u8; 32] = [0x42u8; 32];
const D: Duration = Duration::from_millis(100);
const STRICT: Duration = Duration::from_millis(50);
const PROVIDER: u64 = 999;
const OTHER_PROVIDER: u64 = 998;

fn spec_for(owner: AudienceScopeCommitment, provider: u64) -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("gpu.infer"),
        constraints: CanonicalConstraints::from_entries([("model", "llama-70b")]).unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(2)),
        providers: ProviderSelector::Node(provider),
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: owner,
    }
}

/// A single sensing-enabled node. Not started: the heartbeat sweep (the
/// only other row-expiry path) never runs, so any row that disappears in
/// a test was deregistered by the lease, not swept.
async fn sensing_node() -> Arc<MeshNode> {
    node_with(
        MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK).with_sensing_coalescing(true),
    )
    .await
}

async fn node_with(cfg: MeshNodeConfig) -> Arc<MeshNode> {
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
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    assert!(node.sensing_table_is_empty(), "starts empty");

    let t1 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("first acquire registers");
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "first acquire installs the Local row"
    );

    let t2 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("second acquire shares the lease");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::Local],
        "the equivalent second acquire is still one registration"
    );

    node.release_sensing_interest_lease(t1);
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "a surviving holder keeps the registration live"
    );

    node.release_sensing_interest_lease(t2);
    assert!(
        node.sensing_downstreams(&key).is_empty(),
        "the last release deregisters the interest"
    );
    assert!(node.sensing_table_is_empty(), "no rows remain");
}

/// A stricter interval joining an existing lease keeps one registration
/// locally, and the whole thing tears down cleanly on the final release.
/// (The remote cadence effect is witnessed in `sensing_lease_wire.rs`.)
#[tokio::test]
async fn a_stricter_acquire_keeps_one_local_registration() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let loose = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("loose acquire");
    let strict = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("strict acquire re-registers tighter");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::Local],
        "tightening the cadence is still one registration"
    );

    node.release_sensing_interest_lease(strict);
    node.release_sensing_interest_lease(loose);
    assert!(
        node.sensing_table_is_empty(),
        "both holders gone — interest deregistered"
    );
}

/// Release is ticket-owned: the ticket alone tears down the exact interest
/// it named. Releasing one ticket never strands another key's registration.
/// (The A-ticket/B-arguments corruption is now impossible by construction —
/// release takes no spec/provider — so this checks the residual behavior.)
#[tokio::test]
async fn releasing_one_ticket_leaves_another_key_untouched() {
    let node = sensing_node().await;
    let spec_a = spec_for(node.sensing_local_root(), PROVIDER);
    let spec_b = spec_for(node.sensing_local_root(), OTHER_PROVIDER);
    let key_a = ProviderInterestKey::new(spec_a.key(), PROVIDER);
    let key_b = ProviderInterestKey::new(spec_b.key(), OTHER_PROVIDER);

    let ta = node
        .acquire_sensing_interest_lease(&spec_a, PROVIDER, D)
        .expect("acquire A");
    let _tb = node
        .acquire_sensing_interest_lease(&spec_b, OTHER_PROVIDER, D)
        .expect("acquire B");

    node.release_sensing_interest_lease(ta);
    assert!(node.sensing_downstreams(&key_a).is_empty(), "A torn down");
    assert!(
        node.sensing_downstreams(&key_b)
            .contains(&DownstreamId::Local),
        "B's registration is untouched by A's release"
    );
}

/// A refused registration (interest table over `max_interests_per_peer`)
/// fails the acquisition, rolls the holder back, and does NOT wedge the
/// lease as "installed": once capacity frees, the next acquire registers.
#[tokio::test]
async fn overcap_rolls_back_and_does_not_wedge_the_lease() {
    let cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK)
        .with_sensing_coalescing(true)
        .with_max_interests_per_peer(1);
    let node = node_with(cfg).await;

    let spec_a = spec_for(node.sensing_local_root(), PROVIDER);
    let spec_b = spec_for(node.sensing_local_root(), OTHER_PROVIDER);
    let key_b = SensingLeaseKey::ExactProvider {
        audience: spec_b.audience,
        interest_digest: spec_b.interest_digest(),
        provider: OTHER_PROVIDER,
    };

    // Fill the one Local slot.
    let ta = node
        .acquire_sensing_interest_lease(&spec_a, PROVIDER, D)
        .expect("first acquire fits");

    // The second distinct interest overflows the table — the acquire fails
    // and nothing is left wedged in the lease registry.
    let err = node
        .acquire_sensing_interest_lease(&spec_b, OTHER_PROVIDER, D)
        .expect_err("over capacity");
    assert!(
        matches!(err, SensingRegistrationError::OverCapacity),
        "got {err:?}"
    );
    assert!(
        node.sensing_lease_entry_for_test(&key_b).is_none(),
        "the rolled-back acquire left no lease entry"
    );

    // Free capacity, then the same interest registers cleanly (recovery).
    node.release_sensing_interest_lease(ta);
    let _tb = node
        .acquire_sensing_interest_lease(&spec_b, OTHER_PROVIDER, D)
        .expect("recovers once capacity frees");
    let key_b_row = ProviderInterestKey::new(spec_b.key(), OTHER_PROVIDER);
    assert!(node
        .sensing_downstreams(&key_b_row)
        .contains(&DownstreamId::Local));
}

/// Releasing a ticket twice is a harmless no-op (the SDK guard's drop
/// must be idempotent against an explicit close).
#[tokio::test]
async fn double_release_is_idempotent() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let ticket = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("acquire");
    node.release_sensing_interest_lease(ticket);
    assert!(node.sensing_downstreams(&key).is_empty());
    // Second release of the same ticket does nothing and must not panic.
    node.release_sensing_interest_lease(ticket);
    assert!(node.sensing_table_is_empty());
}

/// With the plane disabled, an explicit deregister is a no-op rather than
/// an error or panic.
#[tokio::test]
async fn deregister_is_a_noop_when_the_plane_is_disabled() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let node = node_with(MeshNodeConfig::new(addr, PSK)).await; // sensing OFF
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    node.deregister_sensing_interest(&spec, PROVIDER);
    assert!(node.sensing_table_is_empty());
}
