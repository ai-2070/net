//! ICB-2 — single-claimant claim boundaries.
//!
//! ONE uncontended claimant, ONE direct observer, ONE direct logical
//! session, a FRESH island per sample. From one common start time `t0`
//! (the `reserve_island` invocation) we measure three INDEPENDENT
//! endpoints:
//!
//!   A. local exact commit  — the claimant's reservation fold reads
//!      holder == claimant (exact-holder read, never a bare watch wake).
//!   B. API return          — `reserve_island` returns `ClaimOutcome::Won`
//!      (this includes local apply + peer fan-out work).
//!   C. direct remote        — the direct observer's fold reads holder ==
//!      claimant AND its CountingRouter records exactly one verified
//!      delivery from the claimant, BOTH halves INSIDE the timed endpoint.
//!      Bare exact state does not stop the remote timer: the router applies
//!      to the fold (whose watcher wakes) and THEN records the delivery.
//!
//! Critical terminology (Kyra ICB-2): `ClaimOutcome::Won` is NOT local
//! exact commit and NOT remote exact visibility — they are three distinct
//! boundaries. All three are reported FROM t0; no signed `local→API` or
//! `API→remote` residual is derived (observation order is not guaranteed —
//! the local transition happens before API return internally, but the
//! async watcher may be scheduled later).
//!
//! This is UNCONTENDED single-claimant visibility — never call it
//! distributed reservation convergence (that is ICB-3). No concurrent
//! claimants, divergence, fallback, sensed readiness, takeover, or
//! runtime expiry here (ICB-3..6). No threshold or public claim (ICB-7).
//!
//! A routed logical-peer row would be a FOURTH separately-labeled endpoint
//! and must pass the reservation-delivery preflight first; a raw A↔R↔B
//! chain is refused (reservations reach direct peers only), so ICB-2's
//! baseline emits the direct row only.
//!
//! Run: `cargo bench -p net-mesh --bench island_claim_boundaries --features net`

#[path = "bench_island_claim/mod.rs"]
mod bench_island_claim;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bench_island_claim::*;
use net::adapter::net::behavior::fold::{Fold, FoldChannelRouter, ReservationFold};
use net::adapter::net::behavior::gang::ClaimOutcome;
use net::adapter::net::{EntityKeypair, MeshNode};

const ITERS: u64 = 300;
const WARMUP: u64 = 30;
const ISLAND_BASE: u64 = 0x2C00_0000;
const PREFLIGHT_ISLAND: u64 = 0x2C00_FFFF;
/// Per-sample joint ceiling; a joint-sample timeout skips the sample (no print).
const SAMPLE_DEADLINE: Duration = Duration::from_secs(5);

fn main() {
    let rt = runtime();
    rt.block_on(async {
        println!("\n=== ICB-2 single-claimant claim boundaries ===\n");

        println!("-- witnesses --");
        w1_2_bare_wake_needs_exact_holder().await;
        w3_api_won_not_remote_visibility().await;
        w4_missing_delivery_fails().await;
        w5_wrong_publisher_excluded();
        w6_count_overshoot_fails().await;
        w7_raw_chain_no_routed_row().await;
        w8_reset_failure_blocks_timing().await;
        w9_final_state_exact().await;
        w10_combined_remote_endpoint_needs_delivery().await;

        println!("\n-- measurement (direct row) --");
        measure_direct().await;
    });
}

// ============================================================================
// Measurement — the direct single-claimant row (three endpoints from t0).
// ============================================================================

