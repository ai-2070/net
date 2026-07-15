//! P5 — spend-policy contention. Measure `check_and_reserve` under
//! concurrency WITHOUT optimizing it; the goal is to discover the smallest
//! legitimate atomic accounting boundary before any store replacement.
//!
//! Protocol for every stateful sample (a "storm" = N barrier-synced callers):
//!   1. restore a prepared fixed-cardinality baseline OUTSIDE timing;
//!   2. decode + verify starting cardinality / spend;
//!   3. release all contenders on a `Barrier`;
//!   4. time each complete `check_and_reserve`;
//!   5. join;
//!   6. decode the final persisted state;
//!   7. assert accounting + no-overspend;
//!   8. record bytes / cardinality before and after.
//!
//! Parts:
//!   P5a same-counter contention (ample + near-limit K, cardinality axis);
//!   P5b different capabilities sharing one (day,asset) counter (parent cap);
//!   P5c logically independent reservations (different asset ⇒ no shared
//!       counter) — the partitionability row: if throughput/tails match P5a,
//!       the file lock, not accounting authority, imposes the coupling;
//!   P5d approval contention (same approval key);
//!   P5e housekeeping interaction (a focused correctness group).
//!
//! Run: cargo bench -p net-payments --bench spend_contention

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use net::adapter::net::identity::EntityKeypair;
use net_payments::core::quote::PaymentQuote;
use net_payments::core::registry::{default_mock_registry, AssetEntry, AssetRegistry};
use net_payments::core::units::AtomicAmount;
use net_payments::policy::spend::{SpendDecision, SpendPolicyEngine, SpendProfile};
use net_payments::x402::caip::AssetId;
use net_payments::x402::requirements::PaymentRequirements;
use net_payments::x402::X402Carry;
use tokio::sync::Barrier;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{
    new_hist, restore_state, runtime, snapshot_state, state_bytes, state_placement,
};

const NOW: u64 = 1_000_000_000_000_000;
const NS_PER_DAY: u64 = 86_400_000_000_000;
const TTL: u64 = 60_000_000_000;
const NET: &str = "mock:net";
const ASSET: &str = "musd";
const AMOUNT: u128 = 2500;
const CONCURRENCY: &[usize] = &[16, 128];
const CARDS: &[usize] = &[0, 100, 1000]; // P5a history-cardinality axis (10k opt)

fn target_samples() -> usize {
    std::env::var("NET_PAY_BENCH_SAMPLES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(256)
}

// ---- registry / quotes -----------------------------------------------------

/// A mock registry with `musd` plus `n_extra` independent assets
/// `musd0..musdN` (each its own CAIP id ⇒ its own day counter) for P5c.
fn registry(n_extra: usize) -> Arc<AssetRegistry> {
    let signer = EntityKeypair::generate().entity_id().clone();
    let mut reg = default_mock_registry(signer);
    for i in 0..n_extra {
        reg.assets.push(AssetEntry {
            id: AssetId::parse(&format!("mock:net/token:musd{i}")).unwrap(),
            x402_asset: format!("musd{i}"),
            decimals: 6,
            symbol: format!("MUSD{i}"),
            display_name: None,
            equivalence_class: None,
        });
    }
    Arc::new(reg)
}

fn reqs(asset: &str, amount: u128) -> X402Carry<PaymentRequirements> {
    X402Carry::author(&PaymentRequirements {
        scheme: "mock".into(),
        network: NET.into(),
        amount: amount.to_string(),
        asset: asset.into(),
        pay_to: "mock-provider-settle-addr".into(),
        max_timeout_seconds: 60,
        extra: None,
    })
    .unwrap()
}

/// A distinct quote (unique caller ⇒ unique quote id) for `capability` paying
/// `amount` of `asset`, issued at `issued`.
fn quote(
    reg: &AssetRegistry,
    capability: &str,
    asset: &str,
    amount: u128,
    issued: u64,
) -> PaymentQuote {
    PaymentQuote::new(
        EntityKeypair::generate().entity_id().clone(),
        EntityKeypair::generate().entity_id().clone(),
        capability,
        None,
        reqs(asset, amount),
        reg.reference().unwrap(),
        issued,
        issued + TTL,
    )
}

// ---- state decode ----------------------------------------------------------

/// Count pending approval records (the spend store's growth term).
fn approval_count(path: &Path) -> usize {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice::<serde_json::Value>(&raw)
            .expect("state is JSON")
            .get("approvals")
            .and_then(|a| a.as_object())
            .map(|o| o.len())
            .unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("approval_count: {e}"),
    }
}

