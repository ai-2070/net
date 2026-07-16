//! ICB-1 — ordinary matcher scaling.
//!
//! A converged, STATIC in-process fixture → `match_islands` → ranked
//! viable island IDs. No claim, CAS, reservation, sensed readiness, or
//! transport: this measures ONLY the read-only match pipeline
//! (`MeshNode::match_islands` = gang §2 steps 1–3) as the candidate
//! population scales.
//!
//! Matrix: island population {10, 100, 1000} × units/island {1, 8, 72} ×
//! capability shape {sparse, dense}.
//!
//! Boundary discipline (Kyra ICB-1): **`min_units` is an ELIGIBILITY
//! constraint; any later successful CLAIM reserves the whole island. The
//! matcher reserves nothing.** Reported island populations are the three
//! distinct pipeline stages: island population (seeded) → candidate
//! islands before numeric filtering (bench-reconstructed) → viable
//! islands returned. Sparse/dense are defined NUMERICALLY in the row.
//!
//! Population discipline: the exact matched-host / island / candidate-
//! island counts are asserted BEFORE and AFTER every timed batch; a
//! mismatch FAILS the row instead of printing a distribution. Fixtures
//! carry an explicit long TTL so nothing expires mid-timing.
//!
//! No threshold or performance claim here — that is ICB-7.
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_match --features net`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use bench_island_claim::{node, runtime, LatencyReport, WORKER_THREADS};
// The matcher's coarse query uses the fold-level `CapabilityFilter`
// (`CapabilityQuery::Composite`); `find_nodes_by_filter` (the
// bench-reconstruction of matched hosts) takes the behavior-level one.
use net::adapter::net::behavior::capability::CapabilityFilter as QueryFilter;
use net::adapter::net::behavior::fold::{
    CapabilityFilter as FoldCapFilter, CapabilityFold, CapabilityMembership, CapabilityQuery,
    EnvelopeMeta, FoldKind, IslandQuery, IslandRecord, IslandTopologyFold, NodeState,
    SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
use net::adapter::net::MeshNode;

/// The capability tag that makes a host a candidate for the matcher.
const MATCH_TAG: &str = "gpu:h100";
/// A non-matching tag of equal length — the "not a candidate" host shape,
/// so payload size never confounds the sparse/dense axis.
const NOMATCH_TAG: &str = "cpu:none0";
/// Fixed capability class for every seeded host.
const CLASS: u64 = 0;
/// Eligibility threshold: an island needs ≥ this many units to be viable.
/// (Eligibility only — the matcher never reserves.)
const MIN_UNITS: usize = 8;
/// Host node-id base, offset from island ids to keep the two id spaces
/// visually distinct.
const HOST_BASE: u64 = 1_000_000;
/// Explicit long fixture TTL so the fold sweeper never removes a seeded
/// entry inside a timed batch (Kyra population discipline).
const FIXTURE_TTL_SECS: u32 = 3_600;
/// Sparse fixture: 1 in [`SPARSE_DEN`] hosts carries [`MATCH_TAG`].
const SPARSE_DEN: usize = 10;

const ITERS: u64 = 500;
const WARMUP: u64 = 50;

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-1 ordinary matcher scaling (match_islands) ===\n");
        for &islands in &[10usize, 100, 1000] {
            for &units in &[1u32, 8, 72] {
                for dense in [false, true] {
                    run_cell(islands, units, dense).await;
                }
            }
        }
    });
}

/// The three-stage populations reconstructed from the folds — the values
/// asserted stable across a timed batch (Kyra population discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Populations {
    matched_hosts: usize,
    candidate_islands: usize, // hosted-by a matched host, PRE numeric filter
    island_pop: usize,        // total seeded islands
    viable: usize,            // match_islands result length
}

async fn run_cell(islands: usize, units: u32, dense: bool) {
    let node = node().await; // unstarted: match_islands is a pure fold read
    let kp = net::adapter::net::EntityKeypair::generate();

    // Seed one host + one island per index; a matched host carries
    // MATCH_TAG (dense: every host; sparse: every SPARSE_DEN-th).
    for i in 0..islands {
        let host_id = HOST_BASE + i as u64;
        let matched = dense || i % SPARSE_DEN == 0;
        let tags = vec![if matched { MATCH_TAG } else { NOMATCH_TAG }.to_string()];
        seed_host(&node, &kp, host_id, tags);
        seed_island(&node, &kp, i as u64, units, host_id);
    }

    let crit = criteria();
    let filter = query_filter();

    // Expected populations from the fixture definition.
    let expected_matched = if dense {
        islands
    } else {
        islands.div_ceil(SPARSE_DEN)
    };
    let expected_viable = if units as usize >= MIN_UNITS {
        expected_matched
    } else {
        0
    };
    let expected = Populations {
        matched_hosts: expected_matched,
        candidate_islands: expected_matched, // one island per matched host
        island_pop: islands,
        viable: expected_viable,
    };

    // Population assertion BEFORE the timed batch.
    let before = reconstruct(&node, &crit, &filter);
    assert_populations(before, expected, islands, units, dense, "before");

    // Timed batch: the read-only match pipeline.
    let mut report = LatencyReport::new();
    for i in 0..ITERS {
        let t0 = Instant::now();
        let viable = node.match_islands(&crit);
        let dt = t0.elapsed();
        std::hint::black_box(&viable);
        if i >= WARMUP {
            report.record(dt.as_nanos() as u64);
        }
    }

    // Population assertion AFTER — fixtures must be byte-static.
    let after = reconstruct(&node, &crit, &filter);
    assert_eq!(
        before, after,
        "fixture drifted during timing (islands={islands} units={units} dense={dense}): {before:?} -> {after:?}"
    );
    assert_populations(after, expected, islands, units, dense, "after");

    print_row(&report, islands, units, dense, expected);
}

// ============================================================================
// Fixture seeding (in-process fold applies; excluded from timing).
// ============================================================================

fn fixture_meta() -> EnvelopeMeta {
    EnvelopeMeta {
        announced_at: 0,
        ttl_secs: Some(FIXTURE_TTL_SECS),
        flags: 0,
    }
}

/// Seed one capability host (`node_id = host_id`) carrying `tags`, Idle.
fn seed_host(
    node: &Arc<MeshNode>,
    kp: &net::adapter::net::EntityKeypair,
    host_id: u64,
    tags: Vec<String>,
) {
    let ann = SignedAnnouncement::sign(
        kp,
        CapabilityFold::KIND_ID,
        CLASS,
        host_id,
        1,
        fixture_meta(),
        CapabilityMembership {
            class_hash: CLASS,
            tags,
            hardware: None,
            state: NodeState::Idle,
            region: None,
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata: BTreeMap::new(),
        },
    )
    .expect("sign host capability");
    node.capability_fold()
        .apply(ann)
        .expect("apply host capability");
}

/// Seed one island of `units` units hosted by `host_id`.
fn seed_island(
    node: &Arc<MeshNode>,
    kp: &net::adapter::net::EntityKeypair,
    island_id: u64,
    units: u32,
    host_id: u64,
) {
    let record = IslandRecord {
        id: island_id,
        units: UnitSet::new((0..units).collect()),
        host: host_id,
        capabilities: Vec::new(),
        load: 0.5,
        p50_latency_us: 1_500,
    };
    let ann = SignedAnnouncement::sign(
        kp,
        IslandTopologyFold::KIND_ID,
        CLASS,
        host_id,
        1,
        fixture_meta(),
        record,
    )
    .expect("sign island");
    node.island_fold().apply(ann).expect("apply island");
}

// ============================================================================
// Criteria + reconstruction.
// ============================================================================

/// Fold-level filter for the matcher's coarse capability query.
fn match_filter() -> FoldCapFilter {
    FoldCapFilter {
        tags_all: vec![MATCH_TAG.to_string()],
        ..Default::default()
    }
}

/// Behavior-level filter for `find_nodes_by_filter` (matched-host
/// reconstruction) — selects the same hosts as [`match_filter`].
fn query_filter() -> QueryFilter {
    QueryFilter::new().require_tag(MATCH_TAG.to_string())
}

fn criteria() -> MatchCriteria {
    MatchCriteria {
        capability: CapabilityQuery::Composite(match_filter()),
        numeric: NumericFilter {
            min_units: MIN_UNITS,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    }
}

/// Reconstruct the three pipeline populations from the folds, exactly the
/// way the matcher reads them (matched hosts → candidate islands →
/// viable). "candidate islands" is bench-reconstructed from the same
/// `HostedByAny` query the matcher's step [2] runs, BEFORE the numeric
/// filter.
fn reconstruct(node: &Arc<MeshNode>, crit: &MatchCriteria, filter: &QueryFilter) -> Populations {
    let matched_hosts = node.find_nodes_by_filter(filter);
    let hosts_set: HashSet<u64> = matched_hosts.iter().copied().collect();
    let candidate_islands = node
        .island_fold()
        .query(IslandQuery::HostedByAny(hosts_set))
        .len();
    let island_pop = node.island_fold().query(IslandQuery::All).len();
    let viable = node.match_islands(crit).len();
    Populations {
        matched_hosts: matched_hosts.len(),
        candidate_islands,
        island_pop,
        viable,
    }
}

fn assert_populations(
    got: Populations,
    expected: Populations,
    islands: usize,
    units: u32,
    dense: bool,
    when: &str,
) {
    assert_eq!(
        got, expected,
        "population mismatch {when} timing (islands={islands} units={units} dense={dense})"
    );
}

// ============================================================================
// Reporting (metadata + distribution; NO threshold / perf claim — ICB-7).
// ============================================================================

fn print_row(report: &LatencyReport, islands: usize, units: u32, dense: bool, pop: Populations) {
    // Sparse/dense defined numerically, not qualitatively.
    let density = if dense {
        "dense: 100% of hosts match (fraction=1.0)".to_string()
    } else {
        format!(
            "sparse: 1/{SPARSE_DEN} of hosts match (fraction={:.2}, every {SPARSE_DEN}th)",
            1.0 / SPARSE_DEN as f64
        )
    };
    println!(
        "── ICB-1 · islands={islands} · units={units} · {} ──",
        if dense { "dense" } else { "sparse" }
    );
    println!(
        "   island_pop={} · matched_hosts={} · candidate_islands(pre-numeric, bench-reconstructed)={} · eligible_pop={} · viable_returned={}",
        pop.island_pop, pop.matched_hosts, pop.candidate_islands, pop.candidate_islands, pop.viable,
    );
    println!(
        "   min_units={MIN_UNITS} (ELIGIBILITY only — a later successful CLAIM reserves the whole island; the matcher reserves nothing) · selection=LeastLoaded",
    );
    println!("   {density}");
    println!(
        "   samples={} · workers={WORKER_THREADS} · timeouts=0 (sync in-process match; no wait)",
        report.samples(),
    );
    println!(
        "   p50={:.2}us p95={:.2}us p99={:.2}us max={:.2}us",
        report.quantile_us(0.50),
        report.quantile_us(0.95),
        report.quantile_us(0.99),
        report.quantile_us(1.0),
    );
    println!();
}
