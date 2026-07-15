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
#[cfg(feature = "tool")]
use net::adapter::net::cortex::tool::ToolDescriptor;
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
        // CPB-5: fan-out + intake, batch-completion semantics.
        fan_out_and_intake().await;
        // CPB-3: RT-3 registry-driven convergence (feature "tool").
        #[cfg(feature = "tool")]
        rt3_registry_convergence().await;
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

// ============================================================================
// CPB-5 — fan-out all-visible + intake all-visible (batch completion, C12).
// ============================================================================

const TOPO_N: usize = 16;
const TOPO_ITERS: u64 = 50;

async fn fan_out_and_intake() {
    println!("=== CPB-5 topology matrix (batch completion) ===\n");
    fanout_all_visible().await;
    intake_all_visible().await;
}

/// A → 16 consumers. Report first-consumer-visible, all-16-visible, and
/// the per-consumer distribution (C12: never treat the first wake as
/// completion).
async fn fanout_all_visible() {
    let (a, consumers) = fan_out(&BenchConfig::wire_floor(), TOPO_N).await;
    let a_id = a.node_id();
    let manifest_bytes = manifest_bytes(&manifest_tags(&["fanout:svc", "gen:0"]));

    let mut per_consumer = LatencyReport::new();
    let mut first_visible = LatencyReport::new();
    let mut all_visible = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for k in 0..TOPO_ITERS {
        let tag = format!("gen:{k}");
        // Subscribe every consumer BEFORE the announce (no missed wakes).
        let subs: Vec<_> = consumers
            .iter()
            .map(|c| c.capability_fold().subscribe_changes())
            .collect();
        let t0 = Instant::now();
        a.announce_capabilities(manifest_tags(&["fanout:svc", &tag]))
            .await
            .expect("announce");
        // Await each consumer's exact-state concurrently; each task records
        // its own elapsed at its own completion.
        let mut handles = Vec::with_capacity(consumers.len());
        for (c, mut rx) in consumers.iter().cloned().zip(subs) {
            let tag = tag.clone();
            handles.push(tokio::spawn(async move {
                let ok = tokio::time::timeout(
                    DEADLINE,
                    await_capability_state(&mut rx, || {
                        c.find_nodes_by_filter(&require_tag(&tag)).contains(&a_id)
                    }),
                )
                .await
                .is_ok();
                (t0.elapsed(), ok)
            }));
        }
        let mut elapsed = Vec::with_capacity(consumers.len());
        for h in handles {
            let (d, ok) = h.await.expect("join");
            if ok {
                elapsed.push(d);
            }
        }
        if elapsed.len() == consumers.len() {
            for d in &elapsed {
                per_consumer.record(d.as_nanos() as u64);
            }
            first_visible.record(elapsed.iter().min().expect("min").as_nanos() as u64);
            all_visible.record(elapsed.iter().max().expect("max").as_nanos() as u64);
        } else {
            timeouts += 1;
        }
    }

    let version_delta = a.capability_announce_version() - version_before;
    let row = |label: &'static str| RowMeta {
        label,
        start_event: "publish_call (announce_capabilities)",
        endpoint: "exact-state read per consumer (find_nodes_by_filter)",
        topology: "A->16 consumers",
        hop_count: 0,
        manifest_bytes,
        version_delta,
        candidate_pop: consumers.len(),
        warmup: 0,
        workers: WORKER_THREADS,
        topology_reused: true,
        timeouts,
        outliers: 0,
    };
    per_consumer.print_row(row("fan-out per-consumer"));
    first_visible.print_row(row("fan-out first-visible"));
    all_visible.print_row(row("fan-out all-16-visible"));
    drop((a, consumers));
}

/// 16 providers → B. Endpoint is ALL 16 exact provider versions visible
/// (C12: one watch generation can coalesce several changes, so the first
/// wake is NOT completion), then the candidate population equals 16.
async fn intake_all_visible() {
    let (b, providers) = intake(&BenchConfig::wire_floor(), TOPO_N).await;
    let manifest_bytes = manifest_bytes(&manifest_tags(&["intake:svc", "p0.k0"]));
    let mut all_visible = LatencyReport::new();
    let mut timeouts = 0u64;
    let mut population = 0usize;

    for k in 0..TOPO_ITERS {
        let mut rx = b.capability_fold().subscribe_changes();
        let t0 = Instant::now();
        // Concurrent announces from all providers — intake pressure.
        let mut ann = Vec::with_capacity(providers.len());
        for (i, p) in providers.iter().cloned().enumerate() {
            let caps = manifest_tags(&["intake:svc", &format!("p{i}.k{k}")]);
            ann.push(tokio::spawn(async move {
                let _ = p.announce_capabilities(caps).await;
            }));
        }
        for h in ann {
            let _ = h.await;
        }
        let ok = tokio::time::timeout(
            DEADLINE,
            await_capability_state(&mut rx, || {
                (0..TOPO_N).all(|i| {
                    !b.find_nodes_by_filter(&require_tag(&format!("p{i}.k{k}")))
                        .is_empty()
                })
            }),
        )
        .await
        .is_ok();
        if ok {
            all_visible.record(t0.elapsed().as_nanos() as u64);
        } else {
            timeouts += 1;
        }
        population = b.find_nodes_by_filter(&require_tag("intake:svc")).len();
    }

    all_visible.print_row(RowMeta {
        label: "intake all-16-visible",
        start_event: "16x publish_call (concurrent)",
        endpoint: "all 16 exact provider versions visible (batch completion)",
        topology: "16 providers->B",
        hop_count: 0,
        manifest_bytes,
        version_delta: 0, // per-provider; not a single origin counter
        candidate_pop: population,
        warmup: 0,
        workers: WORKER_THREADS,
        topology_reused: true,
        timeouts,
        outliers: 0,
    });
    drop((b, providers));
}

