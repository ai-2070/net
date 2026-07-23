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
use std::time::{Duration, Instant};

use net::adapter::net::behavior::sensing::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, DownstreamId,
    Incarnation, InterestSpec, ProviderInterestKey, ProviderSelector, ResultMode, SensingLeaseKey,
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

/// L1 follow-up (review NARROW HOLD): the direct `Local` row and the lease's
/// `LeasedLocal` row are distinct table slots but feed ONE shared consumer cell,
/// whose cadence must be the DERIVED aggregate (min across both) at every
/// mutation. Coexistence + relax-back + removal, asserted via the cell-interval
/// seam. RED-coupled: re-anchoring the cell to the registering/deregistering
/// row's own interval (last-writer) instead of the aggregate fails the 50 ms
/// coexistence and the 100 ms relax-back assertions.
#[tokio::test]
async fn local_and_leased_share_one_aggregate_consumer_cadence() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    // Direct Local at 100 ms, then a lease at 50 ms.
    node.register_sensing_interest(&spec, PROVIDER, D, Duration::from_secs(30))
        .expect("direct registration");
    let lease = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("lease acquire");

    // Both ownership rows exist as distinct slots.
    let rows = node.sensing_downstreams(&key);
    assert!(
        rows.contains(&DownstreamId::Local) && rows.contains(&DownstreamId::LeasedLocal),
        "both the direct and leased local rows exist: {rows:?}"
    );
    // The shared consumer cell carries the aggregate: min(100, 50) = 50 ms.
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(STRICT),
        "the shared consumer cadence is the derived aggregate (50 ms)"
    );

    // Releasing the lease leaves the direct row and relaxes the shared cell to
    // 100 ms — and, because the direct row survives, sends no upstream Deregister.
    node.release_sensing_interest_lease(lease);
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::Local],
        "only the direct row survives — the branch is not deregistered upstream"
    );
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(D),
        "the shared consumer cadence relaxes to the surviving direct row's 100 ms"
    );

    // Deregistering the direct row removes the local consumer cell entirely.
    node.deregister_sensing_interest(&spec, PROVIDER);
    assert!(node.sensing_table_is_empty(), "no rows remain");
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        None,
        "no live local row remains → the consumer cell is removed (no ghost)"
    );
}

/// L1 follow-up: reversing the registration order proves the shared cadence is
/// aggregate-derived, not last-writer-selected.
#[tokio::test]
async fn shared_consumer_cadence_is_order_independent() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    // Lease at 50 ms FIRST, then a looser direct watch at 100 ms.
    let lease = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("lease acquire");
    node.register_sensing_interest(&spec, PROVIDER, D, Duration::from_secs(30))
        .expect("direct registration");

    // The shared cadence stays at the strictest (50 ms) — the later, looser
    // registration did not overwrite it.
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(STRICT),
        "the shared cadence is min(50, 100), not the last-registered 100 ms"
    );

    node.release_sensing_interest_lease(lease);
    node.deregister_sensing_interest(&spec, PROVIDER);
    assert!(node.sensing_table_is_empty());
}

/// L1 narrow-hold Finding 1: a LEASE-ONLY consumer cell (no direct `Local` row)
/// SURVIVES the periodic materialized-branch sweep and keeps its lease cadence —
/// the normal org-routing shape. RED-coupled: the prior `Local`-only liveness
/// check in the sweep deleted this cell.
#[tokio::test]
async fn lease_only_consumer_cell_survives_the_sweep() {
    let node = sensing_node().await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let _lease = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("lease acquire");
    assert_eq!(
        node.sensing_downstreams(&key),
        vec![DownstreamId::LeasedLocal],
        "only the leased row exists — no direct Local row"
    );
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(STRICT)
    );

    // Drive one production materialized-branch sweep.
    node.run_sensing_consumer_cell_sweep_for_test(Instant::now());

    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(STRICT),
        "the lease-only consumer cell survives the sweep at the lease cadence"
    );
}

