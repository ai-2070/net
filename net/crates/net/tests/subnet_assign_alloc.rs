//! `SubnetPolicy::assign` allocates one view, not one `String` per tag
//! (`PERF_AUDIT_2026_08_04_SUBNET_PATHS.md` §2).
//!
//! `assign` runs on announcement ingest — once for every
//! signature-verified direct announcement, i.e. at gossip rate times
//! mesh size. It delegates to a rule evaluator documented as
//! allocation-free *precisely because* the scoped-discovery path runs
//! that evaluator under the capability fold's read locks. The wrapper
//! then rendered every tag to an owned `String` ahead of it, which is
//! pure copying for the variant subnet rules actually match: an
//! operator rule keys on `region:` / `fleet:`-shaped prefixes, and
//! those parse to `Tag::Legacy(String)`, whose wire form is already in
//! hand.
//!
//! "Borrows instead of rendering" is a claim about the implementation,
//! not about the signature: a `Cow`-returning `as_wire` that always
//! took the `Owned` arm would type-check identically and be exactly as
//! slow. So this file counts real allocator calls rather than asserting
//! the shape of the API.
//!
//! It lives in its own test binary deliberately, and holds exactly one
//! `#[test]` — same rationale as `subnet_route_hop_alloc.rs`: a
//! `#[global_allocator]` is process-wide, so a second test running
//! concurrently would charge its allocations to this one's counter.

#![cfg(feature = "net")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use net::adapter::net::behavior::capability::CapabilitySet;
use net::adapter::net::{SubnetId, SubnetPolicy, SubnetRule};

/// Allocation counter. Only ever read while a single test thread is
/// running the measured section.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: every method forwards directly to `System`, which upholds the
// `GlobalAlloc` contract; the only addition is a relaxed counter
// increment, which touches no allocator state and cannot affect the
// returned pointers.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `layout` is forwarded unchanged from our caller,
        // which is required to have supplied a valid one.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` come from a matching `alloc` in this
        // same allocator, which forwarded to `System`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as `dealloc`, plus `new_size` is forwarded unchanged
        // from a caller required to have checked it.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

/// A three-level operator policy of the shape deployments actually
/// write.
fn policy() -> SubnetPolicy {
    SubnetPolicy::new()
        .add_rule(SubnetRule::new("region:", 0).map("us", 3).map("eu", 4))
        .add_rule(SubnetRule::new("fleet:", 1).map("blue", 7).map("green", 8))
        .add_rule(SubnetRule::new("unit:", 2).map("alpha", 2))
}

#[test]
fn assign_allocates_one_view_regardless_of_tag_count() {
    let policy = policy();

    // Two announcements from the same fleet shape: one carrying three
    // tags, one carrying sixteen. Every tag is a `region:` / `fleet:` /
    // `unit:` / plain-token shape, i.e. `Tag::Legacy` — what an operator
    // rule matches on, and what ingest overwhelmingly carries.
    let small = CapabilitySet::new()
        .add_tag("region:us")
        .add_tag("fleet:blue")
        .add_tag("unit:alpha");

    let mut large = CapabilitySet::new()
        .add_tag("region:us")
        .add_tag("fleet:blue")
        .add_tag("unit:alpha");
    for i in 0..13 {
        large = large.add_tag(format!("filler-tag-{i}"));
    }

    // Warm any lazily-initialized state the first call might touch, so
    // the measured sections compare like with like.
    let _ = policy.assign(&small);
    let _ = policy.assign(&large);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let small_verdict = policy.assign(&small);
    let small_allocs = ALLOCATIONS.load(Ordering::Relaxed) - before;

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let large_verdict = policy.assign(&large);
    let large_allocs = ALLOCATIONS.load(Ordering::Relaxed) - before;

    // Correctness first — an allocation witness over a function that
    // stopped working is worthless.
    assert_eq!(small_verdict, SubnetId::new(&[3, 7, 2]));
    assert_eq!(large_verdict, SubnetId::new(&[3, 7, 2]));

    // The load-bearing assertion: the count does NOT scale with the tag
    // count. Before the fix this was one `String` per tag plus the
    // `Vec`, so sixteen tags cost thirteen more allocations than three.
    assert_eq!(
        small_allocs, large_allocs,
        "assign must not allocate per tag: {small_allocs} allocations for 3 tags \
         vs {large_allocs} for 16 — a per-tag render is back",
    );

    // And the absolute floor: exactly the one borrowed-view `Vec`.
    assert_eq!(
        small_allocs, 1,
        "assign should allocate exactly the one Cow view vector, got {small_allocs}",
    );
}
