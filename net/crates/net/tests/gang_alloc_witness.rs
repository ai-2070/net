//! Allocation witnesses for PERF_AUDIT_2026_07_31_GANG_SCHEDULER §1 and §6.
//!
//! The measurement contract in that audit requires *allocation counts*,
//! not just wall-clock, for the findings whose claim is "this allocates
//! and then throws the allocation away". Wall-clock under-reports them:
//! an allocator with a warm free list makes a discarded clone look
//! cheap on a quiet benchmark machine and expensive under real
//! fragmentation, so the count is the load-bearing number.
//!
//! Lives in its own integration binary because it installs a
//! `#[global_allocator]`. Counting is **per-thread** (a `Cell` in TLS,
//! const-initialized so the TLS access itself never allocates), so the
//! harness's other threads cannot pollute a measurement region.
//!
//! These are mechanism witnesses, not thresholds — see ICB-7 for
//! anything that makes a public performance claim.

#![cfg(feature = "net")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use net::adapter::net::behavior::fold::{
    CapabilityFilter, CapabilityFold, CapabilityMembership, CapabilityQuery, EnvelopeMeta, Fold,
    FoldKind, IslandRecord, IslandTopologyFold, NodeState, SignedAnnouncement, UnitSet,
};
use net::adapter::net::behavior::gang::{
    match_islands, match_islands_sensed, MatchCriteria, NumericFilter, SelectionPolicy,
};
use net::adapter::net::identity::EntityKeypair;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    /// Counting is off by default so fixture construction — which
    /// allocates heavily and is not what any of these tests measure —
    /// stays out of every total.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

// SAFETY: every allocation call is forwarded verbatim to `System`,
// which upholds the `GlobalAlloc` contract; this type adds only a
// counter increment and never inspects, retains, or derives a pointer.
// The counter is a `Cell` in thread-local storage, so incrementing it
// cannot allocate (const-initialized — no lazy init on first touch)
// and cannot re-enter the allocator, which is what would otherwise
// make a counting `GlobalAlloc` unsound.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.get() {
            ALLOCS.set(ALLOCS.get() + 1);
        }
        // SAFETY: `layout` is forwarded unchanged from a caller that
        // already owes `System` the same validity guarantee.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `Self::alloc`/`Self::realloc`, which
        // return `System`'s pointers unmodified, so `System` is the
        // rightful owner and `layout` is the one it was allocated with.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.get() {
            ALLOCS.set(ALLOCS.get() + 1);
        }
        // SAFETY: as `dealloc` — `ptr` and `layout` are `System`'s own,
        // and `new_size` is forwarded unchanged from the caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Disarms counting on drop, so a panic inside a measured closure
/// cannot leave the thread's counter armed for whatever runs next.
///
/// Not merely tidy: a `#[test]` that fails while counting is on would
/// charge the harness's own unwinding — and any later measurement
/// sharing the thread — to a counter nobody reset, turning one real
/// failure into a second, misleading one. Cheap enough to be
/// unconditional.
struct CountingGuard;

impl Drop for CountingGuard {
    fn drop(&mut self) {
        COUNTING.set(false);
    }
}

/// Run `f` with allocation counting on, returning its allocation count.
fn count_allocs<R>(f: impl FnOnce() -> R) -> (usize, R) {
    ALLOCS.set(0);
    COUNTING.set(true);
    let _guard = CountingGuard;
    let out = f();
    drop(_guard);
    (ALLOCS.get(), out)
}

fn new_fold<K: FoldKind>() -> Fold<K> {
    Fold::with_sweep_interval(Duration::ZERO)
}

