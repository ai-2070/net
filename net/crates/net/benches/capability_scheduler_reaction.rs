//! CPB-2: publication call → real scheduler-decision change. The
//! commercially meaningful boundary — not "the capability index
//! reacted" (that is CPB-1) but "the scheduler would now make a
//! different placement decision" (C3).
//!
//! - **CPB-2a** — publication → `subscribe_sensing_scheduler_inputs()`
//!   wake, attributed to the capability change by confirming the
//!   capability-fold generation advanced (the unified watch aggregates
//!   several planes, so a bare wake is not attributable; C3/plan §2).
//! - **CPB-2b (headline)** — publication → `match_islands(criteria)`
//!   returns a CHANGED result over seeded island topology. A provider's
//!   `gpu:h100` capability appearing/disappearing (propagated A→B) flips
//!   whether its island is a viable placement target. {direct, routed}.
//!
//! Setup: B is the matcher, built `.with_sensing_coalescing(true)` (the
//! scheduler-input plane is armed only then). B's `island_fold` is
//! seeded once (in-process, excluded from timing) with an island hosted
//! by A; the timed lever is A's capability announce over transport.
//!
//! No rank-change case (C3): island load / route economics / sensed
//! readiness are separate scheduler inputs and must not be smuggled
//! into a capability-announcement benchmark.
//!
//! Run: `cargo bench --features "net redex" --bench capability_scheduler_reaction`

#[path = "bench_mesh_pair/mod.rs"]
mod bench_mesh_pair;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_mesh_pair::*;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::behavior::fold::{
    CapabilityFilter as FoldCapFilter, CapabilityQuery, EnvelopeMeta, FoldKind, IslandRecord,
    IslandTopologyFold, SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
use net::adapter::net::{EntityKeypair, MeshNode};

const ITERS: u64 = 200;
const WARMUP: u64 = 20;
const DEADLINE: Duration = Duration::from_secs(5);

/// The island seeded on B, hosted by A.
const ISLAND: u64 = 0xA1;
/// The capability tag that makes A a viable host for its island.
const MATCH_TAG: &str = "gpu:h100";
/// A non-matching tag of equal length — the "disappeared" state, so
/// payload size never confounds appear vs disappear.
const NOMATCH_TAG: &str = "cpu:none";

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== CPB-2 publication -> scheduler-decision change ===\n");
        scheduler_input_wake_direct().await; // 2a
        match_change("A->B direct", 0, false).await; // 2b direct
        match_change("A->R->B routed", 1, true).await; // 2b routed
    });
}

// ============================================================================
// Criteria + island seeding.
// ============================================================================

