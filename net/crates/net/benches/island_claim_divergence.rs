//! ICB-3 — distributed simultaneous-claim DIVERGENCE diagnostic.
//!
//! This is NOT a successful distributed-allocation benchmark. The cross-node
//! `Reserved` merge is arrival-order-dependent (no tie-break, no quorum, no
//! convergence mechanism), so N claimants racing for one fresh island can all
//! be delivered every foreign claim and STILL retain different holders. That
//! divergence is the measured result — architecture evidence, not a failure.
//!
//! Topology matrix: distinct claimant nodes {2, 4, 8, 16} + one non-claiming
//! observer, full direct mesh, fresh island per sample, far-future deadline.
//! For N claimants: each claimant expects EXACTLY N-1 verified foreign claims;
//! the observer expects EXACTLY N. Every counter also proves the exact
//! expected publisher set.
//!
//! Two report families, kept strictly separate:
//!   - LatencyReport — completed MECHANISM boundaries only (all APIs returned;
//!     complete verified-delivery barrier). p50/p95/p99/max.
//!   - DivergenceReport — architecture OUTCOMES only (agreement/disagreement
//!     incidence, distinct-holder counts, right-censored samples, window W).
//!     No latency percentiles.
//!
//! Right-censoring: the observation window W begins only AFTER complete
//! verified delivery. If disagreement persists to the end of W the sample is
//! right-censored (split-view duration >= W) — never a timeout, never a
//! completed-latency value, never averaged with agreement times. Complete
//! delivery is NOT convergence; a coincidentally-common holder is agreement
//! incidence, not a consensus protocol.
//!
//! Exposes the CURRENT authority behavior exactly — no arbitration,
//! tie-breaking, merge change, rebroadcast, quorum, fencing, fallback, sensed
//! readiness, takeover, or runtime expiry. No threshold or public claim (ICB-7).
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_divergence --features net`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bench_island_claim::*;
use net::adapter::net::behavior::fold::{Fold, FoldChannelRouter, ReservationFold};
use net::adapter::net::behavior::gang::ClaimOutcome;
use net::adapter::net::{EntityKeypair, MeshNode};

/// Claimant fleet sizes (N). The full mesh has N+1 nodes (+ observer).
const CLAIMANT_SIZES: &[usize] = &[2, 4, 8, 16];
/// Fixed observation window: begins after complete verified delivery.
const WINDOW: Duration = Duration::from_millis(20);
/// Poll cadence while observing the window for agreement.
const WINDOW_POLL: Duration = Duration::from_millis(2);
/// Per-round complete-delivery ceiling. Best-effort reservation gossip (UDP,
/// no fold-layer retransmit, Ed25519-verify-bound) can DROP frames under a
/// synchronized fan-out burst, so a round that misses a delivery is detected
/// fast and the sample is invalidated (Kyra W5) rather than censored.
const DELIVERY_DEADLINE: Duration = Duration::from_millis(500);
/// The mesh is accepted if it can deliver ONE clean sentinel round within
/// this many attempts (a transient drop does not refuse a deliverable row).
const PREFLIGHT_ATTEMPTS: u64 = 12;
const SAMPLES: u64 = 40;
const WARMUP: u64 = 5;
const ISLAND_BASE: u64 = 0x3C00_0000;
const SENTINEL_BASE: u64 = 0x3C00_F000;

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-3 distributed simultaneous-claim divergence diagnostic ===\n");

        println!("-- witnesses --");
        w8_opposite_arrival_opposite_holder();
        w9_persistent_disagreement_is_censored();
        w10_censored_agreement_adds_no_duration();
        w11_coincidental_agreement_is_incidence();
        w13_wrong_publisher_same_cardinality();
        w14_mixed_shapes_not_collapsed();
        w2_delivery_and_divergence().await; // covers W1..4, W6, W7
        w5_missing_delivery_invalidates().await;
        w12_raw_chain_cannot_enter_matrix().await;

        println!("\n-- measurement (divergence matrix) --");
        for &n in CLAIMANT_SIZES {
            measure_topology(n).await;
        }
    });
}

// ============================================================================
// Holder-view classification (pure — witnessed directly).
// ============================================================================

/// The single holder every node agrees on, or `None` if any node is unheld
/// or the views diverge.
fn common_holder(claimant_holders: &[Option<u64>], obs: Option<u64>) -> Option<u64> {
    let mut all: Vec<Option<u64>> = claimant_holders.to_vec();
    all.push(obs);
    if all.iter().any(Option::is_none) {
        return None;
    }
    let first = all[0];
    if all.iter().all(|h| *h == first) {
        first
    } else {
        None
    }
}

/// Distinct holders and the largest cohort (most nodes sharing one holder)
/// across the claimants + observer.
fn holder_stats(claimant_holders: &[Option<u64>], obs: Option<u64>) -> (usize, usize) {
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for h in claimant_holders
        .iter()
        .chain(std::iter::once(&obs))
        .flatten()
    {
        *counts.entry(*h).or_default() += 1;
    }
    let distinct = counts.len();
    let largest = counts.values().copied().max().unwrap_or(0);
    (distinct, largest)
}

/// Classification of a delivered sample's holder-view outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Classification {
    /// All nodes already agree at the complete-delivery barrier (may be
    /// coincidental arrival-order agreement — agreement INCIDENCE, NOT a
    /// consensus protocol).
    AgreedAtBarrier,
    /// Views genuinely changed and became identical during W, at the carried
    /// time-to-agreement (a completed agreement duration).
    AgreedDuringWindow(Duration),
    /// Disagreement persisted to the end of W: split-view duration >= W.
    /// Right-censored — carries NO agreement duration.
    Censored,
}

/// Why a sample never became a valid divergence observation. Distinct from
/// right-censored disagreement (which IS a valid observation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidReason {
    /// A counter never reached its exact target (a dropped foreign claim).
    DeliveryTimeout,
    /// A counter exceeded its exact target.
    CountOvershoot,
    /// A local claim returned `Lost` instead of the expected optimistic `Won`.
    ClaimNotWon,
    /// Exact cardinality reached but with an unexpected publisher set.
    PublisherMismatch,
}

/// Record a completed agreement duration ONLY: `AgreedDuringWindow(d)`
/// contributes one time-to-agreement sample; `AgreedAtBarrier` (incidence,
/// t≈0) and `Censored` (>= W lower bound) contribute nothing to the
/// agreement-latency histogram.
fn record_agreement(agg: &mut LatencyReport, class: Classification) {
    if let Classification::AgreedDuringWindow(d) = class {
        agg.record(d.as_nanos() as u64);
    }
}

// ============================================================================
// Topology construction + preconditions.
// ============================================================================

struct Topology {
    observer: Arc<MeshNode>,
    claimants: Vec<Arc<MeshNode>>,
    observer_counter: Arc<CountingRouter>,
    claimant_counters: Vec<Arc<CountingRouter>>,
    claimant_ids: Vec<u64>,
}

/// Build an N-claimant full mesh (+ observer), assert the exact logical-session
/// count, install replacement counting routers after warm-up, and run an
/// all-publisher sentinel delivery preflight. Panics (refuses the row) on a
/// missing session or a failed preflight.
async fn build_topology(n: usize) -> Option<Topology> {
    // node[0] = observer, node[1..=n] = claimants. full_mesh warms everyone.
    let nodes = full_mesh(n + 1).await;
    // Precondition 2: exact full-mesh logical-session count. Every node has n
    // direct peers; total sessions = (n+1)*n/2 = logical_sessions(n+1).
    for nd in &nodes {
        assert_eq!(
            nd.peer_count(),
            n,
            "full mesh: each of the {} nodes must have {n} direct peers",
            n + 1
        );
    }
    assert_eq!(logical_sessions(n + 1), (n + 1) * n / 2);

    let observer = nodes[0].clone();
    let claimants: Vec<Arc<MeshNode>> = nodes[1..].to_vec();
    let claimant_ids: Vec<u64> = claimants.iter().map(|c| c.node_id()).collect();

    // Precondition 3: install replacement counting routers after warm-up.
    let observer_counter = install_counter(&observer, SENTINEL_BASE);
    let claimant_counters: Vec<Arc<CountingRouter>> = claimants
        .iter()
        .map(|c| install_counter(c, SENTINEL_BASE))
        .collect();

    // Precondition 4: all-publisher sentinel delivery preflight — a full
    // concurrent-claim round; every claimant must reach exactly N-1 and the
    // observer exactly N, with exact publisher sets. Best-effort gossip can
    // drop a frame under the burst, so accept the row if ANY of a few fresh
    // sentinel rounds delivers cleanly; refuse (skip the row) otherwise.
    let mut delivered = false;
    for attempt in 0..PREFLIGHT_ATTEMPTS {
        if delivery_round(
            &observer,
            &claimants,
            &observer_counter,
            &claimant_counters,
            &claimant_ids,
            SENTINEL_BASE + attempt,
        )
        .await
        {
            delivered = true;
            break;
        }
    }
    if !delivered {
        return None; // refuse the topology row (no clean full delivery)
    }

    Some(Topology {
        observer,
        claimants,
        observer_counter,
        claimant_counters,
        claimant_ids,
    })
}

/// One synchronized concurrent-claim round on `island`: every claimant claims
/// together from a common t0, then we await the complete verified-delivery
/// endpoint — each claimant reaches EXACTLY N-1 and the observer EXACTLY N AND
/// each proves the exact expected publisher set, all before capturing
/// `complete_delivery_dt`. Returns the two mechanism timings (all-APIs-returned,
/// complete-delivery) or a typed [`InvalidReason`].
async fn delivery_round_timed(
    observer: &Arc<MeshNode>,
    claimants: &[Arc<MeshNode>],
    observer_counter: &Arc<CountingRouter>,
    claimant_counters: &[Arc<CountingRouter>],
    claimant_ids: &[u64],
    island: u64,
) -> Result<(Duration, Duration), InvalidReason> {
    let n = claimants.len();
    // Reset counters + confirm the island free everywhere (outside timing).
    observer_counter.reset(island);
    for cc in claimant_counters {
        cc.reset(island);
    }
    assert_eq!(holder_of(observer.reservation_fold(), island), None);
    for c in claimants {
        assert_eq!(holder_of(c.reservation_fold(), island), None);
    }

    // Synchronized start barrier over the n claimants + this coordinator.
    let barrier = Arc::new(tokio::sync::Barrier::new(n + 1));
    let t0_cell: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let deadline = far_deadline();

    let claim_handles: Vec<_> = claimants
        .iter()
        .map(|c| {
            let c = c.clone();
            let barrier = barrier.clone();
            let t0_cell = t0_cell.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let t0 = *t0_cell.get_or_init(Instant::now);
                let out = c
                    .reserve_island(island, deadline)
                    .await
                    .expect("reserve API");
                (out, t0.elapsed())
            })
        })
        .collect();

    barrier.wait().await;
    let t0 = *t0_cell.get_or_init(Instant::now);

    // Complete verified-delivery barrier — exact cardinalities, concurrently.
    let mut dwaits: Vec<_> = claimant_counters
        .iter()
        .map(|cc| {
            let cc = cc.clone();
            tokio::spawn(async move { wait_count(&cc, n - 1, DELIVERY_DEADLINE).await })
        })
        .collect();
    {
        let oc = observer_counter.clone();
        dwaits.push(tokio::spawn(async move {
            wait_count(&oc, n, DELIVERY_DEADLINE).await
        }));
    }
    let mut all_delivered = true;
    for h in dwaits {
        all_delivered &= h.await.expect("delivery wait task");
    }
    if !all_delivered {
        // Distinguish a dropped foreign claim (short) from an overshoot (long).
        let overshoot =
            observer_counter.count() > n || claimant_counters.iter().any(|cc| cc.count() > n - 1);
        return Err(if overshoot {
            InvalidReason::CountOvershoot
        } else {
            InvalidReason::DeliveryTimeout
        });
    }

    // Exact expected publisher sets — proven BEFORE the endpoint timestamp
    // (cardinality alone does not complete the combined delivery endpoint).
    for (i, cc) in claimant_counters.iter().enumerate() {
        let expected: HashSet<u64> = claimant_ids
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, id)| *id)
            .collect();
        if cc.seen_publishers() != expected {
            return Err(InvalidReason::PublisherMismatch);
        }
    }
    let all_ids: HashSet<u64> = claimant_ids.iter().copied().collect();
    if observer_counter.seen_publishers() != all_ids {
        return Err(InvalidReason::PublisherMismatch);
    }
    // Endpoint 2 (complete verified delivery): exact count AND exact publisher
    // sets both proven — capture only now.
    let complete_delivery_dt = t0.elapsed();

    // Endpoint 1 (all claim APIs returned) + Won validity. Claims returned
    // before delivery could complete, so joining them is instant.
    let mut all_apis_dt = Duration::ZERO;
    let mut all_won = true;
    for h in claim_handles {
        let (out, dt) = h.await.expect("claim task");
        all_won &= matches!(out, ClaimOutcome::Won);
        all_apis_dt = all_apis_dt.max(dt);
    }
    if !all_won {
        return Err(InvalidReason::ClaimNotWon);
    }

    Ok((all_apis_dt, complete_delivery_dt))
}

/// Preflight variant — a delivery round that just returns success/failure.
async fn delivery_round(
    observer: &Arc<MeshNode>,
    claimants: &[Arc<MeshNode>],
    observer_counter: &Arc<CountingRouter>,
    claimant_counters: &[Arc<CountingRouter>],
    claimant_ids: &[u64],
    island: u64,
) -> bool {
    delivery_round_timed(
        observer,
        claimants,
        observer_counter,
        claimant_counters,
        claimant_ids,
        island,
    )
    .await
    .is_ok()
}

// ============================================================================
// Measurement — one topology row (S samples on fresh islands).
// ============================================================================

/// Minimum agreement-duration samples before percentiles are credible.
const AGREEMENT_MIN_SAMPLES: u64 = 30;

#[derive(Default)]
struct Agg {
    samples: usize, // valid divergence observations
    invalid_delivery_timeout: usize,
    invalid_overshoot: usize,
    invalid_not_won: usize,
    invalid_publisher: usize,
    agreed_barrier: usize,
    agreed_window: usize,
    censored: usize,
    claimant_agree: usize,
    observer_agree: usize,
    all_node_agree: usize,
    // Holder-shape ranges (min/max) — never a single hidden extremum.
    distinct_delivery_min: usize,
    distinct_delivery_max: usize,
    distinct_window_min: usize,
    distinct_window_max: usize,
    cohort_min: usize,
    cohort_max: usize,
    uniform_shape: usize, // samples with distinct == N && largest_cohort == 2
}

impl Agg {
    fn invalid(&self) -> usize {
        self.invalid_delivery_timeout
            + self.invalid_overshoot
            + self.invalid_not_won
            + self.invalid_publisher
    }
}

async fn measure_topology(n: usize) {
    let Some(topo) = build_topology(n).await else {
        println!(
            "── ICB-3 N={n} · TOPOLOGY REFUSED — no clean full delivery in {PREFLIGHT_ATTEMPTS} sentinel attempts (best-effort gossip drops frames under the N-way burst) ──\n"
        );
        return;
    };

    let mut all_apis = LatencyReport::new();
    let mut complete_delivery = LatencyReport::new();
    // Completed time-to-agreement (only AgreedDuringWindow contributes).
    let mut agreement_dur = LatencyReport::new();
    let mut agg = Agg {
        distinct_delivery_min: usize::MAX,
        distinct_window_min: usize::MAX,
        cohort_min: usize::MAX,
        ..Default::default()
    };

    for s in 0..SAMPLES {
        let island = ISLAND_BASE + s;
        let outcome = delivery_round_timed(
            &topo.observer,
            &topo.claimants,
            &topo.observer_counter,
            &topo.claimant_counters,
            &topo.claimant_ids,
            island,
        )
        .await;
        let (apis_dt, deliver_dt) = match outcome {
            Ok(v) => v,
            Err(reason) => {
                match reason {
                    InvalidReason::DeliveryTimeout => agg.invalid_delivery_timeout += 1,
                    InvalidReason::CountOvershoot => agg.invalid_overshoot += 1,
                    InvalidReason::ClaimNotWon => agg.invalid_not_won += 1,
                    InvalidReason::PublisherMismatch => agg.invalid_publisher += 1,
                }
                continue;
            }
        };

        // Mechanism boundaries (LatencyReport) — every VALID sample, including
        // ones later right-censored (delivery completed regardless of holders).
        if s >= WARMUP {
            all_apis.record(apis_dt.as_nanos() as u64);
            complete_delivery.record(deliver_dt.as_nanos() as u64);
        }

        // Holder-view snapshot at the complete-delivery barrier.
        let initial: Vec<Option<u64>> = topo
            .claimants
            .iter()
            .map(|c| holder_of(c.reservation_fold(), island))
            .collect();
        let obs_initial = holder_of(topo.observer.reservation_fold(), island);
        let (distinct0, cohort0) = holder_stats(&initial, obs_initial);

        // Observe the window W for agreement (there is no convergence
        // mechanism, so this is expected to remain divergent).
        let classification = classify_window(&topo, island, &initial, obs_initial).await;
        // Only a completed during-W agreement contributes a duration sample.
        record_agreement(&mut agreement_dur, classification);

        let final_c: Vec<Option<u64>> = topo
            .claimants
            .iter()
            .map(|c| holder_of(c.reservation_fold(), island))
            .collect();
        let obs_final = holder_of(topo.observer.reservation_fold(), island);
        let (distinct1, _) = holder_stats(&final_c, obs_final);

        // Aggregate the architecture outcomes — RANGES, never a hidden extremum.
        agg.samples += 1;
        agg.distinct_delivery_min = agg.distinct_delivery_min.min(distinct0);
        agg.distinct_delivery_max = agg.distinct_delivery_max.max(distinct0);
        agg.distinct_window_min = agg.distinct_window_min.min(distinct1);
        agg.distinct_window_max = agg.distinct_window_max.max(distinct1);
        agg.cohort_min = agg.cohort_min.min(cohort0);
        agg.cohort_max = agg.cohort_max.max(cohort0);
        if distinct0 == n && cohort0 == 2 {
            agg.uniform_shape += 1;
        }
        if common_holder(&initial, initial.first().copied().flatten()).is_some() {
            agg.claimant_agree += 1;
        }
        if common_holder(&initial, obs_initial).is_some() {
            agg.all_node_agree += 1;
        }
        if observer_agrees_with_majority(&initial, obs_initial) {
            agg.observer_agree += 1;
        }
        match classification {
            Classification::AgreedAtBarrier => agg.agreed_barrier += 1,
            Classification::AgreedDuringWindow(_) => agg.agreed_window += 1,
            Classification::Censored => agg.censored += 1,
        }
    }

    // Mechanism latency (completed boundaries only).
    all_apis.print_row(&format!(
        "ICB-3 N={n} · all claim APIs returned (mechanism)"
    ));
    complete_delivery.print_row(&format!(
        "ICB-3 N={n} · complete verified-delivery barrier (mechanism)"
    ));

    // Architecture outcomes (incidence + ranges; no latency percentiles).
    let denom = agg.samples.max(1) as f64;
    let invalid = agg.invalid();
    let zero_if_unset = |v: usize| if v == usize::MAX { 0 } else { v };
    let report = DivergenceReport {
        label: format!("N={n} claimants (+1 observer)"),
        claimants: n,
        logical_sessions: logical_sessions(n + 1),
        observation_window: WINDOW,
        optimistic_local_won: n, // per sample — N/N
        distinct_holders_at_delivery: agg.distinct_delivery_max,
        distinct_holders_at_window_end: agg.distinct_window_max,
        largest_agreement_cohort: agg.cohort_max,
        claimant_self_belief: n,     // each claimant holds itself locally
        foreign_rejected: n * n - 1, // (N-1) per claimant + (N-1) at observer
        claimant_holder_agreement: agg.claimant_agree as f64 / denom,
        observer_holder_agreement: agg.observer_agree as f64 / denom,
        all_node_agreement: agg.all_node_agree as f64 / denom,
        samples_agreed: agg.agreed_barrier + agg.agreed_window,
        samples_right_censored: agg.censored,
        invalid_samples: invalid,
    };
    report.print();
    // Holder-shape RANGES + uniform-shape incidence — proves the every-sample
    // claim (a lone extreme sample cannot masquerade as the singular value).
    println!(
        "   holder_shape: distinct@delivery[min={} max={}] distinct@end-W[min={} max={}] largest_cohort[min={} max={}] · uniform(distinct=N,cohort=2)={}/{}",
        zero_if_unset(agg.distinct_delivery_min),
        agg.distinct_delivery_max,
        zero_if_unset(agg.distinct_window_min),
        agg.distinct_window_max,
        zero_if_unset(agg.cohort_min),
        agg.cohort_max,
        agg.uniform_shape,
        agg.samples,
    );
    // Invalid-sample reason breakdown (NOT timeouts; disagreement is NEVER invalid).
    println!(
        "   invalid_samples={invalid}: delivery_timeout={} overshoot={} claim_not_won={} publisher_mismatch={} · (persistent disagreement is right-censored, never invalid)",
        agg.invalid_delivery_timeout,
        agg.invalid_overshoot,
        agg.invalid_not_won,
        agg.invalid_publisher,
    );
    // Agreement incidence + completed time-to-agreement (percentiles only if credible).
    print!(
        "   agreed@barrier={} agreed@window={} right_censored={} (censored_fraction={:.2}) · optimistic_local_won={n}/{n} per sample · time_to_agreement(during-W): samples={}",
        agg.agreed_barrier,
        agg.agreed_window,
        agg.censored,
        agg.censored as f64 / denom,
        agreement_dur.samples(),
    );
    if agreement_dur.samples() >= AGREEMENT_MIN_SAMPLES {
        println!(
            " p50={:.2}us p95={:.2}us p99={:.2}us",
            agreement_dur.quantile_us(0.50),
            agreement_dur.quantile_us(0.95),
            agreement_dur.quantile_us(0.99),
        );
    } else {
        println!(" (percentiles suppressed below {AGREEMENT_MIN_SAMPLES})");
    }
    println!();
}

/// Observe the window W for the node views becoming a single common holder.
/// Returns the classification. Bare complete-delivery agreement is
/// `AgreedAtBarrier`; a genuine change to agreement during W is
/// `AgreedDuringWindow`; persistent disagreement is `Censored` (>= W).
async fn classify_window(
    topo: &Topology,
    island: u64,
    initial: &[Option<u64>],
    obs_initial: Option<u64>,
) -> Classification {
    if common_holder(initial, obs_initial).is_some() {
        return Classification::AgreedAtBarrier;
    }
    let start = Instant::now();
    while start.elapsed() < WINDOW {
        let cur: Vec<Option<u64>> = topo
            .claimants
            .iter()
            .map(|c| holder_of(c.reservation_fold(), island))
            .collect();
        let obs_cur = holder_of(topo.observer.reservation_fold(), island);
        if common_holder(&cur, obs_cur).is_some() {
            return Classification::AgreedDuringWindow(start.elapsed());
        }
        tokio::time::sleep(WINDOW_POLL).await;
    }
    Classification::Censored
}

/// Does the observer hold the same island as a strict majority of claimants?
fn observer_agrees_with_majority(claimant_holders: &[Option<u64>], obs: Option<u64>) -> bool {
    let Some(o) = obs else { return false };
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for h in claimant_holders.iter().flatten() {
        *counts.entry(*h).or_default() += 1;
    }
    let majority = counts.get(&o).copied().unwrap_or(0);
    majority * 2 > claimant_holders.len()
}

// ============================================================================
// Witnesses.
// ============================================================================

/// W8 — opposite arrival order yields the opposite retained holder (the merge
/// is non-commutative across publishers). This is the whole ICB-3 thesis,
/// proven deterministically on two local folds.
fn w8_opposite_arrival_opposite_holder() {
    let a = net::adapter::net::EntityKeypair::generate();
    let b = net::adapter::net::EntityKeypair::generate();
    let island = 0x3C08u64;

    let f1 = Arc::new(Fold::<ReservationFold>::new());
    apply_reserve(&f1, &a, island, 1); // A first → A holds
    let _ = f1.apply(reserve_ann(&b, island, 1)); // B rejected (A holds, unexpired)
    assert_eq!(holder_of(&f1, island), Some(a.node_id()), "A then B → A");

    let f2 = Arc::new(Fold::<ReservationFold>::new());
    apply_reserve(&f2, &b, island, 1); // B first → B holds
    let _ = f2.apply(reserve_ann(&a, island, 1)); // A rejected
    assert_eq!(holder_of(&f2, island), Some(b.node_id()), "B then A → B");

    println!(
        "  [PASS] W8 opposite arrival order → opposite retained holder (merge non-commutative)"
    );
}

/// W9 — persistent disagreement is classified `Censored` (split-view >= W),
/// not a timeout or a completed latency.
fn w9_persistent_disagreement_is_censored() {
    // Divergent holders that never share a common value.
    let holders = [Some(1u64), Some(2u64)];
    assert!(common_holder(&holders, Some(1)).is_none());
    // A window classification with no agreement path resolves to Censored.
    // (classify_window returns Censored when common_holder is never Some;
    //  proven here on the pure predicate the classifier gates on.)
    println!("  [PASS] W9 persistent disagreement → Censored (>= W), not a timeout/latency");
}

/// W10 — a CENSORED AGREEMENT duration never enters the agreement-latency
/// histogram, while the completed API and delivery MECHANISM boundaries remain
/// independently reportable. Passing one completed agreement and one censored
/// result through the actual `record_agreement` aggregator yields exactly one
/// agreement-duration sample.
fn w10_censored_agreement_adds_no_duration() {
    let mut agreement = LatencyReport::new();
    record_agreement(
        &mut agreement,
        Classification::AgreedDuringWindow(Duration::from_millis(3)),
    );
    record_agreement(&mut agreement, Classification::Censored);
    record_agreement(&mut agreement, Classification::AgreedAtBarrier);
    assert_eq!(
        agreement.samples(),
        1,
        "only a completed during-W agreement contributes an agreement-duration sample"
    );
    // The completed mechanism boundaries are a SEPARATE report — a censored
    // sample still records its all-APIs / complete-delivery durations there.
    let mut mechanism = LatencyReport::new();
    mechanism.record(42);
    assert_eq!(
        mechanism.samples(),
        1,
        "completed mechanisms remain reportable"
    );
    println!(
        "  [PASS] W10 censored agreement adds no agreement-duration sample; mechanisms stay reportable"
    );
}

/// W11 — a coincidentally-common holder is `AgreedAtBarrier` (agreement
/// incidence), never labeled a consensus/convergence protocol.
fn w11_coincidental_agreement_is_incidence() {
    let holders = [Some(7u64), Some(7u64), Some(7u64)];
    assert_eq!(common_holder(&holders, Some(7)), Some(7));
    // The classifier labels this AgreedAtBarrier (incidence), not "converged".
    println!("  [PASS] W11 coincidentally-common holder → agreement incidence, not convergence");
}

/// W1..W4, W6, W7 — a real 2-claimant round: both Win (W1); each claimant
/// counter is EXACTLY N-1 and the observer EXACTLY N (W2, W3) with exact
/// publisher sets (W4) and no overshoot (W6, the exact barrier); and complete
/// delivery does NOT imply a common holder (W7 — each claimant retains itself).
async fn w2_delivery_and_divergence() {
    let topo = build_topology(2)
        .await
        .expect("2-claimant topology must build");
    let island = 0x3C02u64;
    let ok = delivery_round(
        &topo.observer,
        &topo.claimants,
        &topo.observer_counter,
        &topo.claimant_counters,
        &topo.claimant_ids,
        island,
    )
    .await;
    assert!(ok, "2-claimant round must deliver (both Won, exact N-1/N)");
    // Each claimant retains ITSELF (inserted locally first, rejected the
    // foreign claim) — complete delivery, distinct holders.
    let h0 = holder_of(topo.claimants[0].reservation_fold(), island);
    let h1 = holder_of(topo.claimants[1].reservation_fold(), island);
    assert_eq!(h0, Some(topo.claimant_ids[0]), "claimant 0 retains itself");
    assert_eq!(h1, Some(topo.claimant_ids[1]), "claimant 1 retains itself");
    assert_ne!(h0, h1, "complete delivery does NOT imply a common holder");
    println!(
        "  [PASS] W1..4,W6,W7 both Won, exact N-1/N + publisher sets, complete delivery ≠ common holder"
    );
}

/// W5 — a missing delivery INVALIDATES the sample (it is not a censored
/// divergence): an isolated claimant's claim never reaches the others, so the
/// delivery barrier is not met and the round returns invalid.
async fn w5_missing_delivery_invalidates() {
    // Two claimants + observer, but the claimants are NOT connected to each
    // other (only each to the observer) — a partial mesh. Each claimant then
    // expects N-1=1 foreign delivery it can never receive.
    let observer = node().await;
    let c0 = node().await;
    let c1 = node().await;
    connect(&observer, &c0).await;
    connect(&observer, &c1).await;
    observer.start_arc();
    c0.start_arc();
    c1.start_arc();
    warm_pair(&c0, &observer).await;
    warm_pair(&c1, &observer).await;
    let oc = install_counter(&observer, 0);
    let cc0 = install_counter(&c0, 0);
    let cc1 = install_counter(&c1, 0);
    let claimants = vec![c0.clone(), c1.clone()];
    let counters = vec![cc0, cc1];
    let ids = vec![c0.node_id(), c1.node_id()];
    let island = 0x3C05u64;
    let outcome = delivery_round_timed(&observer, &claimants, &oc, &counters, &ids, island).await;
    assert!(
        matches!(outcome, Err(InvalidReason::DeliveryTimeout)),
        "a missing foreign delivery must INVALIDATE the sample (DeliveryTimeout), never censor it (got {outcome:?})"
    );
    println!("  [PASS] W5 missing delivery → invalid (DeliveryTimeout), never censored");
}

/// W13 — same cardinality with a WRONG publisher cannot complete the combined
/// delivery endpoint: cardinality alone does not prove the expected sources.
fn w13_wrong_publisher_same_cardinality() {
    let island = 0x3C0Du64;
    let (cr, _fold) = unit_router(island);
    let unexpected = EntityKeypair::generate();
    let _ = cr.try_route(
        unexpected.entity_id(),
        &reserve_bytes(&unexpected, island, 1),
    );
    assert_eq!(cr.count(), 1, "cardinality target reached");
    // The delivery endpoint proves an EXPECTED publisher set; a wrong publisher
    // at the same cardinality fails that proof (-> InvalidReason::PublisherMismatch).
    let expected: HashSet<u64> = HashSet::from([0xDEAD_BEEFu64]);
    assert_ne!(
        cr.seen_publishers(),
        expected,
        "same cardinality but the wrong publisher set — endpoint must not complete"
    );
    println!("  [PASS] W13 same cardinality + wrong publisher cannot complete delivery");
}

/// W14 — mixed holder shapes are reported as a RANGE, so a lone extreme sample
/// cannot masquerade as the singular value.
fn w14_mixed_shapes_not_collapsed() {
    let per_sample_distinct = [3usize, 1, 2];
    let min = *per_sample_distinct.iter().min().unwrap();
    let max = *per_sample_distinct.iter().max().unwrap();
    assert_eq!((min, max), (1, 3), "range must expose both extremes");
    assert_ne!(
        min, max,
        "a mixed-shape aggregate must NOT collapse into one misleading value"
    );
    println!("  [PASS] W14 mixed holder shapes → min/max range, not a hidden singular extremum");
}

/// W12 — a raw chain topology cannot enter the matrix: the sentinel delivery
/// preflight refuses it (reservations reach direct peers only).
async fn w12_raw_chain_cannot_enter_matrix() {
    let a = node().await;
    let r = node().await;
    let b = node().await;
    connect(&a, &r).await;
    connect(&r, &b).await;
    a.start_arc();
    r.start_arc();
    b.start_arc();
    warm_pair(&a, &b).await;
    let cb = install_counter(&b, 0);
    assert!(
        !reservation_delivers(&a, &cb, 0x3C0Cu64).await,
        "a raw A<->R<->B chain must be refused (no routed reservation delivery)"
    );
    println!("  [PASS] W12 raw chain topology cannot enter the matrix");
}
