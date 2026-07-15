//! B1 — exact-proof provider admission (headline) + the rejection matrix,
//! and B2/B4/B5 (ready-settled gate, duplicate storms) land here too.
//!
//! Boundaries (see `docs/plans/PAYMENTS_BENCHMARKS_PLAN.md`):
//!   - boundary 2 (headline): quote+proof received → `accept_payment`
//!     completes → `redeem_for_invocation` admits the handler. Zero-delay
//!     mock facilitator, so what remains is the Net payment tax.
//!   - boundary 1: `redeem_for_invocation` alone on a ready-settled quote —
//!     "ready-settled invocation gate overhead."
//!
//! Per decision D3, the *successful* accept/redeem totals and the duplicate
//! storms are STATEFUL and single-use; they land on the custom hdrhistogram
//! harness in P2/P3 (fresh inputs per sample, store cardinality held fixed).
//!
//! This P1 cut lands only the two *stateless, repeatable* smoke bars that
//! validate the non-mesh harness end to end — one down each gate:
//!   - `reject_expired`      — `accept_payment` on an expired quote. The
//!     quote signature is verified, then expiry rejects BEFORE any state
//!     claim or facilitator call, so it is repeatable and bounded (payment
//!     admission is an adversarial public surface — rejection stays cheap).
//!   - `redeem_unknown_quote`— `redeem_for_invocation` for a quote id that
//!     was never issued: a structured `Denied`, no mutation.
//!
//! P2 adds the full eight-row rejection matrix (bad-sig / payload-mismatch /
//! verify-rejected / expired / already-served / replay / quote-already-paid
//! / in-progress) plus the boundary-1 and boundary-2 totals.
//!
//! Run: cargo bench -p net-payments --bench admission

use criterion::{criterion_group, criterion_main, Criterion};
use net_payments::core::verification::VerificationTier;

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{build_engine, issue, payload_for, runtime, AMOUNT, NOW, TOOL_ID, TTL_NS};

fn bench_admission(c: &mut Criterion) {
    let rt = runtime();
    let fx = build_engine();

    // An expired quote + its proof. accept_payment verifies the quote
    // signature then rejects on expiry before claiming any state, so this
    // never mutates the store and can be iterated freely.
    let expired = issue(&fx, AMOUNT, NOW);
    let expired_proof = payload_for(&expired);
    let way_past = NOW + TTL_NS + 86_400_000_000_000; // a day past expiry + tolerance

    let mut g = c.benchmark_group("admission_stateless");
    g.sample_size(50);

    // Down the acceptance gate: a cheap, bounded rejection.
    g.bench_function("reject_expired", |b| {
        b.to_async(&rt).iter(|| async {
            let d = fx
                .engine
                .accept_payment(&expired, &expired_proof, VerificationTier::Observed, way_past)
                .await
                .expect("accept_payment (expired)");
            std::hint::black_box(d);
        });
    });

    // Down the redemption gate: an unknown quote is a structured denial,
    // not a panic — repeatable, no state consumed.
    g.bench_function("redeem_unknown_quote", |b| {
        b.to_async(&rt).iter(|| async {
            let d = fx
                .engine
                .redeem_for_invocation(TOOL_ID, "no-such-quote", None)
                .await
                .expect("redeem_for_invocation (unknown)");
            std::hint::black_box(d);
        });
    });

    g.finish();
}

criterion_group!(benches, bench_admission);
criterion_main!(benches);
