//! CPB-4: coalescing efficiency. TWO separate benchmark groups with
//! DIFFERENT contracts (C6) — never merged, and NOT a stale-sleeper
//! correctness guard (C7: an ordinary burst bench stays green while a
//! stale delayed sleeper still owns a newer window; that defect needs a
//! deterministic time-controlled regression test, not a benchmark. The
//! related token work lives in PR #557's RT-1/RT-4 fixes and SI-6.1's
//! fold-reconciliation token — related patterns, not this slice).
//!
//! - **RT-3 debounce group** — N rapid REGISTRY mutations (re-versions of
//!   one tool, so the registry never bloats) that all land inside the
//!   100 ms debounce window should collapse to ~one publication. The
//!   honest coalescing figure is **broadcasts the consumer actually
//!   applied** (B's capability-fold generation delta), because A's
//!   `capability_announce_version` counts announce *calls*, not wire
//!   broadcasts (D2).
//! - **RT-1 rate-limit group** — one leading EXPLICIT announce + many
//!   in-window explicit announces coalesce to ~one leading + one
//!   trailing broadcast. Again measured as broadcasts B applied
//!   (~2), vs the M+1 announce calls A made.
//!
//! Both groups also verify **final-state correctness** (B converges to
//! the exact last version) and report **logical payload bytes accepted
//! by one direct consumer** (per-announce bytes × broadcasts applied) —
//! never "bytes sent" (D2).
//!
//! Run: `cargo bench --features "net tool" --bench capability_burst`

#[path = "bench_mesh_pair/mod.rs"]
mod bench_mesh_pair;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_mesh_pair::*;
use net::adapter::net::cortex::tool::ToolDescriptor;
use net::adapter::net::MeshNode;

const ITERS: u64 = 20;
const BURSTS: [u64; 3] = [1, 16, 128];
const DEADLINE: Duration = Duration::from_secs(5);
/// RT-1 rate window — short so the trailing flush lands quickly; the
/// leading+trailing contract is independent of the exact window.
const RT1_WINDOW: Duration = Duration::from_millis(250);

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== CPB-4 coalescing efficiency (two separate groups) ===\n");
        rt3_debounce_group().await;
        println!();
        rt1_rate_limit_group().await;
    });
}

/// Accumulated over `ITERS` bursts of one size.
struct BurstStats {
    burst: u64,
    announce_calls: u64, // Σ A version delta (announce CALLS, not broadcasts)
    broadcasts: u64,     // Σ B fold-gen delta (broadcasts B actually applied)
    correct: u64,        // final-state-correct count
    per_announce_bytes: usize,
    latency: LatencyReport,
}

impl BurstStats {
    fn new(burst: u64, per_announce_bytes: usize) -> Self {
        Self {
            burst,
            announce_calls: 0,
            broadcasts: 0,
            correct: 0,
            per_announce_bytes,
            latency: LatencyReport::new(),
        }
    }

    fn print(&self, group: &str) {
        let per = |n: u64| n as f64 / ITERS as f64;
        let bytes_per_burst = self.per_announce_bytes as f64 * per(self.broadcasts);
        println!(
            "[{group}] burst={:>3}  calls/burst={:>5.1} (delta,not emissions)  \
             broadcasts_applied/burst={:>4.1}  final_correct={}/{}  \
             logical_bytes_accepted/burst={:>6.0}  conv p50={:.1}ms p99={:.1}ms",
            self.burst,
            per(self.announce_calls),
            per(self.broadcasts),
            self.correct,
            ITERS,
            bytes_per_burst,
            self.latency.quantile_us(0.50) / 1_000.0,
            self.latency.quantile_us(0.99) / 1_000.0,
        );
    }
}

// ============================================================================
// RT-3 debounce group — N rapid re-versions of ONE tool collapse to ~1 publish.
// ============================================================================

