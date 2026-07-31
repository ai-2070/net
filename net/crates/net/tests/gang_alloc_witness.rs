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
use net::adapter::net::behavior::gang::{match_islands, MatchCriteria, NumericFilter, SelectionPolicy};
use net::adapter::net::identity::EntityKeypair;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    /// Counting is off by default so fixture construction — which
    /// allocates heavily and is not what any of these tests measure —
    /// stays out of every total.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.get() {
            ALLOCS.set(ALLOCS.get() + 1);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.get() {
            ALLOCS.set(ALLOCS.get() + 1);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Run `f` with allocation counting on, returning its allocation count.
fn count_allocs<R>(f: impl FnOnce() -> R) -> (usize, R) {
    ALLOCS.set(0);
    COUNTING.set(true);
    let out = f();
    COUNTING.set(false);
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
) -> (Fold<CapabilityFold>, Fold<IslandTopologyFold>, MatchCriteria) {
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
