//! CPB-1: capability propagation latency — publication call → remote
//! exact-state visibility. Timer starts just before
//! `announce_capabilities` (C2: an API invocation, not an internal
//! commit) and stops only after B's fold exposes the exact new state
//! via [`await_capability_state`] (C1: the watch wake alone is not
//! query-visibility). Never a poll loop.
//!
//! CPB-0 lands this as a single warm-replacement smoke (direct A→B,
//! small manifest, membership-controlling tag) proving the harness
//! stands up and records a valid distribution through the exact-state
//! endpoint. CPB-1 expands to warm-update / warm-add-remove / cold
//! publication × {direct, two-hop routed} × {small, realistic GPU};
//! CPB-3 adds the RT-3 registry-mutation modes.
//!
//! Run: `cargo bench --features net --bench capability_propagation`

#[path = "bench_mesh_pair/mod.rs"]
mod bench_mesh_pair;

use std::time::{Duration, Instant};

use bench_mesh_pair::*;

/// Iterations recorded per case, after `WARMUP` discarded iterations.
const ITERS: u64 = 200;
const WARMUP: u64 = 20;
/// Per-sample visibility deadline; an exceeded deadline is counted as a
/// timeout and the sample is dropped (never recorded as a latency).
const DEADLINE: Duration = Duration::from_secs(5);

fn main() {
    let rt = runtime();
    rt.block_on(async {
        let cfg = BenchConfig::wire_floor();
        let (a, b) = direct_pair(&cfg).await;
        let a_id = a.node_id();
        let manifest_bytes = manifest_bytes(&manifest_small("cap:0"));

        let mut report = LatencyReport::new();
        let mut timeouts = 0u64;
        let version_before = a.capability_announce_version();

        for i in 0..ITERS {
            // A genuine warm replacement: A's whole set becomes the
            // single membership-controlling tag `cap:{i}` (C9 warm
            // replace; C8 exact-state via a tag that controls membership).
            let tag = format!("cap:{i}");
            let caps = manifest_small(&tag);
            let mut rx = b.capability_fold().subscribe_changes();

            let t0 = Instant::now();
            a.announce_capabilities(caps).await.expect("announce");
            let visible = tokio::time::timeout(
                DEADLINE,
                await_capability_state(&mut rx, || {
                    b.find_nodes_by_filter(&require_tag(&tag)).contains(&a_id)
                }),
            )
            .await;
            let elapsed = t0.elapsed();

            match visible {
                Ok(()) => {
                    if i >= WARMUP {
                        report.record(elapsed.as_nanos() as u64);
                    }
                }
                Err(_) => timeouts += 1,
            }
        }

        let version_delta = a.capability_announce_version() - version_before;
        let candidate_pop = b
            .find_nodes_by_filter(&require_tag(&format!("cap:{}", ITERS - 1)))
            .len();

        println!("\n=== CPB-1 capability propagation latency (wire floor) ===\n");
        report.print_row(RowMeta {
            label: "warm replace (membership)",
            start_event: "publish_call (announce_capabilities)",
            endpoint: "exact-state read (find_nodes_by_filter)",
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
    });
}
