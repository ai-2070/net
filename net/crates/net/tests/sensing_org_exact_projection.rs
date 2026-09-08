//! OA-4: the request-relative sensed projection over retained organization
//! exact-provider demand, and the deterministic order it implies.
//!
//! Every witness here drives the REAL retained demand — `OrgSensingFamily`
//! acquires actual node-global leases, which anchor actual consumer cells — and
//! reads the projection through its only entry point,
//! `OrgSensingCapabilityDemand::project_sensed_order`. Nothing is hand-built:
//! the branch keys come from the acquisition itself, and readiness is admitted
//! through the ordinary observation cell.
//!
//! What these witnesses hold to account:
//!
//! * ONE captured instant decides freshness, at the exact boundary
//!   (`deadline - 1ns` still projects, `deadline` does not), with no worker tick
//!   and no mutation required to turn expired evidence into `Unknown`;
//! * exactly one row per authorized population member, in population order,
//!   with missing / removed / unretained / expired evidence all `Unknown`;
//! * `Unknown` never prunes, and an over-budget `Ready` stays potential rather
//!   than becoming non-viable; only a FRESH explicit `NotReady` is non-viable,
//!   and only for its own exact interest;
//! * the proximity, budget and ordering work runs with every sensing guard
//!   released — proved positively, from inside those phases;
//! * the order is a stable class permutation of the COMPLETE candidate list,
//!   including lists longer than the sensing population cap, and it never
//!   invents, duplicates or drops a candidate;
//! * the result is ADVISORY plain data: it authorizes nothing, and churn after
//!   the capture cannot make it authorize anything.
//!
//! Run: `cargo nextest run --features "cortex tool fixtures" --test
//! sensing_org_exact_projection`
#![cfg(all(feature = "net", feature = "fixtures"))]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use net::adapter::net::behavior::org::{OrgKeypair, OrgMembershipCert};
use net::adapter::net::behavior::org_authority::NodeAuthority;
use net::adapter::net::behavior::org_sensing_demand::{
    org_sensed_bucket_permutation, OrgSensedProjection, OrgSensingCapabilityDemand,
    OrgSensingFamily,
};
use net::adapter::net::behavior::sensing::{
    AttestedStatus, ConsumerLatencyBudget, DeliveredBeat, Incarnation, ProjectedReadiness,
    ProviderInterestKey,
};
use net::adapter::net::{EntityKeypair, MeshNode, MeshNodeConfig};

const TAG: &str = "nrpc:gpu.infer";

