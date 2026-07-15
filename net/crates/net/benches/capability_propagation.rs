//! CPB-1: capability propagation latency — publication call → remote
//! exact-state visibility. Timer starts just before
//! `announce_capabilities` (C2: an API invocation, not an internal
//! commit) and stops only after B's fold exposes the exact new state
//! via [`await_capability_state`] (C1: the watch wake alone is not
//! query-visibility). Never a poll loop.
//!
//! The matrix (C8 exact-state per operation, C9 cold/warm separation):
//!
//! - **warm update** — service membership preserved; a per-iteration
//!   discriminator tag `gen:{i}` proves the exact new version landed
//!   (an exact-tag assertion, since `CapabilityMembership` carries no
//!   per-publisher version field). {direct, routed} × {small, GPU}.
//! - **warm add / remove** — alternate equal-sized states to flip a
//!   target tag's membership; `find_nodes_by_filter` suffices (the tag
//!   controls membership). Reported as two distributions.
//! - **cold publication** — a fresh established pair per sample (setup
//!   excluded from timing); the first fold insert of the origin on B.
//!   Small N, topology NOT reused.
//!
//! Routed rows are reported separately (the additive hop cost).
//! Acceptance (C11) is valid distributions + exact-state correctness +
//! zero timeouts, NOT a latency threshold. CPB-3 adds the RT-3
//! registry-mutation modes.
//!
//! Run: `cargo bench --features net --bench capability_propagation`

#[path = "bench_mesh_pair/mod.rs"]
mod bench_mesh_pair;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_mesh_pair::*;
use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::MeshNode;

const ITERS: u64 = 200;
const WARMUP: u64 = 20;
const COLD_SAMPLES: u64 = 30;
/// Per-sample visibility deadline; an exceeded deadline is counted as a
/// timeout and the sample is dropped (never recorded as a latency).
const DEADLINE: Duration = Duration::from_secs(5);

/// The stable service tag whose membership an *update* preserves.
const SVC: &str = "svc:print";

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== CPB-1 capability propagation latency (wire floor) ===\n");
        warm_update_matrix().await;
        warm_add_remove_direct().await;
        cold_publication_direct().await;
    });
}

// ============================================================================
// Timed sample — announce then stop at the exact-state read (C1/C2).
// ============================================================================