async fn measure_direct() {
    // Before timing: warm identity/capability, install the observer's
    // counting router, prove direct reservation delivery.
    let (claimant, observer) = pair().await;
    let ocount = install_counter(&observer, PREFLIGHT_ISLAND);
    assert!(
        reservation_delivers(&claimant, &ocount, PREFLIGHT_ISLAND).await,
        "ICB-2 direct row requires a proven direct reservation delivery (preflight)"
    );
    let cid = claimant.node_id();

    let mut local = LatencyReport::new();
    let mut api = LatencyReport::new();
    let mut remote = LatencyReport::new();
    let mut joint_timeouts = 0u64;

    for i in 0..ITERS {
        let island = ISLAND_BASE + i;
        match one_sample(&claimant, &observer, &ocount, island, cid).await {
            Some((dl, da, dr)) => {
                if i >= WARMUP {
                    local.record(dl.as_nanos() as u64);
                    api.record(da.as_nanos() as u64);
                    remote.record(dr.as_nanos() as u64);
                }
            }
            None => joint_timeouts += 1, // joint-sample timeout: omitted from all three
        }
    }

    print_endpoint(
        &local,
        "single claimant: invocation start → local exact holder observed",
        joint_timeouts,
        "paired sample postcondition",
    );
    print_endpoint(
        &api,
        "single claimant: invocation start → reserve_island API returns Won",
        joint_timeouts,
        "paired sample postcondition",
    );
    print_endpoint(
        &remote,
        "single claimant: invocation start → direct observer exact holder + verified claimant delivery observed",
        joint_timeouts,
        "part of the timed endpoint",
    );
    println!(
        "   routed row: NOT emitted — a routed reservation row needs a proven logical-peer delivery; a raw A↔R↔B chain is refused (W7). No logical routed peer in ICB-2's baseline."
    );
}

/// One uncontended sample on a FRESH island. Returns the three endpoint
/// durations from a common t0, or `None` on a joint-sample timeout (skipped,
/// not printed). Panics on any hard invariant violation (Lost, wrong
/// publisher, count overshoot, holder mismatch) — those never print.
async fn one_sample(
    claimant: &Arc<MeshNode>,
    observer: &Arc<MeshNode>,
    ocount: &Arc<CountingRouter>,
    island: u64,
    cid: u64,
) -> Option<(Duration, Duration, Duration)> {
    // Outside timing: retarget the counting router, confirm the island is
    // exactly free everywhere, subscribe both exact-state receivers.
    ocount.reset(island);
    assert_eq!(
        holder_of(claimant.reservation_fold(), island),
        None,
        "claimant island not free pre-sample"
    );
    assert_eq!(
        holder_of(observer.reservation_fold(), island),
        None,
        "observer island not free pre-sample"
    );
    let mut rx_local = claimant.reservation_fold().subscribe_changes();
    let mut rx_remote = observer.reservation_fold().subscribe_changes();

    // The deadline policy is constructed OUTSIDE the timed region: the arg
    // is evaluated before `reserve_island` is entered, so its
    // `SystemTime::now()` cost must not sit inside any endpoint's boundary.
    let claim_deadline = far_deadline();

    // Common start; three endpoints measured independently from t0. The
    // remote endpoint's verified-delivery barrier is INSIDE its timed
    // future (`remote_endpoint`), not a post-join postcondition.
    let t0 = Instant::now();
    let claim_fut = async {
        let out = claimant
            .reserve_island(island, claim_deadline)
            .await
            .expect("reserve API");
        (t0.elapsed(), out)
    };
    let local_fut = async {
        await_reservation_holder(&mut rx_local, claimant.reservation_fold(), island, cid).await;
        t0.elapsed()
    };
    let remote_fut = async {
        remote_endpoint(
            &mut rx_remote,
            observer.reservation_fold(),
            island,
            cid,
            ocount,
        )
        .await;
        t0.elapsed()
    };
    // Joint ceiling over ALL THREE endpoints. A timeout omits the sample
    // from all three paired distributions — it is a JOINT-sample timeout,
    // not specifically a delivery timeout.
    let joined = tokio::time::timeout(SAMPLE_DEADLINE, async {
        tokio::join!(claim_fut, local_fut, remote_fut)
    })
    .await;
    let ((api_dt, outcome), local_dt, remote_dt) = match joined {
        Ok(v) => v,
        Err(_) => return None, // joint-sample timeout: omit from all three
    };

    // Postconditions (paired sample checks). The verified-delivery barrier
    // is already timed inside `remote_fut`; these are redundant final state.
    assert_eq!(
        outcome,
        ClaimOutcome::Won,
        "uncontended single claim must Win (not Lost)"
    );
    assert_eq!(
        holder_of(claimant.reservation_fold(), island),
        Some(cid),
        "claimant final holder must be exact"
    );
    assert_eq!(
        holder_of(observer.reservation_fold(), island),
        Some(cid),
        "observer final holder must be exact"
    );

    Some((local_dt, api_dt, remote_dt))
}