// ============================================================================
// CPB-3 — RT-3 registry mutation -> remote visibility, two policy modes
// (C4/C5). Driven by a real tool-registry mutation (tool_registry().insert),
// which fires the local-caps change signal and settles through start_arc()'s
// change-driven auto-announcer — NOT an explicit announce. So the measured
// latency INCLUDES the RT-3 debounce (and, in default-policy, the rate-limit
// floor). Feature "tool"; otherwise this whole section is cfg'd out and the
// bench stays a pure --features net wire-floor suite.
// ============================================================================

/// Longer deadline: default-policy's rate-limit floor can push a trailing
/// flush toward `min_announce_interval` (10 s default).
#[cfg(feature = "tool")]
const DEADLINE_RT3: Duration = Duration::from_secs(20);

#[cfg(feature = "tool")]
async fn rt3_registry_convergence() {
    println!("=== CPB-3 RT-3 registry mutation -> remote visibility ===\n");
    // debounce-only: isolates RT-3 burst settling (100 ms debounce, no rate limit).
    run_rt3("RT-3 debounce-only", &BenchConfig::debounce_only(), 40, 5).await;
    // default-policy: rate-limit-dominated. A SMALL labeled scenario — thousands
    // of ~10 s samples are impractical (C5). NOT "production defaults".
    run_rt3(
        "RT-3 default-policy (min_interval 10s)",
        &BenchConfig::default_policy(),
        3,
        0,
    )
    .await;
}

#[cfg(feature = "tool")]
async fn run_rt3(label: &str, cfg: &BenchConfig, iters: u64, warmup: u64) {
    let (a, b) = direct_pair(cfg).await;
    let a_id = a.node_id();
    let mut report = LatencyReport::new();
    let mut timeouts = 0u64;
    let version_before = a.capability_announce_version();

    for i in 0..iters {
        let tool_id = format!("bench.tool.{i}");
        let tag = format!("ai-tool:{tool_id}");
        let mut rx = b.capability_fold().subscribe_changes();
        let descriptor = tool_descriptor(&tool_id);
        let t0 = Instant::now();
        // The RT-3 trigger: a registry mutation, not an explicit announce. The
        // change-driven announcer (running because of start_arc) debounces then
        // announces; the measured latency includes that policy window.
        a.tool_registry().insert(descriptor);
        let visible = tokio::time::timeout(
            DEADLINE_RT3,
            await_capability_state(&mut rx, || {
                b.find_nodes_by_filter(&require_tag(&tag)).contains(&a_id)
            }),
        )
        .await;
        match visible {
            Ok(()) => {
                if i >= warmup {
                    report.record(t0.elapsed().as_nanos() as u64);
                }
            }
            Err(_) => timeouts += 1,
        }
    }

    let candidate_pop = b
        .find_nodes_by_filter(&require_tag(&format!("ai-tool:bench.tool.{}", iters - 1)))
        .len();
    report.print_row(RowMeta {
        label,
        start_event: "registry mutation (tool_registry.insert)",
        endpoint: "exact-state read (ai-tool tag via find_nodes_by_filter)",
        topology: "A->B direct",
        hop_count: 0,
        manifest_bytes: manifest_bytes(&manifest_tags(&["ai-tool:bench.tool.0"])),
        version_delta: a.capability_announce_version() - version_before,
        candidate_pop,
        warmup,
        workers: WORKER_THREADS,
        topology_reused: true,
        timeouts,
        outliers: 0,
    });
    drop((a, b));
}

#[cfg(feature = "tool")]
fn tool_descriptor(tool_id: &str) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_string(),
        name: "bench tool".to_string(),
        version: "1.0".to_string(),
        description: None,
        input_schema: None,
        output_schema: None,
        requires: Vec::new(),
        estimated_time_ms: 0,
        stateless: true,
        streaming: false,
        tags: Vec::new(),
        pricing_terms: None,
        node_count: 0,
    }
}
