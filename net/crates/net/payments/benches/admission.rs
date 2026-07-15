//! Admission diagnostics (Criterion) — two repeatable microbenchmarks that
//! validate the non-mesh harness and give a quick crypto-vs-store signal.
//!
//! **These are diagnostics, not public results.** Criterion reports a
//! bootstrap confidence interval, not a per-op p50/p95/p99, so it cannot
//! satisfy the plan's public-output contract; every *published* payment
//! number goes through the custom histogram harness + `BenchMetadata::report`
//! (see `redeem_matrix.rs`). Criterion stays for repeatable diagnostics.
//!
//! Two bars, one down each gate:
//!   - `reject_expired`      — `accept_payment` on an expired quote. The
//!     quote signature is verified, then expiry rejects BEFORE any state
//!     access or facilitator call — a genuine *pre-state* crypto diagnostic,
//!     repeatable and bounded (payment admission is an adversarial public
//!     surface; rejection stays cheap).
//!   - `redeem_unknown_quote`— `redeem_for_invocation` for an id that was
//!     never issued: a *repeatable logical denial*. It is NOT stateless — it
//!     loads + parses the durable store to look the id up (finds nothing);
//!     post the write-amplification fix it does not *write*. So it is
//!     repeatable, but its cost is a store read, not pure logic.
//!
//! The full, stateful admission story — the boundary-2 accept+redeem totals,
//! the boundary-1 ready-settled gate, the eight-row rejection matrix, and the
//! duplicate storms — lives on the custom harness (P2/P3), where store
//! cardinality is held constant per sample via snapshot/restore.
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
    // signature then rejects on expiry before any state access, so this
    // never touches the store and can be iterated freely.
    let expired = issue(&fx, AMOUNT, NOW);
    let expired_proof = payload_for(&expired);
    let way_past = NOW + TTL_NS + 86_400_000_000_000; // a day past expiry + tolerance

    let mut g = c.benchmark_group("admission_diagnostics");
    g.sample_size(50);

    // Down the acceptance gate: a cheap, bounded, pre-state rejection.
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

    // Down the redemption gate: an unknown quote is a structured denial. It
    // reads (loads + parses) the store but, post-fix, does not write — a
    // repeatable logical denial.
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