/// A matcher fixture: `hosts` publishers, each carrying `tags_per_host`
/// capability tags and hosting one 8-unit island.
fn fixture(
    hosts: usize,
    tags_per_host: usize,
) -> (
    Fold<CapabilityFold>,
    Fold<IslandTopologyFold>,
    MatchCriteria,
) {
    let caps: Fold<CapabilityFold> = new_fold();
    let topo: Fold<IslandTopologyFold> = new_fold();

    for i in 0..hosts {
        let kp = EntityKeypair::generate();
        let node = kp.entity_id().node_id();

        // Every host carries the match tag plus `tags_per_host - 1`
        // filler tags, and a metadata map — the payload bulk a clone
        // would have to duplicate.
        let mut tags = vec!["gpu:h100".to_string()];
        for t in 1..tags_per_host {
            tags.push(format!("filler:{i}:{t}"));
        }
        let mut metadata = BTreeMap::new();
        for t in 0..tags_per_host {
            metadata.insert(format!("k{t}"), format!("v{t}"));
        }
        let membership = CapabilityMembership {
            class_hash: 0x67_70_75,
            tags,
            hardware: None,
            state: NodeState::Idle,
            region: Some("us-east".into()),
            price_quote: None,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
            metadata,
            owner: None,
        };
        caps.apply(
            SignedAnnouncement::sign(
                &kp,
                CapabilityFold::KIND_ID,
                membership.class_hash,
                node,
                1,
                EnvelopeMeta::default(),
                membership,
            )
            .expect("sign cap"),
        )
        .expect("apply cap");

        let record = IslandRecord {
            id: 0xA000 + i as u64,
            units: UnitSet::new((0..8).collect()),
            host: node,
            capabilities: vec!["model:a1".into()],
            load: 0.1,
            p50_latency_us: 1_000,
        };
        topo.apply(
            SignedAnnouncement::sign(
                &kp,
                IslandTopologyFold::KIND_ID,
                0,
                node,
                1,
                EnvelopeMeta::default(),
                record,
            )
            .expect("sign island"),
        )
        .expect("apply island");
    }

    let criteria = MatchCriteria {
        capability: CapabilityQuery::Composite(CapabilityFilter {
            tags_all: vec!["gpu:h100".into()],
            ..Default::default()
        }),
        numeric: NumericFilter {
            min_units: 8,
            ..Default::default()
        },
        selection: SelectionPolicy::LeastLoaded,
        prefer_capability: None,
    };
    (caps, topo, criteria)
}

/// §1: the matcher's allocation count must be independent of
/// **capability payload size**.
///
/// Step 1 keeps one `u64` per matched host. Pre-fix it got that `u64`
/// by deep-cloning the host's whole `CapabilityMembership` — tags Vec,
/// metadata BTreeMap, allow-lists — so matcher allocations grew with
/// tags-per-host even though the matcher's *output* is identical. This
/// holds the host count fixed and scales only the payload: any
/// remaining growth is a payload clone that survived.
///
/// Asserted as a ratio rather than an exact count so it does not break
/// on an unrelated allocation elsewhere in the pipeline.
#[test]
fn matcher_allocations_do_not_scale_with_capability_payload_size() {
    const HOSTS: usize = 50;
    let down: HashSet<u64> = HashSet::new();

    let (caps_lean, topo_lean, crit_lean) = fixture(HOSTS, 1);
    let (caps_fat, topo_fat, crit_fat) = fixture(HOSTS, 40);

    // Warm any lazily-initialized state so it lands outside the count.
    let _ = match_islands(&caps_lean, &topo_lean, &crit_lean, &down);
    let _ = match_islands(&caps_fat, &topo_fat, &crit_fat, &down);

    let (lean_allocs, lean_out) =
        count_allocs(|| match_islands(&caps_lean, &topo_lean, &crit_lean, &down));
    let (fat_allocs, fat_out) =
        count_allocs(|| match_islands(&caps_fat, &topo_fat, &crit_fat, &down));

    // Same work, same answer — only the payload bulk differs.
    assert_eq!(lean_out.len(), HOSTS, "lean fixture must match every host");
    assert_eq!(fat_out.len(), HOSTS, "fat fixture must match every host");

    // 40x the tags and 40x the metadata entries per host. A surviving
    // payload clone would show up as a large multiple here; the
    // allowance covers ordinary jitter, not a per-host deep clone.
    assert!(
        fat_allocs <= lean_allocs * 2,
        "matcher allocations scaled with payload size: {lean_allocs} (1 tag/host) \
         -> {fat_allocs} (40 tags/host) over {HOSTS} hosts — a payload clone survived",
    );

    println!(
        "§1 matcher allocations over {HOSTS} hosts: {lean_allocs} (1 tag) vs \
         {fat_allocs} (40 tags)",
    );
}