/// L1 narrow-hold: when a stricter `LeasedLocal` row EXPIRES while a looser
/// direct `Local` row survives, the periodic sweep relaxes the shared consumer
/// cell to the survivor's cadence — `local_consumer_interval` excludes the
/// expired row — never leaving it at the stale strict cadence. Driven with a
/// synthetic sweep instant to avoid racing real time.
#[tokio::test]
async fn expired_lease_row_relaxes_consumer_cell_to_surviving_direct() {
    let ttl = Duration::from_millis(200);
    let cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK)
        .with_sensing_coalescing(true)
        .with_sensing_interest_ttl(ttl);
    let node = node_with(cfg).await;
    let spec = spec_for(node.sensing_local_root(), PROVIDER);
    let key = ProviderInterestKey::new(spec.key(), PROVIDER);

    let t = Instant::now();
    // Lease (50 ms) acquired FIRST → LeasedLocal expires ≈ t + ttl.
    let _lease = node
        .acquire_sensing_interest_lease(&spec, PROVIDER, STRICT)
        .expect("lease acquire");
    // A real gap, then the direct row (100 ms) → Local expires ≈ t + gap + ttl,
    // OUTLIVING the lease row.
    tokio::time::sleep(Duration::from_millis(30)).await;
    node.register_sensing_interest(&spec, PROVIDER, D, ttl)
        .expect("direct registration");
    // Both live now → the cell carries the aggregate (50 ms).
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(STRICT)
    );

    // Sweep at a synthetic `now` AFTER the lease expiry (≈ t+200 ms) but BEFORE
    // the direct's (≈ t+230 ms): the lease row is excluded from the projection,
    // so the cell relaxes to the direct row's 100 ms.
    node.run_sensing_consumer_cell_sweep_for_test(t + ttl + Duration::from_millis(15));
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(D),
        "the surviving direct row relaxes the consumer cell to 100 ms after the lease row expires"
    );
}

/// L1 narrow-hold: a REFUSAL PARTITION that removes the stricter local row while
/// a looser one survives re-anchors the shared consumer cell to the survivor —
/// the origin-refusal path is reconciled like every other local-row-removing
/// mutation. A self-provider branch, an origin cadence floor of 75 ms: the direct
/// 100 ms row is above it, the 50 ms lease is below → the partition removes the
/// lease row, keeps the direct row, and the cell re-anchors to 100 ms.
#[tokio::test]
async fn refusal_partition_reanchors_consumer_cell_to_surviving_direct() {
    let cfg = MeshNodeConfig::new("127.0.0.1:0".parse().unwrap(), PSK)
        .with_sensing_coalescing(true)
        .with_sensing_incarnation(Incarnation::new(1))
        .with_attestation_cadence_floor(Duration::from_millis(75))
        .with_max_interests_per_peer(1024);
    let node = node_with(cfg).await;
    let self_id = node.node_id(); // self-provider → the origin emitter gates cadence
    let spec = spec_for(node.sensing_local_root(), self_id);
    let key = ProviderInterestKey::new(spec.key(), self_id);

    // Direct Local at 100 ms ≥ the 75 ms floor → accepted, cell at 100 ms.
    node.register_sensing_interest(&spec, self_id, D, Duration::from_secs(30))
        .expect("direct 100ms is above the cadence floor");
    assert_eq!(node.sensing_consumer_cell_interval_for_test(&key), Some(D));

    // A lease at 50 ms drops the aggregate below the floor; the origin refusal
    // partitions the branch — removing the 50 ms lease row, keeping the 100 ms
    // direct row.
    let err = node
        .acquire_sensing_interest_lease(&spec, self_id, STRICT)
        .expect_err("50ms is below the cadence floor");
    assert!(
        matches!(err, SensingRegistrationError::RefusedByFloor { .. }),
        "got {err:?}"
    );

    assert!(
        node.sensing_downstreams(&key)
            .contains(&DownstreamId::Local),
        "the direct row survives the refusal partition"
    );
    assert_eq!(
        node.sensing_consumer_cell_interval_for_test(&key),
        Some(D),
        "the refusal partition removed the lease row; the cell re-anchored to the surviving direct 100 ms"
    );
}
