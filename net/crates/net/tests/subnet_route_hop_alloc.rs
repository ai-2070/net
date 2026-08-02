//! The protected forwarding primitive does not allocate
//! (`docs/internal/plans/SUBNET_AUTH_PLAN.md` D6).
//!
//! Sealing a route-hop envelope runs once per packet per hop on a
//! relay. `seal` returning a `Vec` put an allocator round trip — and a
//! free, and a cold cache line — between a packet arriving and
//! leaving, for a buffer whose exact size is known before any work
//! starts. `seal_into` writes into a buffer the caller already owns.
//!
//! "Does not allocate" is a claim about the implementation, not about
//! the signature: a `seal_into` that internally built a `Vec` would
//! type-check identically and be exactly as slow. So this file counts
//! real allocator calls through a global allocator rather than
//! asserting the shape of the API.
//!
//! It lives in its own test binary deliberately, and holds exactly one
//! `#[test]`. A `#[global_allocator]` is process-wide, so a second test
//! running concurrently on another thread would charge its allocations
//! to this one's counter — under plain `cargo test` that is a flaky
//! failure, and under a process-per-test runner it is an invisible
//! difference in behaviour between the two. One binary, one test, no
//! ambiguity about whose allocation was whose.

#![cfg(feature = "net")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use net::adapter::net::subnet::route_hop::{open, seal, seal_into, sealed_len};
use net::adapter::net::RoutingHeader;

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

const KEY: [u8; 32] = [0x3C; 32];

fn header() -> RoutingHeader {
    RoutingHeader::new(0x0BAD_F00D_1234, 0xFEED, 8)
}

/// Seal a realistic forwarding load into a reused buffer, count
/// allocator calls across the steady state, and confirm that what the
/// reused buffer produced is still a correct envelope.
///
/// Cheapness and correctness are asserted together on purpose: a
/// `seal_into` that wrote nothing would pass the allocation half
/// perfectly.
#[test]
fn sealing_into_a_reused_buffer_allocates_nothing() {
    const PACKETS: usize = 512;
    // Spread of inner sizes a relay actually sees, including the two
    // ends: an empty inner packet and something near a 1500-byte MTU.
    const SIZES: [usize; 5] = [0, 64, 512, 1200, 1400];

    let inners: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&n| (0..n).map(|i| (i % 251) as u8).collect())
        .collect();

    // Size once, up front — exactly what a forwarder does when it
    // learns its MTU. This allocation is deliberately outside the
    // measured section.
    let mut buf = vec![0u8; sealed_len(*SIZES.iter().max().unwrap())];
    let hdr = header();

    // Warm up so any lazy one-time initialization inside the MAC or the
    // test harness is charged before the count starts.
    for inner in &inners {
        let n = seal_into(&mut buf, &KEY, 1, 0, &hdr, inner).expect("buffer was sized for this");
        assert!(n > 0);
    }

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let mut sealed_bytes = 0usize;
    let mut last_len = 0usize;
    for i in 0..PACKETS {
        let inner = &inners[i % inners.len()];
        let n = seal_into(&mut buf, &KEY, 7, i as u64, &hdr, inner)
            .expect("the buffer is sized for the largest packet");
        sealed_bytes += n;
        last_len = n;
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;

    assert!(
        sealed_bytes > 0,
        "guard against the loop being optimized into nothing",
    );
    assert_eq!(
        allocations, 0,
        "sealing {PACKETS} hops into a reused buffer must not allocate; saw {allocations}",
    );

    // Cheap is only interesting if it is also right. Everything below
    // may allocate freely — the count has already been taken.
    let last_inner = &inners[(PACKETS - 1) % inners.len()];
    let opened = open(&KEY, &buf[..last_len]).expect("the last sealed hop must verify");
    assert_eq!(opened.hop_session_id, 7);
    assert_eq!(opened.hop_sequence, (PACKETS - 1) as u64);
    assert_eq!(
        opened.inner,
        &last_inner[..],
        "a reused buffer must not leave a previous packet's tail behind",
    );
    assert_eq!(
        &buf[..last_len],
        &seal(&KEY, 7, (PACKETS - 1) as u64, &hdr, last_inner)[..],
        "the allocation-free path must not be a different wire format",
    );
}
