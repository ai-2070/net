//! SI-1d: the sign/verify benchmark gating the attestation
//! cadence-floor default (`SENSING_INTEREST_COALESCING_PLAN.md`,
//! v4.3 status block: "the sign/verify benchmark at the 50 ms
//! cadence floor + realistic fan-out — batching only if the numbers
//! justify it").
//!
//! What is measured, over a realistic attestation (a REAL
//! `InterestSpec` digest, a plausible capability id, `Ready` with
//! `estimated_start: Some(..)`, cadence promised at the floor):
//!
//! 1. `sign_attestation` — the per-(interest × branch) cost an
//!    origin pays every cadence tick (blake3 transcript digest +
//!    ed25519 sign).
//! 2. `verify_attestation` — the per-attestation cost at a consumer
//!    or admitting relay (transcript re-build + strict ed25519).
//! 3. postcard encode+decode round-trip — the 0x0C03 wire hot path.
//!
//! After the criterion groups, a derived headroom table prints: at
//! the 50 ms cadence floor one (interest × branch) stream costs 20
//! signs/sec at the origin, so streams-per-core = ops/sec ÷ 20.
//! Fan-out is delivery-only: a relay fanning one attestation to
//! 1, 8, or 64 subscribers still performs exactly ONE verify per
//! attestation (relays forward bytes identically — §4.4), so the
//! honest verify figure is verifies/sec/core, constant across
//! fan-out; the table reports it that way instead of inventing a
//! per-subscriber verify multiplier.
//!
//! Run with: `cargo bench --features net --bench sensing_signature`

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::hint::black_box;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::sensing::{
    decode_attestation, encode_attestation, sign_attestation, verify_attestation, AttestedStatus,
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, Incarnation,
    InterestSpec, ProviderSelector, ReadinessAttestation, ResultMode, StatusReason,
    UnsignedAttestation, WorkLatencyEnvelope, DEFAULT_ATTESTATION_CADENCE_FLOOR,
};
use net::adapter::net::EntityKeypair;

// ============================================================================
// Realistic fixture — one attestation as SI-3's origin emitter will
// actually mint it every cadence tick.
// ============================================================================

/// A plausible production interest: a document-print capability
/// under a couple of canonical constraints, owner-scoped, with a
/// start-within envelope. Its digest is the REAL 32-byte
/// capability-interest digest, not a stand-in.
fn spec() -> InterestSpec {
    InterestSpec {
        capability_id: CapabilityId::new("print.document"),
        constraints: CanonicalConstraints::from_entries([
            ("color", "true"),
            ("duplex", "long-edge"),
            ("media", "a4"),
        ])
        .unwrap(),
        work_latency: WorkLatencyEnvelope::start_within(Duration::from_secs(5)),
        providers: ProviderSelector::AnyAuthorized,
        result_mode: ResultMode::Any,
        disclosure_class: DisclosureClass::Owner,
        audience: AudienceScopeCommitment::from_bytes([0xA7; 32]),
    }
}

fn keypair() -> EntityKeypair {
    EntityKeypair::from_bytes([0x42; 32])
}

/// The unsigned attestation an origin emits at the cadence floor:
/// Ready, a provider-side start estimate present, promised cadence
/// exactly the 50 ms floor.
fn unsigned(origin: u64) -> UnsignedAttestation {
    UnsignedAttestation {
        interest_digest: spec().interest_digest(),
        origin,
        origin_incarnation: Incarnation::new(7),
        capability_id: CapabilityId::new("print.document"),
        capability_generation: 42,
        status: AttestedStatus::Ready,
        status_reason: StatusReason::None,
        estimated_start: Some(Duration::from_millis(800)),
        seq: 12_345,
        promised_cadence: DEFAULT_ATTESTATION_CADENCE_FLOOR,
        audience_scope: AudienceScopeCommitment::from_bytes([0xA7; 32]),
    }
}

fn signed() -> ReadinessAttestation {
    let keypair = keypair();
    sign_attestation(&keypair, unsigned(keypair.node_id())).unwrap()
}

// ============================================================================
// Criterion microbenchmarks
// ============================================================================

fn bench_sign(c: &mut Criterion) {
    // One sign per (interest × branch) per cadence tick — the
    // origin-side hot path SI-3 will drive. `iter_batched` pays the
    // UnsignedAttestation clone outside the timed region so the
    // number is blake3 transcript + ed25519 sign, nothing else.
    let keypair = keypair();
    let template = unsigned(keypair.node_id());
    let mut group = c.benchmark_group("sensing_attestation_sign");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_op", |b| {
        b.iter_batched(
            || template.clone(),
            |unsigned| sign_attestation(black_box(&keypair), unsigned).unwrap(),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_verify(c: &mut Criterion) {
    // One verify per received attestation — consumer or admitting
    // relay side. Rebuilds the transcript digest and runs strict
    // ed25519 verification.
    let keypair = keypair();
    let attestation = signed();
    let entity = keypair.entity_id();
    let mut group = c.benchmark_group("sensing_attestation_verify");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_op", |b| {
        b.iter(|| verify_attestation(black_box(&attestation), black_box(entity)).unwrap());
    });
    group.finish();
}