/// Announce `caps` on `a`, then stop the timer once `predicate` (an
/// exact-state read on B) holds. `Err` = visibility deadline exceeded.
async fn timed_sample(
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

/// Record a sample outcome: a good sample into `report` (only when
/// `record`), a deadline miss into `timeouts`.
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

// ============================================================================
// warm update — {direct, routed} × {small, GPU}.
// ============================================================================

async fn warm_update_matrix() {
    // direct
    let (a, b) = direct_pair(&BenchConfig::wire_floor()).await;
    run_update(&a, &b, "warm update · small", "A->B direct", 0, false).await;
    run_update(&a, &b, "warm update · GPU", "A->B direct", 0, true).await;
    drop((a, b));

    // routed A->R->B (relay forwards; warm() asserts convergence)
    let (a, r, b) = routed_chain(&BenchConfig::wire_floor()).await;
    run_update(&a, &b, "warm update · small", "A->R->B routed", 1, false).await;
    run_update(&a, &b, "warm update · GPU", "A->R->B routed", 1, true).await;
    drop((a, r, b));
}

async fn run_update(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
    label: &str,
    topology: &str,
    hop_count: u32,
    gpu: bool,
) {
    let a_id = a.node_id();
    let manifest_bytes = if gpu {
        manifest_bytes(&manifest_realistic_gpu("gen:0").add_tag(SVC))
    } else {
        manifest_bytes(&manifest_tags(&[SVC, "gen:0"]))
    };

    let mut report = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for i in 0..ITERS {
        let gen = format!("gen:{i}");
        // Service membership (SVC) is preserved across every iteration;
        // the gen tag is the version discriminator.
        let caps = if gpu {
            manifest_realistic_gpu(&gen).add_tag(SVC)
        } else {
            manifest_tags(&[SVC, &gen])
        };
        let outcome = timed_sample(a, b, caps, || {
            b.find_nodes_by_filter(&require_tag(&gen)).contains(&a_id)
        })
        .await;
        tally(&mut report, &mut timeouts, i >= WARMUP, outcome);
    }

    let version_delta = a.capability_announce_version() - version_before;
    let candidate_pop = b.find_nodes_by_filter(&require_tag(SVC)).len();
    report.print_row(RowMeta {
        label,
        start_event: "publish_call (announce_capabilities)",
        endpoint: "exact-state read (discriminator tag via find_nodes_by_filter)",
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

// ============================================================================
// warm add / remove — flip a target tag's membership (equal-sized states).
// ============================================================================

async fn warm_add_remove_direct() {
    let (a, b) = direct_pair(&BenchConfig::wire_floor()).await;
    let a_id = a.node_id();
    let manifest_bytes = manifest_bytes(&manifest_tags(&[SVC, "gen:0", "avail"]));

    let mut add = LatencyReport::new();
    let mut remove = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for i in 0..ITERS {
        let gen = format!("gen:{i}");
        let present = i % 2 == 0;
        // Equal-sized states: "avail" (present) vs "unavl" (absent) — both
        // 5 chars — so payload size is not a confound between add/remove.
        let flag = if present { "avail" } else { "unavl" };
        let caps = manifest_tags(&[SVC, &gen, flag]);
        // Exact state: this iteration's gen tag is visible AND "avail"
        // membership matches the expectation.
        let outcome = timed_sample(&a, &b, caps, || {
            let this_version = b.find_nodes_by_filter(&require_tag(&gen)).contains(&a_id);
            let available = b
                .find_nodes_by_filter(&require_tag("avail"))
                .contains(&a_id);
            this_version && available == present
        })
        .await;
        // First WARMUP iterations discarded; then route each sample to its
        // add/remove distribution.
        if i >= WARMUP {
            match outcome {
                Ok(d) if present => add.record(d.as_nanos() as u64),
                Ok(d) => remove.record(d.as_nanos() as u64),
                Err(()) => timeouts += 1,
            }
        } else if outcome.is_err() {
            timeouts += 1;
        }
    }

    let version_delta = a.capability_announce_version() - version_before;
    let candidate_pop = b.find_nodes_by_filter(&require_tag(SVC)).len();
    for (label, report) in [
        ("warm add (tag appears)", &add),
        ("warm remove (tag drops)", &remove),
    ] {
        report.print_row(RowMeta {
            label,
            start_event: "publish_call (announce_capabilities)",
            endpoint: "exact-state read (target-tag membership via find_nodes_by_filter)",
            topology: "A->B direct",
            hop_count: 0,
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
    drop((a, b));
}

// ============================================================================
// cold publication — fresh established pair per sample (C9).
// ============================================================================

async fn cold_publication_direct() {
    let manifest_bytes = manifest_bytes(&manifest_small("cold:0"));
    let mut report = LatencyReport::new();
    let mut timeouts = 0u64;

    for s in 0..COLD_SAMPLES {
        // Fresh pair: handshake/pin paid in established_pair, OUTSIDE the
        // timed region. The first announce below is a cold fold insert.
        let (a, b) = established_pair(&BenchConfig::wire_floor()).await;
        let a_id = a.node_id();
        let tag = format!("cold:{s}");
        let caps = manifest_small(&tag);
        let outcome = timed_sample(&a, &b, caps, || {
            b.find_nodes_by_filter(&require_tag(&tag)).contains(&a_id)
        })
        .await;
        tally(&mut report, &mut timeouts, true, outcome);
        drop((a, b));
    }

    report.print_row(RowMeta {
        label: "cold publication (first insert)",
        start_event: "publish_call (announce_capabilities)",
        endpoint: "exact-state read (find_nodes_by_filter)",
        topology: "A->B direct",
        hop_count: 0,
        manifest_bytes,
        version_delta: 1, // one publish per fresh origin
        candidate_pop: 1,
        warmup: 0,
        workers: WORKER_THREADS,
        topology_reused: false, // fresh pair per sample
        timeouts,
        outliers: 0,
    });
}
