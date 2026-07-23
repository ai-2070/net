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
            .contains(&DownstreamId::LeasedLocal),
        "first acquire installs the lease-owned LeasedLocal row"
    );

    let t2 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("second acquire shares the lease");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::LeasedLocal],
        "the equivalent second acquire is still one registration"
    );

    node.release_sensing_interest_lease(t1);
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::LeasedLocal),
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
        vec![DownstreamId::LeasedLocal],
        "tightening the cadence is still one registration"
    );
    // §8: assert the tighten actually moved the installed cadence — a silently
    // swallowed Reregister would leave the row at the loose D and pass the
    // count-only check above.
    assert_eq!(
        node.sensing_downstream_entry(&key, DownstreamId::LeasedLocal)
            .expect("row present")
            .requested_sample_interval,
        STRICT,
        "the tightened 50 ms cadence is actually installed on the row"
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
            .contains(&DownstreamId::LeasedLocal),
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
        .contains(&DownstreamId::LeasedLocal));
}

/// A registration refused against a cached provider floor (the distinct
/// `RegisterOutcome::RefusedByCachedFloor` branch, not `OverCap`) fails the
/// acquisition, rolls the holder back leaving no lease entry, and — once the
/// floor relaxes — the same key registers cleanly.
#[tokio::test]
async fn below_floor_acquire_is_refused_rolled_back_and_recovers() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);
    let lease_key = SensingLeaseKey::ExactProvider {
        audience: spec.audience,
        interest_digest: spec.interest_digest(),
        provider: PROVIDER,
    };
    let floor = Duration::from_millis(150);

    // A cached provider floor is in place (as a live refusal would leave it).
    node.install_sensing_cached_floor_for_test(&spec, PROVIDER, floor);

    // Acquiring below the floor is refused; the acquisition rolls back and
    // leaves no lease entry — a refused registration never counts as
    // installed.
    let err = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect_err("below the cached floor");
    assert!(
        matches!(
            err,
            SensingRegistrationError::RefusedByFloor { minimum_supported } if minimum_supported == floor
        ),
        "got {err:?}"
    );
    assert!(
        node.sensing_lease_entry_for_test(&lease_key).is_none(),
        "the refused acquire left no lease entry"
    );

    // Relax the floor; the same acquire now installs a real registration.
    node.clear_sensing_cached_floor_for_test(&spec, PROVIDER);
    let _t = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("recovers once the floor relaxes");
    assert!(node
        .sensing_downstreams(&key)
        .contains(&DownstreamId::LeasedLocal));
    assert!(node.sensing_lease_entry_for_test(&lease_key).is_some());
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

/// L1 (review §1): a DIRECT `register_sensing_interest` row is untouched by a
/// refused lease acquire's rollback. The lease owns a distinct `LeasedLocal`
/// slot, so a first-holder acquire refused by a cached floor rolls back without
/// ever removing the direct `Local` row for the same key. RED-coupled: with the
/// lease sharing the `Local` identity, this rollback tore the direct row down.
#[tokio::test]
async fn lease_rollback_leaves_a_direct_local_row_intact() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    // A direct application watch installs the node-local `Local` row at 100 ms.
    node.register_sensing_interest(&spec, PROVIDER, D, Duration::from_secs(30))
        .expect("direct registration installs the Local row");
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "the direct Local row is present"
    );

    // A provider floor of 50 ms is later cached for the key.
    node.install_sensing_cached_floor_for_test(&spec, PROVIDER, STRICT);

    // A lease acquire BELOW that floor is refused; its first-holder rollback
    // must not remove the direct row.
    let below_floor = Duration::from_millis(10);
    let err = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, below_floor)
        .expect_err("below the cached floor");
    assert!(
        matches!(err, SensingRegistrationError::RefusedByFloor { .. }),
        "got {err:?}"
    );

    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "the direct Local row SURVIVES the refused lease acquire's rollback"
    );
    assert_eq!(
        node.sensing_downstream_entry(&key, DownstreamId::Local)
            .expect("direct row still present")
            .requested_sample_interval,
        D,
        "the direct row keeps its own 100 ms cadence, untouched by the lease"
    );
    // The refused acquire also wedged nothing in the lease registry.
    let lease_key = SensingLeaseKey::ExactProvider {
        audience: spec.audience,
        interest_digest: spec.interest_digest(),
        provider: PROVIDER,
    };
    assert!(node.sensing_lease_entry_for_test(&lease_key).is_none());
}

/// L2 (review §3): a STALE ticket release cannot remove a live SUCCESSOR holder
/// of the same key. acquire t1 → release t1 → acquire t2 (same key) → stale
/// release t1: t2's row and lease entry survive, no wire Deregister; the final
/// release t2 cleans up. RED-coupled against any token scheme that recycles or
/// per-entry-scopes tokens instead of the node-global monotonic mint.
#[tokio::test]
async fn stale_ticket_release_cannot_remove_a_live_successor() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);
    let lease_key = SensingLeaseKey::ExactProvider {
        audience: spec.audience,
        interest_digest: spec.interest_digest(),
        provider: PROVIDER,
    };

    let t1 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("acquire t1");
    node.release_sensing_interest_lease(t1);
    assert!(node.sensing_table_is_empty(), "t1 released — no rows");

    // A NEW holder for the same key mints a fresh, monotonic token.
    let t2 = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("acquire t2 (successor)");
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::LeasedLocal),
        "t2 installed the successor row"
    );

    // The STALE t1 release must be a pure no-op — it cannot tear down t2.
    node.release_sensing_interest_lease(t1);
    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::LeasedLocal),
        "the stale t1 release does NOT remove the live successor row"
    );
    assert!(
        node.sensing_lease_entry_for_test(&lease_key).is_some(),
        "t2's lease entry survives the stale release"
    );

    // The successor's OWN release is what finally deregisters.
    node.release_sensing_interest_lease(t2);
    assert!(
        node.sensing_table_is_empty(),
        "t2 release deregisters — no rows remain"
    );
    assert!(node.sensing_lease_entry_for_test(&lease_key).is_none());
}

/// L3 (review §8): a non-first-holder tighten refused by a cached floor rolls
/// back by RELAXING the installed cadence to the surviving holder's minimum, not
/// by deregistering. Witnesses the relax-back arithmetic (lease.rs:222-228) at
/// the node level — existing tests only cover first-holder failures.
#[tokio::test]
async fn refused_non_first_holder_tighten_relaxes_back_to_survivor() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    // A loose holder installs the row at 100 ms.
    let loose = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, D)
        .expect("loose acquire");
    // A cached floor of 60 ms: the 100 ms row stands, but a 50 ms tighten fails.
    node.install_sensing_cached_floor_for_test(&spec, PROVIDER, Duration::from_millis(60));

    // A second holder's tighten to 50 ms is refused; the rollback relaxes the
    // installed cadence back to the survivor's 100 ms rather than deregistering.
    let err = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect_err("tighten below floor");
    assert!(
        matches!(err, SensingRegistrationError::RefusedByFloor { .. }),
        "got {err:?}"
    );
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::LeasedLocal],
        "the survivor's row remains — the refused tighten did not deregister it"
    );
    assert_eq!(
        node.sensing_downstream_entry(&key, DownstreamId::LeasedLocal)
            .expect("survivor row present")
            .requested_sample_interval,
        D,
        "the installed cadence relaxed back to the survivor's 100 ms"
    );

    // The loose holder still cleanly tears down.
    node.release_sensing_interest_lease(loose);
    assert!(node.sensing_table_is_empty(), "final release deregisters");
}
