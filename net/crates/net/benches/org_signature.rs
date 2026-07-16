//! OA-1 §1.6: the UNCACHED org-authority verification benchmark
//! (`ORG_CAPABILITY_AUTH_PLAN.md` — "Uncached verification +
//! bench"; the plan defers any verified-cert cache until this
//! measurement justifies one, and constants freeze only after
//! measurement).
//!
//! What is measured, over the realistic OA-1 objects:
//!
//! 1. `OrgMembershipCert::verify` — the per-cert cost every fold
//!    ingest pays for an announcement carrying `owner_cert`
//!    (domain-prefixed transcript rebuild + strict ed25519).
//!    Announcements re-broadcast every ~TTL/2 with identical
//!    certs, so this is the number a future cache would amortize.
//! 2. `OrgMembershipCert::try_issue` — the operator-side mint
//!    (occasional; included for completeness).
//! 3. `OrgRevocationBundle::verify` — the per-reload cost of an
//!    operator floors bundle at a realistic fleet size (64
//!    floors).
//! 4. Cert wire round-trip (`to_bytes` / `from_bytes`) — the
//!    156-byte fixed-offset codec.
//!
//! Run with: `cargo bench --features net --bench org_signature`

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::collections::BTreeMap;
use std::hint::black_box;

use net::adapter::net::behavior::org::{
    OrgKeypair, OrgMembershipCert, OrgRevocationBundle, ORG_CERT_TTL_SECS_RECOMMENDED,
};
use net::adapter::net::identity::EntityId;

fn org() -> OrgKeypair {
    OrgKeypair::from_bytes([0x42; 32])
}

fn member() -> EntityId {
    EntityId::from_bytes([0x24; 32])
}

fn cert() -> OrgMembershipCert {
    OrgMembershipCert::try_issue(&org(), member(), 5, ORG_CERT_TTL_SECS_RECOMMENDED).expect("issue")
}

/// A realistic operator bundle: 64 member floors (a mid-size
/// fleet's simultaneous rotation).
fn bundle() -> OrgRevocationBundle {
    let mut floors = BTreeMap::new();
    for i in 0..64u64 {
        let mut m = [0u8; 32];
        m[..8].copy_from_slice(&i.to_le_bytes());
        floors.insert(EntityId::from_bytes(m), (i % 7) as u32);
    }
    OrgRevocationBundle::try_issue(&org(), &floors).expect("issue")
}

fn bench_cert_verify(c: &mut Criterion) {
    // The fold-ingest hot path: uncached, every announcement with
    // a cert pays this in full.
    let cert = cert();
    let mut group = c.benchmark_group("org_cert_verify");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_op", |b| {
        b.iter(|| black_box(&cert).verify().unwrap());
    });
    group.finish();
}

fn bench_cert_issue(c: &mut Criterion) {
    let org = org();
    let member = member();
    let mut group = c.benchmark_group("org_cert_issue");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_op", |b| {
        b.iter(|| {
            OrgMembershipCert::try_issue(
                black_box(&org),
                member.clone(),
                5,
                ORG_CERT_TTL_SECS_RECOMMENDED,
            )
            .unwrap()
        });
    });
    group.finish();
}

fn bench_bundle_verify(c: &mut Criterion) {
    // The bundle-reload path: canonical-order re-check + one
    // ed25519 verify over the domain-prefixed payload.
    let bundle = bundle();
    let mut group = c.benchmark_group("org_bundle_verify_64_floors");
    group.throughput(Throughput::Elements(1));
    group.bench_function("single_op", |b| {
        b.iter(|| black_box(&bundle).verify().unwrap());
    });
    group.finish();
}

fn bench_cert_wire_round_trip(c: &mut Criterion) {
    let cert = cert();
    let bytes = cert.to_bytes();
    let mut group = c.benchmark_group("org_cert_wire");
    group.throughput(Throughput::Elements(1));
    group.bench_function("encode_decode", |b| {
        b.iter(|| {
            let encoded = black_box(&cert).to_bytes();
            OrgMembershipCert::from_bytes(black_box(&encoded)).unwrap()
        });
    });
    group.bench_function("decode_only", |b| {
        b.iter(|| OrgMembershipCert::from_bytes(black_box(&bytes)).unwrap());
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_cert_verify, bench_cert_issue, bench_bundle_verify, bench_cert_wire_round_trip
}
criterion_main!(benches);