/// One off-lock observation: the phase, this thread's sensing-guard depth, and
/// the guard set it held.
type OffLockLog = Arc<parking_lot::Mutex<Vec<(&'static str, usize, String)>>>;
const TTL: Duration = Duration::from_secs(30);

/// A sensing-enabled, organization-authoritative node. No transport: every
/// witness here is about one node's own captured view.
async fn projection_node(tag: &str) -> Arc<MeshNode> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let node = Arc::new(
        MeshNode::new(
            EntityKeypair::generate(),
            MeshNodeConfig::new(addr, [0x31u8; 32])
                .with_sensing_coalescing(true)
                .with_sensing_interest_ttl(TTL),
        )
        .await
        .expect("MeshNode::new"),
    );
    let keys = OrgKeypair::from_bytes([0x42u8; 32]);
    let entity = node.entity_id().clone();
    let cert = OrgMembershipCert::try_issue(&keys, entity.clone(), 1, 3600).expect("issue cert");
    let dir = std::env::temp_dir().join(format!(
        "oa4-projection-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let authority =
        Arc::new(NodeAuthority::adopt(&dir, cert, &entity, 0, None).expect("adopt org authority"));
    node.install_node_authority(authority)
        .expect("install org authority");
    node
}

fn beat(status: AttestedStatus, start_ms: u64, seq: u64, bearing: bool) -> DeliveredBeat {
    DeliveredBeat {
        attested_status: status,
        estimated_start: Some(Duration::from_millis(start_ms)),
        source_incarnation: Incarnation::new(1),
        capability_generation: 1,
        seq,
        promised_cadence: Duration::from_secs(1),
        continuity_bearing: bearing,
    }
}

/// Admit one live beat for `branch` and return the deadline it armed.
fn admit(node: &Arc<MeshNode>, branch: &ProviderInterestKey, beat: DeliveredBeat) -> Instant {
    node.sensing_admit_beat_for_test(branch, beat, Instant::now())
        .expect("retained demand must have anchored a consumer cell for its own branch")
}

fn branches(demand: &Arc<OrgSensingCapabilityDemand>) -> Vec<(u64, ProviderInterestKey)> {
    let mut all = demand.retained_branches_for_test();
    all.sort_by_key(|(provider, _)| *provider);
    all
}

fn readiness_of(projection: &OrgSensedProjection, provider: u64) -> ProjectedReadiness {
    projection
        .rows()
        .iter()
        .find(|row| row.provider == provider)
        .map(|row| row.readiness)
        .unwrap_or_else(|| panic!("no row for provider {provider:#x}"))
}

/// The buckets must partition the rows exactly: every provider once, none
/// invented, and the classification of each row is the one the buckets claim.
fn assert_partitions_rows(projection: &OrgSensedProjection, budget_admits_ready: bool) {
    let mut seen: Vec<u64> = projection
        .viable()
        .iter()
        .chain(projection.potential())
        .chain(projection.non_viable())
        .copied()
        .collect();
    let total = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        total,
        seen.len(),
        "a provider appears in more than one bucket: {projection:?}"
    );
    let mut expected: Vec<u64> = projection.rows().iter().map(|row| row.provider).collect();
    expected.sort_unstable();
    assert_eq!(
        seen, expected,
        "the buckets must cover exactly the captured rows - nothing lost, nothing invented"
    );
    for row in projection.rows() {
        let bucket = if projection.viable().contains(&row.provider) {
            "viable"
        } else if projection.non_viable().contains(&row.provider) {
            "non_viable"
        } else {
            "potential"
        };
        let want = match row.readiness {
            ProjectedReadiness::Ready if budget_admits_ready => "viable",
            ProjectedReadiness::NotReady => "non_viable",
            _ => "potential",
        };
        assert_eq!(
            bucket, want,
            "row {row:?} was bucketed as {bucket}, not {want}: {projection:?}"
        );
    }
}

// ---- COHERENT CAPTURE ---------------------------------------------------

/// Rows, buckets and every count taken from them are folds of ONE capture, so
/// they agree even while readiness is being republished underneath.
///
/// The concurrent writer is the point: a projection that read the map twice, or
/// classified from a second read, could report a provider as viable and absent
/// from its own rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counts_ranking_and_rows_agree_under_concurrent_status_change() {
    let node = projection_node("coherent").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let (first, second) = (
        node.node_id().wrapping_add(1),
        node.node_id().wrapping_add(2),
    );
    let demand = family
        .reconcile(TAG, &[first, second])
        .expect("retain both providers");
    let keys = branches(&demand);
    assert_eq!(keys.len(), 2, "both providers must be retained");

    admit(&node, &keys[0].1, beat(AttestedStatus::Ready, 5, 1, true));
    admit(
        &node,
        &keys[1].1,
        beat(AttestedStatus::NotReady, 5, 1, true),
    );

    // Republish readiness for BOTH branches while the projections run.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let node = Arc::clone(&node);
        let keys = keys.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut seq = 2u64;
            while !stop.load(Ordering::Relaxed) {
                for (index, (_, branch)) in keys.iter().enumerate() {
                    let status = if (seq as usize + index).is_multiple_of(2) {
                        AttestedStatus::Ready
                    } else {
                        AttestedStatus::NotReady
                    };
                    let _ = node.sensing_admit_beat_for_test(
                        branch,
                        beat(status, 5, seq, true),
                        Instant::now(),
                    );
                }
                seq += 1;
            }
        })
    };

    let budget = ConsumerLatencyBudget::default();
    for _ in 0..400 {
        let projection = demand.project_sensed_order(Instant::now(), &budget);
        assert_eq!(
            projection.rows().len(),
            demand.population().len(),
            "exactly one row per authorized population member"
        );
        let order: Vec<u64> = projection.rows().iter().map(|row| row.provider).collect();
        assert_eq!(
            order,
            demand.population().to_vec(),
            "rows preserve the population's identity AND order"
        );
        assert_partitions_rows(&projection, true);
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer");

    drop(demand);
    drop(family);
}