fn bench_wire_round_trip(c: &mut Criterion) {
    // postcard encode + strict decode of the 0x0C03 payload — the
    // wire hot path every relay hop pays (forwarding never re-signs
    // or re-verifies; it re-encodes at most).
    let attestation = signed();
    let mut group = c.benchmark_group("sensing_attestation_wire");
    group.throughput(Throughput::Elements(1));
    group.bench_function("encode_decode_round_trip", |b| {
        b.iter(|| {
            let bytes = encode_attestation(black_box(&attestation)).unwrap();
            decode_attestation(black_box(&bytes)).unwrap()
        });
    });
    group.finish();
}

// ============================================================================
// Derived headroom table — the SI-1d deliverable the plan's
// cadence-floor default is judged against.
// ============================================================================

/// Median per-op time from `SAMPLES` timed batches of
/// `OPS_PER_SAMPLE` calls each (median, not mean, so one scheduler
/// hiccup cannot skew the derived table).
fn median_op_time(mut op: impl FnMut()) -> Duration {
    const SAMPLES: usize = 101;
    const OPS_PER_SAMPLE: u32 = 64;
    // Warm-up pass.
    for _ in 0..OPS_PER_SAMPLE {
        op();
    }
    let mut samples: Vec<Duration> = (0..SAMPLES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..OPS_PER_SAMPLE {
                op();
            }
            start.elapsed() / OPS_PER_SAMPLE
        })
        .collect();
    samples.sort();
    samples[SAMPLES / 2]
}

fn ops_per_sec(op_time: Duration) -> f64 {
    1.0 / op_time.as_secs_f64()
}

/// Prints the cadence-floor headroom table. Registered as the last
/// criterion target so it rides `cargo bench --bench
/// sensing_signature` without a separate binary; it does its own
/// median timing rather than reaching into criterion's estimates.
fn report_cadence_headroom(_c: &mut Criterion) {
    let keypair = keypair();
    let template = unsigned(keypair.node_id());
    let attestation = signed();
    let entity = keypair.entity_id().clone();

    let sign_time = median_op_time(|| {
        black_box(sign_attestation(black_box(&keypair), template.clone()).unwrap());
    });
    let verify_time = median_op_time(|| {
        verify_attestation(black_box(&attestation), black_box(&entity)).unwrap();
    });
    let round_trip_time = median_op_time(|| {
        let bytes = encode_attestation(black_box(&attestation)).unwrap();
        black_box(decode_attestation(black_box(&bytes)).unwrap());
    });

    // One (interest × branch) stream at the floor = one attestation
    // per floor period.
    let floor = DEFAULT_ATTESTATION_CADENCE_FLOOR;
    let signs_per_stream = 1.0 / floor.as_secs_f64();
    let sign_rate = ops_per_sec(sign_time);
    let verify_rate = ops_per_sec(verify_time);
    let sign_streams = sign_rate / signs_per_stream;
    let verify_streams = verify_rate / signs_per_stream;

    println!();
    println!("=== SI-1d derived headroom @ {floor:?} cadence floor ===");
    println!(
        "one (interest x branch) stream costs {signs_per_stream:.0} attestations/sec at the floor"
    );
    println!(
        "sign:       {:>9.2} us/op -> {:>10.0} signs/sec/core   -> {:>8.0} concurrent streams/core",
        sign_time.as_secs_f64() * 1e6,
        sign_rate,
        sign_streams,
    );
    println!(
        "verify:     {:>9.2} us/op -> {:>10.0} verifies/sec/core -> {:>8.0} concurrent streams/core",
        verify_time.as_secs_f64() * 1e6,
        verify_rate,
        verify_streams,
    );
    println!(
        "round-trip: {:>9.2} us/op (postcard encode + strict decode)",
        round_trip_time.as_secs_f64() * 1e6,
    );
    println!("fan-out (relay verify load per attested stream, verifies/sec):");
    for fan_out in [1u32, 8, 64] {
        println!(
            "  {fan_out:>3} subscribers -> {signs_per_stream:.0} verifies/sec \
             (ONE verify per attestation; fan-out multiplies delivery, not verification)"
        );
    }
    println!();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets =
        bench_sign,
        bench_verify,
        bench_wire_round_trip,
        report_cadence_headroom,
}
criterion_main!(benches);