/// The combined REMOTE endpoint: block until the observer's reservation
/// fold reads holder == `cid` AND its counting router has recorded exactly
/// one verified delivery from `cid`. BOTH halves must hold — bare exact
/// state does NOT complete this endpoint, because the exact-holder watcher
/// can wake before the counting router records the delivery (the router
/// applies to the fold, whose watcher fires, THEN records the delivery).
/// Fail-loud on overshoot or a wrong publisher.
async fn remote_endpoint(
    rx: &mut tokio::sync::watch::Receiver<u64>,
    fold: &Arc<Fold<ReservationFold>>,
    island: u64,
    cid: u64,
    ocount: &Arc<CountingRouter>,
) {
    await_reservation_holder(rx, fold, island, cid).await;
    assert!(
        wait_count(ocount, 1, SAMPLE_DEADLINE).await,
        "observer must receive EXACTLY one verified delivery (got {})",
        ocount.count()
    );
    assert_eq!(
        ocount.seen_publishers(),
        HashSet::from([cid]),
        "the verified delivery's publisher must be the claimant"
    );
}

fn print_endpoint(report: &LatencyReport, label: &str, joint_timeouts: u64, delivery_role: &str) {
    println!("── {label} ──");
    println!(
        "   topology=claimant↔observer · claimants=1 · mode=direct · logical_peers=1 · fan_out_peers=1(observer) · workers={WORKER_THREADS}"
    );
    println!(
        "   island_units=n/a (raw reservation CAS; units are an ICB-1 matcher concept) · deadline=far-future (precomputed outside timing; no takeover — ICB-6 owns it) · fixture=fresh-island-per-sample (no in-timing reset)"
    );
    println!(
        "   preflight=PASS(direct reservation delivery proven) · exact_state=enforced · verified_delivery={delivery_role}"
    );
    println!(
        "   completed_samples={} · joint_sample_timeouts={joint_timeouts} (joint ceiling covers all three endpoints; timed-out samples omitted from all three paired distributions)",
        report.samples()
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

// ============================================================================
// Witnesses (Kyra ICB-2 required, red-verified where load-bearing).
// ============================================================================

/// W1 + W2 — a bare watch wake without the exact holder does not complete
/// local (or remote) timing. `await_reservation_holder` is the same helper
/// for both the claimant's and the observer's fold, so one witness covers
/// both roles: an unrelated change fires the watch, yet the target await
/// times out because the exact read still fails.
async fn w1_2_bare_wake_needs_exact_holder() {
    for _role in 0..2 {
        let fold = Arc::new(Fold::<ReservationFold>::new());
        let target = EntityKeypair::generate();
        let noise = EntityKeypair::generate();
        let mut rx = fold.subscribe_changes();
        apply_reserve(&fold, &noise, 0x2u64, 1); // unrelated wake
        let early = tokio::time::timeout(
            Duration::from_millis(150),
            await_reservation_holder(&mut rx, &fold, 0x1u64, target.node_id()),
        )
        .await;
        assert!(
            early.is_err(),
            "a bare watch wake must not complete exact-holder timing"
        );
    }
    println!("  [PASS] W1,W2 bare watch wake does not complete local/remote timing");
}

/// W3 — `ClaimOutcome::Won` does NOT imply direct remote exact visibility:
/// a disconnected observer never sees the holder even though the local CAS
/// won.
async fn w3_api_won_not_remote_visibility() {
    let claimant = node().await;
    claimant.start_arc();
    let observer = node().await; // deliberately NOT connected
    observer.start_arc();
    let island = 0x2C03u64;
    let co = install_counter(&observer, island);
    let out = claimant
        .reserve_island(island, far_deadline())
        .await
        .expect("reserve");
    assert_eq!(out, ClaimOutcome::Won, "local CAS wins with no observer");
    let mut rxo = observer.reservation_fold().subscribe_changes();
    let remote = tokio::time::timeout(
        Duration::from_millis(300),
        await_reservation_holder(
            &mut rxo,
            observer.reservation_fold(),
            island,
            claimant.node_id(),
        ),
    )
    .await;
    assert!(
        remote.is_err(),
        "API Won must NOT substitute for remote exact visibility"
    );
    assert_eq!(
        co.count(),
        0,
        "no verified delivery to a disconnected observer"
    );
    println!("  [PASS] W3 API Won != direct remote exact visibility");
}

/// W4 — a missing verified delivery fails the sample (isolated observer →
/// count stays 0 → the exact barrier returns false).
async fn w4_missing_delivery_fails() {
    let claimant = node().await;
    claimant.start_arc();
    let observer = node().await; // NOT connected
    observer.start_arc();
    let island = 0x2C04u64;
    let co = install_counter(&observer, island);
    claimant
        .reserve_island(island, far_deadline())
        .await
        .expect("reserve");
    assert!(
        !wait_count(&co, 1, Duration::from_millis(300)).await,
        "a missing verified delivery must fail the sample"
    );
    println!("  [PASS] W4 missing verified delivery fails the sample");
}

/// W5 — a wrong-publisher frame is excluded from verified delivery (the
/// signature/node-id verify fails; nothing is counted).
fn w5_wrong_publisher_excluded() {
    let island = 0x2C05u64;
    let (cr, _fold) = unit_router(island);
    let signer = EntityKeypair::generate();
    let impostor = EntityKeypair::generate();
    let bytes = reserve_bytes(&signer, island, 1);
    assert!(
        cr.try_route(impostor.entity_id(), &bytes).is_err(),
        "a mismatched publisher must not dispatch"
    );
    assert_eq!(cr.count(), 0, "wrong-publisher frame must not count");
    assert!(cr.seen_publishers().is_empty());
    println!("  [PASS] W5 wrong-publisher frame excluded from verified delivery");
}

/// W6 — count overshoot fails the sample (exact barrier rejects 2 > 1).
async fn w6_count_overshoot_fails() {
    let island = 0x2C06u64;
    let (cr, _fold) = unit_router(island);
    let a = EntityKeypair::generate();
    let b = EntityKeypair::generate();
    let _ = cr.try_route(a.entity_id(), &reserve_bytes(&a, island, 1));
    let _ = cr.try_route(b.entity_id(), &reserve_bytes(&b, island, 1));
    assert_eq!(cr.count(), 2);
    assert!(
        !wait_count(&cr, 1, Duration::from_millis(200)).await,
        "count overshoot (2 > 1) must fail the sample"
    );
    println!("  [PASS] W6 count overshoot fails the sample");
}

/// W7 — a raw A↔R↔B chain cannot emit a routed row (reservations reach
/// direct peers only; the preflight refuses).
async fn w7_raw_chain_no_routed_row() {
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
        !reservation_delivers(&a, &cb, 0x2C07u64).await,
        "a raw chain must be refused as a routed reservation row"
    );
    println!("  [PASS] W7 raw A<->R<->B chain cannot emit a routed row");
}

/// W8 — a failed fixture reset prevents timing (fail-loud panic).
async fn w8_reset_failure_blocks_timing() {
    let (h, _peer) = pair().await;
    let island = 0x2C08u64;
    assert_eq!(
        h.reserve_island(island, far_deadline())
            .await
            .expect("reserve"),
        ClaimOutcome::Won
    );
    let o = node().await;
    o.start_arc();
    apply_reserve(o.reservation_fold(), &EntityKeypair::generate(), island, 1);
    let h2 = h.clone();
    let o2 = o.clone();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = tokio::spawn(async move {
        release_and_await_free(&h2, &[&o2], island, Duration::from_millis(300)).await
    })
    .await;
    std::panic::set_hook(prev);
    assert!(
        matches!(&res, Err(e) if e.is_panic()),
        "a failed fixture reset must prevent timing (panic)"
    );
    println!("  [PASS] W8 failed fixture reset prevents timing");
}

/// W9 — final state is exact on both the claimant and the observer.
async fn w9_final_state_exact() {
    let (claimant, observer) = pair().await;
    let island = 0x2C09u64;
    let cid = claimant.node_id();
    claimant
        .reserve_island(island, far_deadline())
        .await
        .expect("reserve");
    let mut rxo = observer.reservation_fold().subscribe_changes();
    tokio::time::timeout(
        Duration::from_secs(3),
        await_reservation_holder(&mut rxo, observer.reservation_fold(), island, cid),
    )
    .await
    .expect("observer must see the claimant");
    assert_eq!(holder_of(claimant.reservation_fold(), island), Some(cid));
    assert_eq!(holder_of(observer.reservation_fold(), island), Some(cid));
    println!("  [PASS] W9 final state exact on claimant + observer");
}

/// W10 — the combined remote endpoint requires verified delivery, not bare
/// exact state. Applying the reservation directly to the observer fold
/// (bypassing the CountingRouter) makes the exact holder visible but leaves
/// the verified-delivery count at zero, so `remote_endpoint` must NOT
/// complete; routing the same signed frame through the counting router
/// (Rejected-but-counted) then completes it with the expected publisher.
async fn w10_combined_remote_endpoint_needs_delivery() {
    let observer = node().await;
    let island = 0x2C0Au64;
    let ocount = install_counter(&observer, island);
    let kp = EntityKeypair::generate();
    let cid = kp.node_id();

    // 1. Apply directly to the observer fold, BYPASSING the counting router.
    observer
        .reservation_fold()
        .apply(reserve_ann(&kp, island, 1))
        .expect("direct apply");
    // 2. The observer's exact-holder read succeeds.
    assert_eq!(
        holder_of(observer.reservation_fold(), island),
        Some(cid),
        "exact holder must be visible"
    );
    // 3. The combined endpoint does NOT complete on bare exact state: the
    //    verified-delivery count is still zero, so `remote_endpoint` blocks.
    let mut rx = observer.reservation_fold().subscribe_changes();
    let bare = tokio::time::timeout(
        Duration::from_millis(200),
        remote_endpoint(&mut rx, observer.reservation_fold(), island, cid, &ocount),
    )
    .await;
    assert!(
        bare.is_err(),
        "bare exact state must NOT complete the combined remote endpoint"
    );
    assert_eq!(ocount.count(), 0, "no verified delivery recorded yet");

    // 4. Route the signed frame THROUGH the counting router. The fold
    //    Rejects (state already present) but the delivery still counts.
    let out = ocount.try_route(kp.entity_id(), &reserve_bytes(&kp, island, 1));
    assert!(
        out.is_ok(),
        "verified route must dispatch (Rejected ok), got {out:?}"
    );

    // 5. The combined endpoint now completes with the expected publisher.
    let mut rx2 = observer.reservation_fold().subscribe_changes();
    let done = tokio::time::timeout(
        Duration::from_millis(500),
        remote_endpoint(&mut rx2, observer.reservation_fold(), island, cid, &ocount),
    )
    .await;
    assert!(
        done.is_ok(),
        "combined endpoint must complete once verified delivery is recorded"
    );
    println!(
        "  [PASS] W10 combined remote endpoint requires verified delivery, not bare exact state"
    );
}