// ---- contention runner -----------------------------------------------------

/// Barrier-synchronized storm: all `quotes.len()` callers hit
/// `check_and_reserve` against the shared store at once. Returns per-caller
/// (latency_ns, decision).
async fn storm(
    engine: Arc<SpendPolicyEngine>,
    reg: Arc<AssetRegistry>,
    quotes: Vec<PaymentQuote>,
    now: u64,
) -> Vec<(u64, SpendDecision)> {
    let barrier = Arc::new(Barrier::new(quotes.len()));
    let mut handles = Vec::with_capacity(quotes.len());
    for q in quotes {
        let engine = Arc::clone(&engine);
        let reg = Arc::clone(&reg);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let t = Instant::now();
            let d = engine
                .check_and_reserve(&q, &reg, now)
                .await
                .expect("check_and_reserve");
            (t.elapsed().as_nanos() as u64, d)
        }));
    }
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.expect("join"));
    }
    out
}

fn admitted(results: &[(u64, SpendDecision)]) -> usize {
    results
        .iter()
        .filter(|(_, d)| matches!(d, SpendDecision::Allowed))
        .count()
}

/// Seed `n` pending approvals into the store (the history bulk): a Production
/// engine denies mock with an approval hold, inserting one pending record per
/// distinct quote. Returns the raw baseline snapshot.
async fn seed_approvals(path: &Path, reg: &Arc<AssetRegistry>, n: usize) -> Vec<u8> {
    if n > 0 {
        let seeder = SpendPolicyEngine::new(path, SpendProfile::Production);
        for i in 0..n {
            let q = quote(reg, "seed-cap", ASSET, AMOUNT, NOW + 100 + i as u64);
            let d = seeder.check_and_reserve(&q, reg, NOW).await.unwrap();
            assert!(matches!(d, SpendDecision::RequiresPaymentApproval { .. }));
        }
    }
    snapshot_state(path)
}

fn storms_for(n: usize) -> usize {
    target_samples().div_ceil(n)
}

// ---- reporting -------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn report(
    label: &str,
    hist: &hdrhistogram::Histogram<u64>,
    samples: usize,
    conc: usize,
    wall: f64,
    attempts: usize,
    reservations: usize,
    denials: usize,
    approvals_created: usize,
    caps: usize,
    assets: usize,
    cap: Option<u128>,
    start_spend: u128,
    final_spend: u128,
    overspend: u128,
    bytes_before: u64,
    bytes_after: u64,
    cards_before: usize,
    cards_after: usize,
    placement: &str,
) {
    let us = |q: f64| hist.value_at_quantile(q) as f64 / 1000.0;
    // p99 only when the sample count is credible (>= 500).
    let p99 = if samples >= 500 {
        format!("{:.1}", us(0.99))
    } else {
        "n/a".into()
    };
    println!(
        "  {label:<34} p50={:>9.1}us p95={:>9.1}us p99={p99:>9}us max={:>9.1}us",
        us(0.50),
        us(0.95),
        hist.max() as f64 / 1000.0,
    );
    println!(
        "      attempts/s={:.1} reservations/s={:.1} denials/s={:.1} approvals_created/s={:.1}",
        attempts as f64 / wall,
        reservations as f64 / wall,
        denials as f64 / wall,
        approvals_created as f64 / wall,
    );
    println!(
        "      conc={conc} samples={samples} caps={caps} assets={assets} cap={} \
         start_spend={start_spend} final_spend={final_spend} OVERSPEND={overspend} \
         admitted={reservations} denied={denials}",
        cap.map(|c| c.to_string()).unwrap_or_else(|| "none".into()),
    );
    println!(
        "      records={cards_before}->{cards_after} bytes={bytes_before}->{bytes_after} {placement}"
    );
}

