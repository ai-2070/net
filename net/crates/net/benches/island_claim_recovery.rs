//! ICB-6 — deadline-enabled takeover + runtime-entry expiry.
//!
//! Two SEPARATELY LABELED recovery groups (E6), plus the M5 short-TTL
//! diagnostic. They are DIFFERENT mechanisms and are never conflated:
//!
//! # Group 1 — deadline-enabled takeover (NOT automatic reclaim)
//!
//! A `Reserved{H, until = D}` deadline passing fires NO event — nothing in the
//! fold reclaims the island when `D` lapses (`reservation.rs`: the deadline is
//! only read at the sole `reservation_expired` call site, inside the
//! cross-publisher merge). The island changes holder ONLY when a foreign
//! claimant actively RE-CLAIMS after `D`: its optimistic CAS then legally
//! Replaces the expired reservation (`new_holder == publisher && expired` →
//! `Won`). Three distinct quantities, never merged:
//!   - configured deadline wait `D` — POLICY (a fixture choice, reported as a
//!     separate column, never a measured mechanism);
//!   - first foreign takeover CAS returns `Won` — MECHANISM;
//!   - a direct observer reads the new holder — VISIBILITY.
//!
//! # Group 2 — runtime-entry expiry (M5 short-TTL diagnostic)
//!
//! An announcement's entry TTL (`EnvelopeMeta.ttl_secs`, or the 30 s
//! `ReservationFold::DEFAULT_TTL`) plus the 500 ms background sweep
//! (`DEFAULT_SWEEP_INTERVAL`) → the entry becomes ABSENT from a fold. The reap
//! fires the LOCAL change watch; removal is LOCAL, not broadcast — each
//! observer sweeps its OWN fold independently. The default 30 s case is too
//! slow for a distribution (small sample, no p99), so this is the explicit
//! short-TTL diagnostic (M5): sign a fresh-island reservation with
//! `ttl_secs = Some(short)`, apply it locally, broadcast it, and await exact
//! absence at BOTH the origin and a remote observer — proving the two sweeps
//! are independent. Bench-only; production defaults are unchanged.
//!
//! The entry TTL is DISTINCT from the reservation deadline: after `D` lapses the
//! entry is still PRESENT (deadline passing ≠ entry removal); the entry is
//! removed only when its TTL lapses (W2 vs W3).
//!
//! Single-claimant / single-taker throughout; no arbitration, quorum, or
//! fencing; ICB-3's distributed-contention result is unchanged; localhost
//! MECHANISM/orientation evidence, no threshold (ICB-7).
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_recovery --features net`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_island_claim::{
    await_reservation_free, await_reservation_holder, far_deadline, full_mesh, holder_of, now_us,
    runtime, LatencyReport, WORKER_THREADS,
};
use net::adapter::net::behavior::fold::{
    EnvelopeMeta, FoldKind, ReservationAnnouncement, ReservationFold, ReservationState,
    SignedAnnouncement,
};
use net::adapter::net::behavior::gang::ClaimOutcome;
use net::adapter::net::{EntityKeypair, MeshNode};

/// Island id bases (distinct spaces per group; fresh id per sample).
const TAKEOVER_BASE: u64 = 0x6C00_1000;
const EXPIRY_BASE: u64 = 0x6C00_2000;
const W_TAKEOVER_BASE: u64 = 0x6C00_0100;
const W_EXPIRY_BASE: u64 = 0x6C00_0200;

/// Configured reservation deadline offset — POLICY (how long a takeover must
/// wait), NOT a measured mechanism.
const DEADLINE_OFFSET_US: u64 = 120_000; // 120 ms
/// Margin past the deadline so `reservation_expired` (now >= until) is true.
const DEADLINE_MARGIN: Duration = Duration::from_millis(15);
/// Configured entry TTL for the M5 short-TTL diagnostic — POLICY. `ttl_secs` is
/// whole seconds, so 1 s is the shortest override.
const SHORT_TTL_SECS: u32 = 1;

const DELIVERY_DEADLINE: Duration = Duration::from_secs(2);
const REMOTE_DEADLINE: Duration = Duration::from_secs(2);
/// Ceiling for exact absence: entry TTL + one sweep interval + margin.
const ABSENCE_DEADLINE: Duration = Duration::from_secs(3);

const SAMPLES_TAKEOVER: u64 = 20;
const WARMUP_TAKEOVER: u64 = 3;
/// Small sample — the short-TTL diagnostic is TTL-bound (no p99).
const SAMPLES_EXPIRY: u64 = 6;

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-6 deadline takeover + runtime-entry expiry ===\n");

        // H=nodes[0], C/taker=nodes[1], O/observer=nodes[2]; full direct mesh.
        let nodes = full_mesh(3).await;

        println!("-- witnesses --");
        w1_deadline_gates_takeover(&nodes).await;
        w2_deadline_is_not_automatic_reclaim(&nodes).await;
        w3_short_ttl_entry_expires_locally(&nodes).await;
        w4_local_and_remote_expiry_are_independent(&nodes).await;

        println!("\n-- measurement --");
        measure_deadline_takeover(&nodes).await;
        measure_runtime_expiry(&nodes).await;
    });
}

// ============================================================================
// Fixtures.
// ============================================================================

/// A signed `Reserved{holder = kp}` for `island` at `generation` with an
/// EXPLICIT entry TTL (`ttl_secs`) and reservation deadline (`until_unix_us`).
/// The harness `reserve_ann` uses the default (30 s) entry TTL; this is the M5
/// short-TTL override path (bench-only).
fn short_ttl_reservation(
    kp: &EntityKeypair,
    island: u64,
    generation: u64,
    ttl_secs: u32,
    until_unix_us: u64,
) -> SignedAnnouncement<ReservationAnnouncement> {
    SignedAnnouncement::sign(
        kp,
        ReservationFold::KIND_ID,
        0,
        kp.node_id(),
        generation,
        EnvelopeMeta {
            announced_at: now_us(),
            ttl_secs: Some(ttl_secs),
            flags: 0,
        },
        ReservationAnnouncement {
            resource_id: island,
            state: ReservationState::Reserved {
                holder: kp.node_id(),
                until_unix_us,
            },
        },
    )
    .expect("sign short-ttl reservation")
}

/// Sleep until the wall clock is strictly past `deadline_us` (+ margin), so a
/// subsequent takeover sees the reservation as expired.
async fn wait_past_deadline(deadline_us: u64) {
    let now = now_us();
    let remaining = Duration::from_micros(deadline_us.saturating_sub(now));
    tokio::time::sleep(remaining + DEADLINE_MARGIN).await;
    assert!(now_us() >= deadline_us, "clock must be past the deadline");
}

/// Await `node` seeing `island` held by exactly `expected`, fail-loud.
async fn await_holder(node: &Arc<MeshNode>, island: u64, expected: u64, timeout: Duration) {
    let mut rx = node.reservation_fold().subscribe_changes();
    tokio::time::timeout(
        timeout,
        await_reservation_holder(&mut rx, node.reservation_fold(), island, expected),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "node {:#x} did not see island {island:#x} held by {expected:#x} within {timeout:?}",
            node.node_id()
        )
    });
}

// ============================================================================
// Witnesses.
// ============================================================================

/// W1 — the reservation deadline GATES the takeover CAS: before `D` a foreign
/// reserve is `Lost` (holder unexpired); after `D` the same reserve is `Won`
/// (the expired reservation is legally replaced). Exercises the real
/// `reserve_island` CAS on both sides of the deadline.
async fn w1_deadline_gates_takeover(nodes: &[Arc<MeshNode>]) {
    let (h, c) = (&nodes[0], &nodes[1]);
    let (h_id, c_id) = (h.node_id(), c.node_id());
    let island = W_TAKEOVER_BASE;
    let deadline = now_us() + DEADLINE_OFFSET_US;
    assert_eq!(
        h.reserve_island(island, deadline).await.expect("H reserve"),
        ClaimOutcome::Won,
        "H must reserve the free island"
    );
    await_holder(c, island, h_id, DELIVERY_DEADLINE).await;

    // BEFORE the deadline: a foreign reserve is rejected.
    assert_eq!(
        c.reserve_island(island, far_deadline())
            .await
            .expect("C reserve pre-deadline"),
        ClaimOutcome::Lost,
        "before the deadline the takeover CAS must be Lost"
    );
    assert_eq!(
        holder_of(c.reservation_fold(), island),
        Some(h_id),
        "the failed takeover left H holding"
    );

    // AFTER the deadline: the same reserve wins.
    wait_past_deadline(deadline).await;
    assert_eq!(
        c.reserve_island(island, far_deadline())
            .await
            .expect("C reserve post-deadline"),
        ClaimOutcome::Won,
        "after the deadline the takeover CAS must be Won"
    );
    assert_eq!(
        holder_of(c.reservation_fold(), island),
        Some(c_id),
        "the takeover installed C"
    );
    println!("  [PASS] W1 deadline gates takeover: pre-D reserve=Lost, post-D reserve=Won");
}

/// W2 — a lapsed deadline is NOT an automatic reclaim: after `D` passes with no
/// re-claim, the island is STILL held by H (no event fired, the entry is still
/// present); only an active takeover changes it. This is the E6 "not automatic
/// reclaim" fact AND the deadline≠entry-TTL distinction (the entry survives the
/// deadline; its 30 s TTL has not lapsed).
async fn w2_deadline_is_not_automatic_reclaim(nodes: &[Arc<MeshNode>]) {
    let (h, c) = (&nodes[0], &nodes[1]);
    let h_id = h.node_id();
    let island = W_TAKEOVER_BASE + 1;
    let deadline = now_us() + DEADLINE_OFFSET_US;
    assert_eq!(
        h.reserve_island(island, deadline).await.expect("H reserve"),
        ClaimOutcome::Won
    );
    await_holder(c, island, h_id, DELIVERY_DEADLINE).await;

    // Wait well past the deadline AND past a sweep interval — nothing reclaims.
    wait_past_deadline(deadline).await;
    tokio::time::sleep(Duration::from_millis(600)).await; // > one sweep interval
    assert_eq!(
        holder_of(c.reservation_fold(), island),
        Some(h_id),
        "a lapsed deadline must NOT auto-reclaim: the entry is still present, still H"
    );
    println!(
        "  [PASS] W2 lapsed deadline is not automatic reclaim (entry present, still H after a sweep)"
    );
}

/// W3 — a short entry-TTL reservation is SWEPT locally: applied to a node's own
/// fold, present at first, then ABSENT after the TTL + one sweep interval, via
/// the local change watch. Exercises the real `ttl_secs` → `expires_at` → sweep
/// path.
async fn w3_short_ttl_entry_expires_locally(nodes: &[Arc<MeshNode>]) {
    let n = &nodes[0];
    let n_id = n.node_id();
    let island = W_EXPIRY_BASE;
    let ann = short_ttl_reservation(
        n.entity_keypair(),
        island,
        1,
        SHORT_TTL_SECS,
        far_deadline(),
    );
    let mut rx = n.reservation_fold().subscribe_changes();
    n.reservation_fold()
        .apply(ann)
        .expect("apply short-ttl reservation");
    assert_eq!(
        holder_of(n.reservation_fold(), island),
        Some(n_id),
        "the short-ttl entry is present immediately after apply"
    );
    tokio::time::timeout(
        ABSENCE_DEADLINE,
        await_reservation_free(&mut rx, n.reservation_fold(), island),
    )
    .await
    .expect("the short-ttl entry must be swept to absence");
    assert_eq!(
        holder_of(n.reservation_fold(), island),
        None,
        "the entry is absent after its TTL + sweep"
    );
    println!("  [PASS] W3 short-ttl entry is swept to exact absence (local watch)");
}

/// W4 — local and remote expiry are INDEPENDENT: a short-TTL reservation
/// broadcast to a peer expires on BOTH the origin's and the observer's folds,
/// each via its OWN sweep (removal is local, never a broadcast).
async fn w4_local_and_remote_expiry_are_independent(nodes: &[Arc<MeshNode>]) {
    let (n, o) = (&nodes[0], &nodes[2]);
    let n_id = n.node_id();
    let island = W_EXPIRY_BASE + 1;
    let ann = short_ttl_reservation(
        n.entity_keypair(),
        island,
        1,
        SHORT_TTL_SECS,
        far_deadline(),
    );
    let mut n_rx = n.reservation_fold().subscribe_changes();
    let mut o_rx = o.reservation_fold().subscribe_changes();
    n.reservation_fold().apply(ann.clone()).expect("apply");
    n.publish_fold_broadcast(&ann).await.expect("broadcast");
    // Present at both first.
    await_holder(n, island, n_id, DELIVERY_DEADLINE).await;
    await_holder(o, island, n_id, DELIVERY_DEADLINE).await;
    // Then absent at both, each by its own local sweep.
    tokio::time::timeout(
        ABSENCE_DEADLINE,
        await_reservation_free(&mut n_rx, n.reservation_fold(), island),
    )
    .await
    .expect("origin sweeps its own entry");
    tokio::time::timeout(
        ABSENCE_DEADLINE,
        await_reservation_free(&mut o_rx, o.reservation_fold(), island),
    )
    .await
    .expect("observer sweeps its own entry independently");
    assert_eq!(holder_of(n.reservation_fold(), island), None);
    assert_eq!(holder_of(o.reservation_fold(), island), None);
    println!(
        "  [PASS] W4 local + remote expiry are independent (each observer sweeps its own fold)"
    );
}

// ============================================================================
// Measurement — group 1: deadline-enabled takeover.
// ============================================================================

async fn measure_deadline_takeover(nodes: &[Arc<MeshNode>]) {
    let (h, c, o) = (&nodes[0], &nodes[1], &nodes[2]);
    let (h_id, c_id) = (h.node_id(), c.node_id());
    let mut cas = LatencyReport::new(); // first foreign takeover CAS (MECHANISM)
    let mut remote = LatencyReport::new(); // observer reads new holder (VISIBILITY)

    for s in 0..SAMPLES_TAKEOVER {
        let island = TAKEOVER_BASE + s;
        let deadline = now_us() + DEADLINE_OFFSET_US;
        assert_eq!(
            h.reserve_island(island, deadline).await.expect("H reserve"),
            ClaimOutcome::Won
        );
        await_holder(c, island, h_id, DELIVERY_DEADLINE).await; // C must hold H's entry

        // Configured deadline wait (POLICY) — never part of the mechanism timer.
        wait_past_deadline(deadline).await;
        // Not automatic reclaim: the entry is still H right up to the takeover.
        assert_eq!(
            holder_of(c.reservation_fold(), island),
            Some(h_id),
            "no auto-reclaim before the takeover"
        );

        let mut o_rx = o.reservation_fold().subscribe_changes();
        let td = far_deadline(); // precomputed outside timing
        let t0 = Instant::now();
        let out = c
            .reserve_island(island, td)
            .await
            .expect("takeover reserve");
        let cas_dt = t0.elapsed();
        // Remote visibility endpoint (inside the timed region).
        tokio::time::timeout(
            REMOTE_DEADLINE,
            await_reservation_holder(&mut o_rx, o.reservation_fold(), island, c_id),
        )
        .await
        .expect("O observes the new holder C");
        let remote_dt = t0.elapsed();

        assert_eq!(
            out,
            ClaimOutcome::Won,
            "the takeover CAS wins after the deadline"
        );
        assert_eq!(
            holder_of(c.reservation_fold(), island),
            Some(c_id),
            "C holds locally"
        );

        if s >= WARMUP_TAKEOVER {
            cas.record(cas_dt.as_nanos() as u64);
            remote.record(remote_dt.as_nanos() as u64);
        }
    }

    cas.print_row("ICB-6 group-1 · first foreign takeover CAS (mechanism)");
    remote.print_row("ICB-6 group-1 · observer reads new holder (visibility)");
    println!(
        "   configured_deadline_wait={}us (POLICY — separate column, not a measured mechanism) · no \"deadline fired\" event; NOT automatic reclaim · takers=1",
        DEADLINE_OFFSET_US
    );
    println!(
        "   iterations={SAMPLES_TAKEOVER} warmups_discarded={WARMUP_TAKEOVER} measured={} · workers={WORKER_THREADS} · p99 is orientation only, NOT a baseline/threshold (ICB-7)",
        SAMPLES_TAKEOVER - WARMUP_TAKEOVER
    );
    println!();
}

// ============================================================================
// Measurement — group 2: runtime-entry expiry (M5 short-TTL diagnostic).
// ============================================================================

async fn measure_runtime_expiry(nodes: &[Arc<MeshNode>]) {
    let (n, o) = (&nodes[0], &nodes[2]);
    let n_id = n.node_id();
    let mut local_absence = LatencyReport::new();
    let mut remote_absence = LatencyReport::new();

    for s in 0..SAMPLES_EXPIRY {
        let island = EXPIRY_BASE + s;
        let ann = short_ttl_reservation(
            n.entity_keypair(),
            island,
            1,
            SHORT_TTL_SECS,
            far_deadline(),
        );
        // Subscribe BEFORE apply so the absence timer never misses the wake.
        let mut n_rx = n.reservation_fold().subscribe_changes();
        let mut o_rx = o.reservation_fold().subscribe_changes();

        let t0 = Instant::now();
        n.reservation_fold().apply(ann.clone()).expect("apply");
        n.publish_fold_broadcast(&ann).await.expect("broadcast");
        await_holder(n, island, n_id, DELIVERY_DEADLINE).await;
        await_holder(o, island, n_id, DELIVERY_DEADLINE).await;

        tokio::time::timeout(
            ABSENCE_DEADLINE,
            await_reservation_free(&mut n_rx, n.reservation_fold(), island),
        )
        .await
        .expect("local absence");
        let local_dt = t0.elapsed();
        tokio::time::timeout(
            ABSENCE_DEADLINE,
            await_reservation_free(&mut o_rx, o.reservation_fold(), island),
        )
        .await
        .expect("remote absence");
        let remote_dt = t0.elapsed();

        assert_eq!(
            holder_of(n.reservation_fold(), island),
            None,
            "local absent"
        );
        assert_eq!(
            holder_of(o.reservation_fold(), island),
            None,
            "remote absent"
        );
        local_absence.record(local_dt.as_nanos() as u64);
        remote_absence.record(remote_dt.as_nanos() as u64);
    }

    // Small sample, TTL-bound — report p50 only (NO p99).
    println!(
        "── ICB-6 group-2 · apply→local absence (entry TTL + sweep) · samples={} ──",
        local_absence.samples()
    );
    println!(
        "   p50={:.2}ms (origin's own sweep)",
        local_absence.quantile_us(0.50) / 1_000.0
    );
    println!(
        "── ICB-6 group-2 · apply→remote absence (observer's independent sweep) · samples={} ──",
        remote_absence.samples()
    );
    println!(
        "   p50={:.2}ms (observer's own sweep — removal is LOCAL, never broadcast)",
        remote_absence.quantile_us(0.50) / 1_000.0
    );
    println!(
        "   configured_ttl={SHORT_TTL_SECS}s (POLICY — separate column) · sweep_interval=500ms · absence ≈ TTL + sweep · M5 short-TTL diagnostic (production DEFAULT_TTL=30s unchanged)"
    );
    println!(
        "   iterations={SAMPLES_EXPIRY} (small — TTL-bound; NO p99) · local expiry SEPARATE from remote expiry · workers={WORKER_THREADS}"
    );
    println!();
}