/// Absence of evidence never prunes, and a `Ready` proof that does not fit THIS
/// request's budget is held back rather than declared non-viable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_never_prunes_and_an_over_budget_ready_stays_potential() {
    let node = projection_node("budget").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let (ready, silent) = (
        node.node_id().wrapping_add(1),
        node.node_id().wrapping_add(2),
    );
    let demand = family.reconcile(TAG, &[ready, silent]).expect("retain");
    let keys = branches(&demand);
    let ready_branch = keys
        .iter()
        .find(|(provider, _)| *provider == ready)
        .map(|(_, branch)| branch.clone())
        .expect("the ready provider is retained");
    // A live Ready proof whose own start estimate is far outside the budget
    // below. `silent` gets no beat at all.
    admit(
        &node,
        &ready_branch,
        beat(AttestedStatus::Ready, 5_000, 1, true),
    );

    let now = Instant::now();
    let generous = demand.project_sensed_order(now, &ConsumerLatencyBudget::default());
    assert_eq!(readiness_of(&generous, ready), ProjectedReadiness::Ready);
    assert_eq!(readiness_of(&generous, silent), ProjectedReadiness::Unknown);
    assert_eq!(
        generous.viable(),
        &[ready],
        "with no deadline nothing is demoted for budget"
    );
    assert_eq!(generous.potential(), &[silent], "and absence never prunes");
    assert!(generous.non_viable().is_empty());

    // The SAME evidence, at the SAME instant, under a request that cannot wait.
    let tight = demand.project_sensed_order(
        now,
        &ConsumerLatencyBudget {
            end_to_end_within: Some(Duration::from_millis(1)),
        },
    );
    assert_eq!(
        readiness_of(&tight, ready),
        ProjectedReadiness::Ready,
        "the readiness is request-independent; only its viability is not"
    );
    assert!(
        tight.viable().is_empty(),
        "an over-budget Ready is not viable for THIS request: {tight:?}"
    );
    assert!(
        tight.non_viable().is_empty(),
        "and it must NOT be pruned - a route change could make it viable: {tight:?}"
    );
    assert_eq!(
        tight.potential(),
        &[ready, silent],
        "both are held as potential, in id order: {tight:?}"
    );
    assert_eq!(tight.preferred(), None);
    assert_partitions_rows(&tight, false);

    drop(demand);
    drop(family);
}

/// Expiry is applied at the CAPTURED instant, at the exact boundary, with no
/// beat and no worker tick — and an expired `NotReady` can never prune.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_without_a_new_beat_maps_unknown_and_an_expired_not_ready_never_prunes() {
    let node = projection_node("expiry").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let provider = node.node_id().wrapping_add(1);
    let demand = family.reconcile(TAG, &[provider]).expect("retain");
    let keys = branches(&demand);
    let deadline = admit(
        &node,
        &keys[0].1,
        beat(AttestedStatus::NotReady, 5, 1, true),
    );
    let budget = ConsumerLatencyBudget::default();

    // One nanosecond INSIDE the window: the pessimistic evidence still stands.
    let fresh = demand.project_sensed_order(deadline - Duration::from_nanos(1), &budget);
    assert_eq!(
        readiness_of(&fresh, provider),
        ProjectedReadiness::NotReady,
        "a fresh explicit NotReady must still project"
    );
    assert_eq!(
        fresh.non_viable(),
        &[provider],
        "and only a fresh explicit NotReady is non-viable"
    );
    assert!(fresh.potential().is_empty());

    // AT the deadline: the same cell, the same beat, no mutation, no tick.
    let expired = demand.project_sensed_order(deadline, &budget);
    assert_eq!(
        readiness_of(&expired, provider),
        ProjectedReadiness::Unknown,
        "expired evidence is Unknown at the boundary itself, not one tick later"
    );
    assert!(
        expired.non_viable().is_empty(),
        "an expired NotReady must never prune: {expired:?}"
    );
    assert_eq!(expired.potential(), &[provider]);

    // Nothing was mutated to reach that verdict: the fresh view is still
    // available from the same cell.
    let again = demand.project_sensed_order(deadline - Duration::from_nanos(1), &budget);
    assert_eq!(
        readiness_of(&again, provider),
        ProjectedReadiness::NotReady,
        "the read path must not have expired the cell it evaluated"
    );

    drop(demand);
    drop(family);
}