fn main() {
    let rt = runtime();
    println!(
        "spend_contention — target_samples={}, network={NET}, asset={ASSET}, amount={AMOUNT}",
        target_samples()
    );

    // =====================================================================
    // P5a — same-counter contention (ample + near-limit), cardinality axis.
    // =====================================================================
    println!("\n## P5a — same-counter contention (one day|asset|capability)");
    let reg = registry(0);
    for &card in CARDS {
        let placement = state_placement();
        let path = placement.dir.path().join("policy.json");
        let base_card = rt.block_on(seed_approvals(&path, &reg, card));
        let plabel = format!(
            "{} ({})",
            path.display(),
            match placement.memory_backed {
                bench_common::MemoryBacking::Asserted => "memory-backed: asserted",
                bench_common::MemoryBacking::NotAsserted => "memory-backed: not asserted",
            }
        );

        for &conc in CONCURRENCY {
            let m = storms_for(conc);

            // --- Ample: no caps → every valid request reserves. ---
            {
                restore_state(&path, &base_card);
                let engine = Arc::new(SpendPolicyEngine::new(&path, SpendProfile::DevTest));
                let ample_baseline = snapshot_state(&path);
                let bytes_before = state_bytes(&path);
                let cards_before = approval_count(&path);
                let mut hist = new_hist();
                let (mut attempts, mut reservations) = (0usize, 0usize);
                let start = Instant::now();
                rt.block_on(async {
                    for s in 0..m {
                        restore_state(&path, &ample_baseline);
                        let qs: Vec<_> = (0..conc)
                            .map(|i| {
                                quote(
                                    &reg,
                                    "cap-a",
                                    ASSET,
                                    AMOUNT,
                                    NOW + 1_000 * s as u64 + i as u64,
                                )
                            })
                            .collect();
                        let res = storm(Arc::clone(&engine), Arc::clone(&reg), qs, NOW).await;
                        let adm = admitted(&res);
                        // Every valid request reserves; counter == sum of admitted.
                        let spent = engine.spent_today(NET, ASSET, NOW).await.unwrap();
                        assert_eq!(adm, conc, "ample: all admitted");
                        assert_eq!(
                            spent,
                            AtomicAmount::from_u128(conc as u128 * AMOUNT),
                            "ample: counter == exact sum of admitted (no lost updates)"
                        );
                        for (ns, _) in &res {
                            hist.record(*ns).unwrap();
                        }
                        attempts += res.len();
                        reservations += adm;
                    }
                });
                let final_spend = conc as u128 * AMOUNT; // per-storm; reset each sample
                report(
                    &format!("ample c{conc} card={card}"),
                    &hist,
                    attempts,
                    conc,
                    start.elapsed().as_secs_f64(),
                    attempts,
                    reservations,
                    attempts - reservations,
                    0,
                    1,
                    1,
                    None,
                    0,
                    final_spend,
                    0,
                    bytes_before,
                    state_bytes(&path),
                    cards_before,
                    approval_count(&path),
                    &plabel,
                );
            }

            // --- Near-limit: cap = K*amount, K neither 1 nor N-1. ---
            {
                let k = (conc / 2).max(1); // K = N/2 (not 1, not N-1 for N>=4)
                let cap = k as u128 * AMOUNT;
                restore_state(&path, &base_card);
                let cfg = SpendPolicyEngine::new(&path, SpendProfile::DevTest);
                rt.block_on(
                    cfg.configure(|d, _| d.max_per_day = Some(AtomicAmount::from_u128(cap))),
                )
                .unwrap();
                let near_baseline = snapshot_state(&path);
                let bytes_before = state_bytes(&path);
                let cards_before = approval_count(&path);
                let engine = Arc::new(SpendPolicyEngine::new(&path, SpendProfile::DevTest));
                let mut hist = new_hist();
                let (mut attempts, mut reservations, mut denials) = (0usize, 0usize, 0usize);
                let start = Instant::now();
                rt.block_on(async {
                    for s in 0..m {
                        restore_state(&path, &near_baseline);
                        let qs: Vec<_> = (0..conc)
                            .map(|i| {
                                quote(
                                    &reg,
                                    "cap-a",
                                    ASSET,
                                    AMOUNT,
                                    NOW + 1_000 * s as u64 + i as u64,
                                )
                            })
                            .collect();
                        let res = storm(Arc::clone(&engine), Arc::clone(&reg), qs, NOW).await;
                        let adm = admitted(&res);
                        let spent = engine.spent_today(NET, ASSET, NOW).await.unwrap();
                        // Exactly K admitted; counter == K*amount <= cap; no overspend.
                        assert_eq!(adm, k, "near-limit: exactly K admitted");
                        assert_eq!(
                            spent,
                            AtomicAmount::from_u128(cap),
                            "near-limit: final reserved == K*amount == cap"
                        );
                        assert!(spent <= AtomicAmount::from_u128(cap), "no overspend");
                        for (ns, _) in &res {
                            hist.record(*ns).unwrap();
                        }
                        attempts += res.len();
                        reservations += adm;
                        denials += conc - adm;
                    }
                });
                report(
                    &format!("near-limit(K={k}) c{conc} card={card}"),
                    &hist,
                    attempts,
                    conc,
                    start.elapsed().as_secs_f64(),
                    attempts,
                    reservations,
                    denials,
                    denials, // over-cap holds create one pending approval each
                    conc,
                    1,
                    Some(cap),
                    0,
                    cap,
                    0,
                    bytes_before,
                    state_bytes(&path),
                    cards_before,
                    approval_count(&path),
                    &plabel,
                );
            }
        }
    }

    // =====================================================================
    // P5b — different capabilities, one shared (day,asset) counter + cap.
    // =====================================================================
    println!("\n## P5b — different capabilities, shared parent cap (one day|asset counter)");
    let reg = registry(0);
    for &conc in CONCURRENCY {
        let m = storms_for(conc);
        let placement = state_placement();
        let path = placement.dir.path().join("policy.json");
        let plabel = format!("{} (op-fs)", path.display());
        // Ample shared cap so all admit — the question is contention, not denial.
        let engine = Arc::new(SpendPolicyEngine::new(&path, SpendProfile::DevTest));
        let mut hist = new_hist();
        let (mut attempts, mut reservations) = (0usize, 0usize);
        let start = Instant::now();
        rt.block_on(async {
            for s in 0..m {
                restore_state(&path, &[]); // fresh
                // conc distinct capabilities, SAME asset → one shared counter.
                let qs: Vec<_> = (0..conc)
                    .map(|i| quote(&reg, &format!("cap-{i}"), ASSET, AMOUNT, NOW + 1_000 * s as u64 + i as u64))
                    .collect();
                let res = storm(Arc::clone(&engine), Arc::clone(&reg), qs, NOW).await;
                let adm = admitted(&res);
                let spent = engine.spent_today(NET, ASSET, NOW).await.unwrap();
                // Distinct capabilities still share one atomic counter.
                assert_eq!(adm, conc, "P5b ample: all admit");
                assert_eq!(
                    spent,
                    AtomicAmount::from_u128(conc as u128 * AMOUNT),
                    "P5b: aggregate spend on the shared counter is exact (no lost updates across capabilities)"
                );
                for (ns, _) in &res {
                    hist.record(*ns).unwrap();
                }
                attempts += res.len();
                reservations += adm;
            }
        });
        report(
            &format!("shared-counter diff-caps c{conc}"),
            &hist,
            attempts,
            conc,
            start.elapsed().as_secs_f64(),
            attempts,
            reservations,
            attempts - reservations,
            0,
            conc,
            1,
            None,
            0,
            conc as u128 * AMOUNT,
            0,
            0,
            state_bytes(&path),
            0,
            approval_count(&path),
            &plabel,
        );
    }

    // =====================================================================
    // P5c — logically independent reservations (different asset per caller).
    //       No shared counter. Compare throughput/tails to P5a/P5b.
    // =====================================================================
    println!("\n## P5c — logically independent (different asset ⇒ no shared counter)");
    for &conc in CONCURRENCY {
        let m = storms_for(conc);
        let reg = registry(conc); // conc independent assets
        let placement = state_placement();
        let path = placement.dir.path().join("policy.json");
        let plabel = format!("{} (op-fs)", path.display());
        let engine = Arc::new(SpendPolicyEngine::new(&path, SpendProfile::DevTest));
        let mut hist = new_hist();
        let (mut attempts, mut reservations) = (0usize, 0usize);
        let start = Instant::now();
        rt.block_on(async {
            for s in 0..m {
                restore_state(&path, &[]);
                // distinct capability AND distinct asset → distinct counter keys.
                let qs: Vec<_> = (0..conc)
                    .map(|i| {
                        quote(
                            &reg,
                            &format!("cap-{i}"),
                            &format!("musd{i}"),
                            AMOUNT,
                            NOW + 1_000 * s as u64 + i as u64,
                        )
                    })
                    .collect();
                let res = storm(Arc::clone(&engine), Arc::clone(&reg), qs, NOW).await;
                let adm = admitted(&res);
                assert_eq!(adm, conc, "P5c: all admit (independent counters)");
                // each asset counter holds exactly one amount.
                for i in 0..conc {
                    let s = engine
                        .spent_today(NET, &format!("musd{i}"), NOW)
                        .await
                        .unwrap();
                    assert_eq!(
                        s,
                        AtomicAmount::from_u128(AMOUNT),
                        "P5c: each independent counter exact"
                    );
                }
                for (ns, _) in &res {
                    hist.record(*ns).unwrap();
                }
                attempts += res.len();
                reservations += adm;
            }
        });
        report(
            &format!("independent diff-asset c{conc}"),
            &hist,
            attempts,
            conc,
            start.elapsed().as_secs_f64(),
            attempts,
            reservations,
            attempts - reservations,
            0,
            conc,
            conc,
            None,
            0,
            conc as u128 * AMOUNT,
            0,
            0,
            state_bytes(&path),
            0,
            approval_count(&path),
            &plabel,
        );
    }

    // =====================================================================
    // P5d — approval contention: many callers, SAME approval key.
    // =====================================================================
    println!("\n## P5d — approval contention (same approval key)");
    let reg = registry(0);
    for &conc in CONCURRENCY {
        let m = storms_for(conc);
        let placement = state_placement();
        let path = placement.dir.path().join("policy.json");
        let plabel = format!("{} (op-fs)", path.display());
        // Production profile: mock requires approval → the FIRST caller inserts
        // the pending record, the rest observe the identical already-pending
        // state. Same quote id across all callers ⇒ one approval key.
        let engine = Arc::new(SpendPolicyEngine::new(&path, SpendProfile::Production));
        let mut hist = new_hist();
        let mut attempts = 0usize;
        let mut approvals_created = 0usize;
        let start = Instant::now();
        rt.block_on(async {
            for s in 0..m {
                restore_state(&path, &[]);
                let shared = quote(&reg, "cap-approval", ASSET, AMOUNT, NOW + 1_000 * s as u64);
                let qs: Vec<_> = (0..conc).map(|_| shared.clone()).collect();
                let res = storm(Arc::clone(&engine), Arc::clone(&reg), qs, NOW).await;
                // No reservation (all held); one logical pending approval.
                assert_eq!(admitted(&res), 0, "approval contention: nothing reserves");
                let pending = engine.pending().await.unwrap();
                assert_eq!(
                    pending.len(),
                    1,
                    "exactly one logical pending approval, no duplicates"
                );
                let uniq: HashSet<_> = pending.into_iter().collect();
                assert_eq!(uniq.len(), 1);
                assert_eq!(
                    engine.spent_today(NET, ASSET, NOW).await.unwrap(),
                    AtomicAmount::from_u128(0),
                    "no premature reservation"
                );
                for (ns, _) in &res {
                    hist.record(*ns).unwrap();
                }
                attempts += res.len();
                approvals_created += 1;
            }
        });
        report(
            &format!("approval-contention c{conc}"),
            &hist,
            attempts,
            conc,
            start.elapsed().as_secs_f64(),
            attempts,
            0,
            attempts,
            approvals_created,
            1,
            1,
            None,
            0,
            0,
            0,
            0,
            state_bytes(&path),
            0,
            approval_count(&path),
            &plabel,
        );
    }

    // =====================================================================
    // P5e — housekeeping interaction (focused correctness group).
    // =====================================================================
    println!("\n## P5e — housekeeping interaction (correctness)");
    rt.block_on(async {
        let reg = registry(0);
        let placement = state_placement();
        let path = placement.dir.path().join("policy.json");
        let engine = SpendPolicyEngine::new(&path, SpendProfile::DevTest);

        // A live counter today + a stale counter from an earlier day.
        engine
            .check_and_reserve(&quote(&reg, "cap-a", ASSET, AMOUNT, NOW), &reg, NOW)
            .await
            .unwrap();
        let stale_day = NOW - 5 * NS_PER_DAY; // NOW ≈ day 11; keep it positive
        // Reserve on a stale day (its own counter key) to plant a stale row.
        let stale_engine = SpendPolicyEngine::new(&path, SpendProfile::DevTest);
        stale_engine
            .check_and_reserve(&quote(&reg, "cap-a", ASSET, AMOUNT, stale_day), &reg, stale_day)
            .await
            .unwrap();
        // Today's live counter is intact; the stale one exists.
        assert_eq!(
            engine.spent_today(NET, ASSET, NOW).await.unwrap(),
            AtomicAmount::from_u128(AMOUNT)
        );
        assert_eq!(
            engine.spent_today(NET, ASSET, stale_day).await.unwrap(),
            AtomicAmount::from_u128(AMOUNT)
        );

        // A hard-denied real-network txn far in the future: nothing reserves,
        // so the ONLY reason to write is housekeeping pruning the now-stale
        // counters. Build it directly (a real network, not the `quote` helper
        // which hardcodes the mock network).
        use std::os::unix::fs::MetadataExt as _;
        let future = NOW + 40 * NS_PER_DAY;
        let mut real = default_mock_registry(EntityKeypair::generate().entity_id().clone());
        real.assets.push(AssetEntry {
            id: AssetId::parse("eip155:8453/erc20:0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap(),
            x402_asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            decimals: 6,
            symbol: "USDC".into(),
            display_name: None,
            equivalence_class: None,
        });
        let real = Arc::new(real);
        let real_q = PaymentQuote::new(
            EntityKeypair::generate().entity_id().clone(),
            EntityKeypair::generate().entity_id().clone(),
            "cap-a",
            None,
            X402Carry::author(&PaymentRequirements {
                scheme: "exact".into(),
                network: "eip155:8453".into(),
                amount: AMOUNT.to_string(),
                asset: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
                max_timeout_seconds: 60,
                extra: None,
            })
            .unwrap(),
            real.reference().unwrap(),
            future,
            future + TTL,
        );

        // Before the prune: capture the inode.
        let ino_before = std::fs::metadata(&path).unwrap().ino();
        let d = engine.check_and_reserve(&real_q, &real, future).await.unwrap();
        assert!(matches!(d, SpendDecision::Denied { .. }), "denied result preserved");
        // The prune was dirty and persisted (inode moved on an otherwise-clean
        // denial), and the stale counter is gone.
        let ino_pruned = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(ino_before, ino_pruned, "dirty housekeeping transition persisted");
        assert_eq!(
            engine.spent_today(NET, ASSET, stale_day).await.unwrap(),
            AtomicAmount::from_u128(0),
            "exact stale rows removed"
        );

        // Now the state is clean: an equivalent denial must NOT rewrite.
        let d = engine.check_and_reserve(&real_q, &real, future).await.unwrap();
        assert!(matches!(d, SpendDecision::Denied { .. }));
        let ino_clean = std::fs::metadata(&path).unwrap().ino();
        assert_eq!(ino_pruned, ino_clean, "no repeated cleanup write once the state is clean");
        println!(
            "  housekeeping: stale pruned + persisted, denial preserved, no repeat cleanup write — OK"
        );
    });
}