/// Islands whose host matches `gpu:h100` and that carry ≥ 8 units,
/// ranked least-loaded — the gang matcher's real placement query.
fn criteria() -> MatchCriteria {
    MatchCriteria {
        capability: CapabilityQuery::Composite(FoldCapFilter {
            tags_all: vec![MATCH_TAG.into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    }
}

/// Seed `b`'s island topology with one island hosted by `a_id`, signed
/// by A's real keypair. In-process setup, excluded from timing.
fn seed_island(b: &Arc<MeshNode>, a_kp: &EntityKeypair, a_id: u64) {
    let record = IslandRecord {
        id: ISLAND,
        units: UnitSet::new((0..8u32).collect()),
        host: a_id,
        capabilities: vec!["model:a1".into()],
        load: 0.5,
        p50_latency_us: 1_500,
    };
    let ann = SignedAnnouncement::sign(
        a_kp,
        IslandTopologyFold::KIND_ID,
        0,
        a_id,
        1,
        EnvelopeMeta::default(),
        record,
    )
    .expect("sign island");
    b.island_fold().apply(ann).expect("apply island");
}

// ============================================================================
// CPB-2a — publication → scheduler-input wake (attributed via fold gen).
// ============================================================================

async fn scheduler_input_wake_direct() {
    let (a, b) = direct_pair(&BenchConfig::wire_floor_scheduler()).await;
    let manifest_bytes = manifest_bytes(&manifest_tags(&[MATCH_TAG, "gen:0"]));
    let mut report = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for i in 0..ITERS {
        let caps = manifest_tags(&[MATCH_TAG, &format!("gen:{i}")]);
        let outcome = timed_scheduler_wake(&a, &b, caps).await;
        tally(&mut report, &mut timeouts, i >= WARMUP, outcome);
    }

    report.print_row(RowMeta {
        label: "sched-input wake",
        start_event: "publish_call (announce_capabilities)",
        endpoint: "scheduler-input gen advanced (attributed: fold gen advanced)",
        topology: "A->B direct",
        hop_count: 0,
        manifest_bytes,
        version_delta: a.capability_announce_version() - version_before,
        candidate_pop: b.find_nodes_by_filter(&require_tag(MATCH_TAG)).len(),
        warmup: WARMUP,
        workers: WORKER_THREADS,
        topology_reused: true,
        timeouts,
        outliers: 0,
    });
}

/// Announce `caps`; stop once the scheduler-input generation advances
/// AND the capability-fold generation advanced — the latter attributes
/// the wake to THIS capability change, filtering spurious route/topology
/// plane wakes on the unified watch.
async fn timed_scheduler_wake(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
    caps: CapabilitySet,
) -> Result<Duration, ()> {
    let mut srx = b.subscribe_sensing_scheduler_inputs();
    let s0 = *srx.borrow();
    let f0 = b.capability_fold().change_generation();
    let t0 = Instant::now();
    a.announce_capabilities(caps).await.expect("announce");
    let wait = async {
        loop {
            if *srx.borrow() != s0 && b.capability_fold().change_generation() != f0 {
                break;
            }
            if srx.changed().await.is_err() {
                break;
            }
        }
    };
    match tokio::time::timeout(DEADLINE, wait).await {
        Ok(()) => Ok(t0.elapsed()),
        Err(_) => Err(()),
    }
}

// ============================================================================
// CPB-2b — publication → match_islands result changes (the headline).
// ============================================================================

async fn match_change(topology: &str, hop_count: u32, routed: bool) {
    // Build the topology, then seed B's island (host = A).
    let (a, b, _relay) = if routed {
        let (a, r, b) = routed_chain(&BenchConfig::wire_floor()).await;
        (a, b, Some(r))
    } else {
        let (a, b) = direct_pair(&BenchConfig::wire_floor()).await;
        (a, b, None)
    };
    let a_id = a.node_id();
    seed_island(&b, a.entity_keypair(), a_id);
    let crit = criteria();
    let manifest_bytes = manifest_bytes(&manifest_tags(&[MATCH_TAG, "gen:0"]));

    let mut appears = LatencyReport::new();
    let mut disappears = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for i in 0..ITERS {
        let gen = format!("gen:{i}");
        let present = i % 2 == 0;
        // Equal-sized states: the matching vs non-matching tag are both
        // 8 chars, so the island's appearance/disappearance is the only
        // difference — not payload size.
        let flag = if present { MATCH_TAG } else { NOMATCH_TAG };
        let caps = manifest_tags(&[flag, &gen]);
        let outcome = timed_announce_until(&a, &b, caps, || {
            // Attribute to THIS version (gen tag visible) AND assert the
            // scheduler decision matches expectation.
            let this_ver = b.find_nodes_by_filter(&require_tag(&gen)).contains(&a_id);
            let island_matched = b.match_islands(&crit).contains(&ISLAND);
            this_ver && island_matched == present
        })
        .await;

        if i >= WARMUP {
            match outcome {
                Ok(d) if present => appears.record(d.as_nanos() as u64),
                Ok(d) => disappears.record(d.as_nanos() as u64),
                Err(()) => timeouts += 1,
            }
        } else if outcome.is_err() {
            timeouts += 1;
        }
    }

    let version_delta = a.capability_announce_version() - version_before;
    let candidate_pop = b.match_islands(&crit).len();
    for (label, report) in [
        ("island appears (viable)", &appears),
        ("island disappears (pruned)", &disappears),
    ] {
        report.print_row(RowMeta {
            label,
            start_event: "publish_call (announce_capabilities)",
            endpoint: "match_islands result changed (real placement decision)",
            topology,
            hop_count,
            manifest_bytes,
            version_delta,
            candidate_pop,
            warmup: WARMUP,
            workers: WORKER_THREADS,
            topology_reused: true,
            timeouts,
            outliers: 0,
        });
    }
}

// ============================================================================
// Shared: announce then stop at an exact-state predicate (C1/C2).
// ============================================================================

async fn timed_announce_until(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
    caps: CapabilitySet,
    predicate: impl FnMut() -> bool,
) -> Result<Duration, ()> {
    let mut rx = b.capability_fold().subscribe_changes();
    let t0 = Instant::now();
    a.announce_capabilities(caps).await.expect("announce");
    match tokio::time::timeout(DEADLINE, await_capability_state(&mut rx, predicate)).await {
        Ok(()) => Ok(t0.elapsed()),
        Err(_) => Err(()),
    }
}

fn tally(
    report: &mut LatencyReport,
    timeouts: &mut u64,
    record: bool,
    outcome: Result<Duration, ()>,
) {
    match outcome {
        Ok(d) => {
            if record {
                report.record(d.as_nanos() as u64);
            }
        }
        Err(()) => *timeouts += 1,
    }
}