/// A provider whose evidence was REMOVED before the capture resolves `Unknown`
/// from current state — no supersession stamp, no detection, no stale verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_removed_before_the_snapshot_resolves_unknown_from_current_state() {
    let node = projection_node("removed-before").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let (kept, dropped) = (
        node.node_id().wrapping_add(1),
        node.node_id().wrapping_add(2),
    );
    let demand = family.reconcile(TAG, &[kept, dropped]).expect("retain");
    let keys = branches(&demand);
    for (_, branch) in &keys {
        admit(&node, branch, beat(AttestedStatus::NotReady, 5, 1, true));
    }
    let before = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        before.non_viable(),
        &[kept, dropped],
        "precondition: both carry fresh explicit NotReady"
    );

    // Narrow the demand: `dropped`'s interest is retired, and with it the
    // consumer cell its evidence lived in.
    let narrowed = family.reconcile(TAG, &[kept]).expect("re-converge");
    assert_eq!(narrowed.retained_providers(), vec![kept]);

    // The OLD demand still names both providers - its population is immutable.
    let after = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        after.rows().len(),
        2,
        "the population is an immutable input: {after:?}"
    );
    assert_eq!(
        readiness_of(&after, dropped),
        ProjectedReadiness::Unknown,
        "removed evidence resolves Unknown from CURRENT state: {after:?}"
    );
    assert_eq!(
        after.non_viable(),
        &[kept],
        "and Unknown never prunes, so the removed provider is not non-viable: {after:?}"
    );
    assert!(after.potential().contains(&dropped));

    drop(narrowed);
    drop(demand);
    drop(family);
}

/// A capture is ADVISORY plain data: churn after it cannot be authorized by it,
/// and the projection itself grants nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_provider_removed_after_the_snapshot_is_advisory_and_cannot_authorize() {
    let node = projection_node("removed-after").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let (kept, gone) = (
        node.node_id().wrapping_add(1),
        node.node_id().wrapping_add(2),
    );
    let demand = family.reconcile(TAG, &[kept, gone]).expect("retain");
    let keys = branches(&demand);
    let gone_branch = keys
        .iter()
        .find(|(provider, _)| *provider == gone)
        .map(|(_, branch)| branch.clone())
        .expect("retained");
    for (_, branch) in &keys {
        admit(&node, branch, beat(AttestedStatus::Ready, 5, 1, true));
    }

    let captured = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert!(
        captured.viable().contains(&gone),
        "precondition: it was viable when observed: {captured:?}"
    );
    let snapshot = captured.clone();

    // Now retire it out from under the capture.
    let narrowed = family.reconcile(TAG, &[kept]).expect("re-converge");
    assert_eq!(narrowed.retained_providers(), vec![kept]);

    // The captured value describes the instant it was taken and does not
    // change - it is a value, not a live view.
    assert_eq!(
        captured, snapshot,
        "a capture is immutable plain data, not a handle on live state"
    );

    // But it confers nothing: the branch has no evidence and no retained
    // interest any more, so a fresh projection cannot offer it, and the
    // retirement is what decides that - not the order.
    assert_eq!(
        node.sensing_projected(&gone_branch),
        ProjectedReadiness::Unknown,
        "sensing holds no live claim for a retired branch"
    );
    assert!(
        !narrowed
            .retained_branches_for_test()
            .iter()
            .any(|(provider, _)| *provider == gone),
        "and the demand no longer retains it, so no later capture can resurrect it"
    );
    let fresh = narrowed.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        fresh.rows().len(),
        1,
        "the current demand's population is the current authorization: {fresh:?}"
    );
    assert!(!fresh.viable().contains(&gone));

    drop(narrowed);
    drop(demand);
    drop(family);
}

/// The proximity pass, the budget classification and the ordering all run with
/// every sensing guard released — observed from inside those phases, not
/// asserted about them from outside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_sensing_lock_is_held_during_route_budget_or_sort_work() {
    let node = projection_node("off-lock").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let provider = node.node_id().wrapping_add(1);
    let demand = family.reconcile(TAG, &[provider]).expect("retain");
    let keys = branches(&demand);
    admit(&node, &keys[0].1, beat(AttestedStatus::Ready, 5, 1, true));

    let observed: OffLockLog = Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let observed = Arc::clone(&observed);
        node.set_sensing_projection_offlock_observer_for_test(Arc::new(
            move |phase, depth, held| {
                observed.lock().push((phase, depth, held));
            },
        ));
    }

    let projection = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(projection.viable(), &[provider], "the projection ran");

    let seen = observed.lock().clone();
    let phases: Vec<&str> = seen.iter().map(|(phase, _, _)| *phase).collect();
    assert!(
        phases.contains(&"proximity") && phases.contains(&"classification"),
        "both off-lock phases must report, or this witness proves nothing: {phases:?}"
    );
    for (phase, depth, held) in &seen {
        assert_eq!(
            *depth, 0,
            "{phase} ran holding {depth} sensing guard(s): {held}"
        );
        assert!(
            !held.contains("sensing_"),
            "{phase} ran holding {held}, which must be released first"
        );
    }

    drop(demand);
    drop(family);
}