async fn rt3_debounce_group() {
    // debounce-only: the 100 ms trailing debounce is what collapses a
    // tight burst; NO rate limit so RT-1 is not also in play.
    let (a, b) = direct_pair(&BenchConfig::debounce_only()).await;
    let a_id = a.node_id();
    let per_announce_bytes = manifest_bytes(&manifest_tags(&["ai-tool:bench.tool"]));

    for &burst in &BURSTS {
        let mut stats = BurstStats::new(burst, per_announce_bytes);
        for k in 0..ITERS {
            let v0 = a.capability_announce_version();
            let g0 = b.capability_fold().change_generation();
            let final_version = format!("{k}.{}", burst - 1);
            let mut rx = b.capability_fold().subscribe_changes();

            let t0 = Instant::now();
            // N rapid mutations to ONE tool_id (re-versioned) — the registry
            // stays size 1, no fold bloat; every insert fires the local-caps
            // change signal that the debouncer coalesces.
            for j in 0..burst {
                a.tool_registry()
                    .insert(tool_descriptor("bench.tool", &format!("{k}.{j}")));
            }
            let converged = tokio::time::timeout(
                DEADLINE,
                await_capability_state(&mut rx, || {
                    has_tool_version(&b, "bench.tool", &final_version)
                }),
            )
            .await
            .is_ok();

            if converged {
                stats.latency.record(t0.elapsed().as_nanos() as u64);
                stats.correct += 1;
            }
            stats.announce_calls += a.capability_announce_version() - v0;
            stats.broadcasts += b.capability_fold().change_generation() - g0;
            let _ = a_id;
        }
        stats.print("RT-3 debounce");
    }
    drop((a, b));
}

/// Does B's aggregated tool view carry `tool_id` at exactly `version`?
fn has_tool_version(b: &Arc<MeshNode>, tool_id: &str, version: &str) -> bool {
    b.list_tools(None)
        .iter()
        .any(|d| d.tool_id == tool_id && d.version == version)
}

// ============================================================================
// RT-1 rate-limit group — 1 leading + M in-window explicit announces -> ~2.
// ============================================================================

async fn rt1_rate_limit_group() {
    let cfg = BenchConfig {
        sensing_coalescing: false,
        announce_debounce: Some(Duration::ZERO), // isolate the rate limiter
        min_announce_interval: Some(RT1_WINDOW),
    };
    let (a, b) = direct_pair(&cfg).await;
    let a_id = a.node_id();
    let per_announce_bytes = manifest_bytes(&manifest_tags(&["burst:svc", "burst:0.0"]));

    for &burst in &BURSTS {
        let mut stats = BurstStats::new(burst, per_announce_bytes);
        for k in 0..ITERS {
            // Clear the rate window from the prior iteration's trailing flush,
            // so this iteration's first announce is a genuine leading edge.
            tokio::time::sleep(RT1_WINDOW + Duration::from_millis(20)).await;

            let v0 = a.capability_announce_version();
            let g0 = b.capability_fold().change_generation();
            let final_tag = format!("burst:{k}.{}", burst - 1);
            let mut rx = b.capability_fold().subscribe_changes();

            let t0 = Instant::now();
            // Leading announce (immediate) + M in-window announces (coalesced
            // into one trailing flush). Each carries a distinct final tag.
            for j in 0..burst {
                a.announce_capabilities(manifest_tags(&["burst:svc", &format!("burst:{k}.{j}")]))
                    .await
                    .expect("announce");
            }
            let converged = tokio::time::timeout(
                DEADLINE,
                await_capability_state(&mut rx, || {
                    b.find_nodes_by_filter(&require_tag(&final_tag))
                        .contains(&a_id)
                }),
            )
            .await
            .is_ok();

            if converged {
                stats.latency.record(t0.elapsed().as_nanos() as u64);
                stats.correct += 1;
            }
            stats.announce_calls += a.capability_announce_version() - v0;
            stats.broadcasts += b.capability_fold().change_generation() - g0;
        }
        stats.print("RT-1 rate-limit");
    }
    drop((a, b));
}

// ============================================================================

fn tool_descriptor(tool_id: &str, version: &str) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_string(),
        name: "bench tool".to_string(),
        version: version.to_string(),
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