/// Review §8: a panic inside a measured closure must not leave the
/// thread's counter armed for whatever runs next.
///
/// Runs first in file order but the ordering is not relied on — the
/// point is the assertion after the unwind, plus the demonstration
/// that a following measurement is unpolluted.
#[test]
fn a_panicking_measurement_does_not_leave_counting_armed() {
    // The default hook would print a panic + backtrace note for a
    // panic this test is deliberately causing.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        count_allocs(|| panic!("deliberate panic inside a measured closure"));
    }));
    std::panic::set_hook(hook);

    assert!(outcome.is_err(), "the closure must actually have panicked");
    assert!(
        !COUNTING.get(),
        "counting stayed armed through an unwind — the next measurement on this \
         thread would be charged for work it never ran",
    );

    // And the counter is usable again: one `Vec` allocation, from a
    // counter that was reset rather than carried over.
    let (allocs, v) = count_allocs(|| vec![0u8; 64]);
    assert_eq!(v.len(), 64);
    assert_eq!(
        allocs, 1,
        "a measurement after an unwind must start from a clean counter",
    );
}

/// §3: with no sensed evidence, the sensed matcher must cost EXACTLY
/// what `match_islands` costs — not one allocation more.
///
/// The audit's §3 contract calls the empty-delta path "byte-identical
/// to `match_islands`: absence of evidence never prunes and never
/// reorders". That is a statement about the *result*, and it held
/// before this test existed; the cost was a separate matter. The
/// single-snapshot rewrite briefly built the island → host band map
/// before the early return, so this path allocated one throwaway
/// `HashMap` sized to the whole candidate set — the same
/// build-it-then-discard-it shape §3 was opened against, reintroduced
/// on the more common branch while removing the second topology scan
/// from the rarer one.
///
/// Equality, not a ratio: with `sensed_non_viable` empty the prune set
/// is `Cow::Borrowed`, so on this path the sensed matcher does
/// literally the same work through the same calls. Any delta at all is
/// a discarded allocation, which is exactly what is being pinned.
#[test]
fn sensed_match_with_no_evidence_allocates_exactly_what_the_plain_matcher_does() {
    const HOSTS: usize = 50;
    let down: HashSet<u64> = HashSet::new();
    let non_viable: HashSet<u64> = HashSet::new();

    let (caps, topo, crit) = fixture(HOSTS, 4);

    // Warm any lazily-initialized state so it lands outside the count.
    let _ = match_islands(&caps, &topo, &crit, &down);
    let _ = match_islands_sensed(&caps, &topo, &crit, &down, &non_viable, &[]);

    let (plain_allocs, plain_out) = count_allocs(|| match_islands(&caps, &topo, &crit, &down));
    let (sensed_allocs, sensed_out) =
        count_allocs(|| match_islands_sensed(&caps, &topo, &crit, &down, &non_viable, &[]));

    // The result equality the §3 contract already promises — asserted
    // here too so a regression cannot satisfy the cost check by
    // returning less.
    assert_eq!(sensed_out, plain_out, "empty sensed delta must not reorder");
    assert_eq!(
        sensed_out.len(),
        HOSTS,
        "the fixture must not be degenerate",
    );

    assert_eq!(
        sensed_allocs, plain_allocs,
        "the no-evidence sensed path allocated {sensed_allocs} vs the plain \
         matcher's {plain_allocs} over {HOSTS} hosts — it is building something \
         it never reads",
    );

    println!("§3 no-evidence sensed match: {sensed_allocs} allocs vs plain {plain_allocs}");
}

/// §6: rejecting the all-zero placeholder signature must allocate
/// **nothing**.
///
/// Pre-fix the check was `self.signature == placeholder_signature()`,
/// and `placeholder_signature()` is `vec![0u8; 64]` — one heap
/// allocation per verify, on the inbound dispatch path, purely to
/// compare against zero. The sentinel arm returns before any signing-
/// bytes work, so post-fix the whole call is allocation-free and the
/// witness is exact rather than a ratio.
#[test]
fn rejecting_a_placeholder_signature_allocates_nothing() {
    let kp = EntityKeypair::generate();
    let node = kp.entity_id().node_id();
    let record = IslandRecord {
        id: 0xA0,
        units: UnitSet::new((0..8).collect()),
        host: node,
        capabilities: vec!["model:a1".into()],
        load: 0.1,
        p50_latency_us: 1_000,
    };
    let ann = SignedAnnouncement::placeholder(
        IslandTopologyFold::KIND_ID,
        0,
        node,
        1,
        EnvelopeMeta::default(),
        record,
    );

    // Warm-up outside the count.
    let _ = ann.verify(kp.entity_id());

    let (allocs, outcome) = count_allocs(|| ann.verify(kp.entity_id()));
    assert!(outcome.is_err(), "a placeholder signature must be rejected");
    assert_eq!(
        allocs, 0,
        "rejecting the placeholder sentinel must not allocate (pre-fix: one \
         throwaway 64-byte Vec per verify)",
    );
}