/// A cell that belongs to ANOTHER interest for the same provider must not
/// contribute readiness — the capture is clamped to THIS demand's retained
/// branch keys, not to "whatever the map holds for this provider id".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unrelated_interest_for_the_same_provider_never_contributes_readiness() {
    let node = projection_node("clamped").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let provider = node.node_id().wrapping_add(1);

    // TWO capabilities, ONE provider: two distinct interest digests, two
    // distinct observation cells.
    let watched = family.reconcile(TAG, &[provider]).expect("retain watched");
    let other = family
        .reconcile("nrpc:other.capability", &[provider])
        .expect("retain other");
    let other_branch = branches(&other)
        .into_iter()
        .next()
        .map(|(_, branch)| branch)
        .expect("retained");
    let watched_branch = branches(&watched)
        .into_iter()
        .next()
        .map(|(_, branch)| branch)
        .expect("retained");
    assert_ne!(
        watched_branch, other_branch,
        "precondition: one provider, two interests, two branch keys"
    );

    // Only the OTHER interest is Ready.
    admit(
        &node,
        &other_branch,
        beat(AttestedStatus::Ready, 5, 1, true),
    );

    let projection =
        watched.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        projection.rows().len(),
        1,
        "one row per population member, from this demand's keys alone: {projection:?}"
    );
    assert_eq!(
        readiness_of(&projection, provider),
        ProjectedReadiness::Unknown,
        "another interest's Ready proof must not answer for this one: {projection:?}"
    );
    assert!(
        projection.viable().is_empty(),
        "and it must not make the provider viable here: {projection:?}"
    );

    // The other demand, at the same instant, DOES see its own evidence - so
    // the clamp is what makes the difference, not a missing beat.
    let sibling = other.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(sibling.viable(), &[provider]);

    drop(watched);
    drop(other);
    drop(family);
}

// ---- THE DETERMINISTIC ORDER -------------------------------------------

/// The pinned interleaved list: sensed-viable first in SENSED RANK ORDER, then
/// everything unverdicted in input order, then the not-ready ones — none
/// removed.
#[test]
fn the_sensed_order_is_a_stable_class_permutation_of_the_complete_list() {
    // A:SameOrg, B:Granted, C:SameOrg, D:Granted, E:SameOrg
    let providers = [0xA, 0xB, 0xC, 0xD, 0xE];
    let same_org = [true, false, true, false, true];
    let ranked = [0xE, 0xC]; // sensed viable, in (cost, provider) order
    let pruned = [0xA]; // fresh explicit NotReady

    let order = org_sensed_bucket_permutation(&same_org, &providers, &ranked, &pruned);
    assert_eq!(
        order,
        vec![4, 2, 1, 3, 0],
        "E before C because sensing ranked it first; B before D because the \
         input did; A last WITHOUT being removed"
    );

    // Emitting the viable bucket in input order instead of sensed rank order is
    // the mutation this pins against.
    let emitted: Vec<u64> = order.iter().map(|&index| providers[index]).collect();
    assert_eq!(emitted, vec![0xE, 0xC, 0xB, 0xD, 0xA]);

    // An all-pruned list falls back to the input order and loses nothing.
    let all_pruned = org_sensed_bucket_permutation(&same_org, &providers, &[], &[0xA, 0xC, 0xE]);
    assert_eq!(all_pruned, vec![1, 3, 0, 2, 4]);
}

/// The complete candidate list is NOT bounded by the sensing population cap:
/// unsensed same-organization candidates and cross-organization ones keep their
/// places, and the result is always a permutation.
#[test]
fn a_candidate_list_beyond_the_sensing_cap_keeps_every_unsensed_member() {
    // 70 candidates: even indices same-organization, odd cross-organization.
    let providers: Vec<u64> = (0..70u64).map(|i| 1000 + i).collect();
    let same_org: Vec<bool> = (0..70).map(|i| i % 2 == 0).collect();
    // 32 sensed rows is the population cap; rank them in DESCENDING provider id
    // so sensed order cannot be mistaken for the input's own order.
    let sensed: Vec<u64> = providers
        .iter()
        .zip(&same_org)
        .filter(|(_, &owned)| owned)
        .map(|(provider, _)| *provider)
        .take(32)
        .collect();
    let mut ranked: Vec<u64> = sensed.iter().copied().take(30).collect();
    ranked.reverse();
    let pruned: Vec<u64> = sensed.iter().copied().skip(30).collect();

    let order = org_sensed_bucket_permutation(&same_org, &providers, &ranked, &pruned);

    let mut permutation = order.clone();
    permutation.sort_unstable();
    assert_eq!(
        permutation,
        (0..providers.len()).collect::<Vec<_>>(),
        "every candidate appears exactly once - none invented, none dropped"
    );

    let emitted: Vec<u64> = order.iter().map(|&index| providers[index]).collect();
    assert_eq!(
        &emitted[..ranked.len()],
        ranked.as_slice(),
        "the viable prefix is the sensed rank order verbatim"
    );

    let tail = &emitted[ranked.len()..];
    let split = tail.len() - pruned.len();
    let (potential, non_viable) = tail.split_at(split);
    let mut expected_potential: Vec<u64> = providers
        .iter()
        .copied()
        .filter(|provider| !ranked.contains(provider) && !pruned.contains(provider))
        .collect();
    assert_eq!(
        potential.to_vec(),
        expected_potential,
        "unsensed same-org and cross-org candidates keep their INPUT order"
    );
    expected_potential.sort_unstable();
    assert!(
        potential
            .iter()
            .any(|provider| same_org[providers.iter().position(|p| p == provider).expect("index")]),
        "an unsensed same-organization candidate must be here, not pruned"
    );
    assert_eq!(
        non_viable.to_vec(),
        pruned,
        "and the not-ready ones are ordered last, still present"
    );
}

/// The population clamp bounds what is SENSED, never what is offered: a demand
/// over more providers than the cap still yields one row per sensed member, and
/// the excess candidates survive the order as unsensed fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_population_clamp_bounds_rows_but_never_drops_a_candidate() {
    let node = projection_node("clamp").await;
    let family = OrgSensingFamily::mint(&node).expect("mint");
    let candidates: Vec<u64> = (1..=40u64)
        .map(|offset| node.node_id().wrapping_add(offset))
        .collect();
    let demand = family
        .reconcile(TAG, &candidates)
        .expect("retain up to the cap");
    assert_eq!(
        demand.population().len(),
        32,
        "the sensed population is capped: {:?}",
        demand.population()
    );

    let keys = branches(&demand);
    for (_, branch) in keys.iter().take(3) {
        admit(&node, branch, beat(AttestedStatus::Ready, 5, 1, true));
    }
    let projection = demand.project_sensed_order(Instant::now(), &ConsumerLatencyBudget::default());
    assert_eq!(
        projection.rows().len(),
        32,
        "one row per SENSED member, not per candidate"
    );

    // The complete authorized list is all 40, in the caller's own order.
    let mut complete = candidates.clone();
    complete.sort_unstable();
    let same_org = vec![true; complete.len()];
    let order = org_sensed_bucket_permutation(
        &same_org,
        &complete,
        projection.viable(),
        projection.non_viable(),
    );
    let mut permutation = order.clone();
    permutation.sort_unstable();
    assert_eq!(
        permutation,
        (0..complete.len()).collect::<Vec<_>>(),
        "all 40 candidates survive the order"
    );
    let emitted: Vec<u64> = order.iter().map(|&index| complete[index]).collect();
    assert_eq!(
        &emitted[..projection.viable().len()],
        projection.viable(),
        "the sensed prefix leads"
    );
    let unsensed: Vec<u64> = complete
        .iter()
        .copied()
        .filter(|provider| !demand.population().contains(provider))
        .collect();
    assert_eq!(unsensed.len(), 8, "eight candidates were never sensed");
    for provider in unsensed {
        assert!(
            emitted.contains(&provider),
            "an unsensed candidate must remain eligible: {provider:#x}"
        );
    }

    drop(demand);
    drop(family);
}
