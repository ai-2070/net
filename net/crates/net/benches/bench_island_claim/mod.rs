//! ICB shared harness — ICB-LOCAL, deliberately separate from the
//! closed CPB `bench_mesh_pair` (plan D1 / item 12). `#[path]`-included
//! by every `island_claim_*` bench; Cargo gives each bench binary its
//! own copy.
//!
//! Everything rides the crate's PUBLIC `net::` API — the same
//! `MeshNode::new` + `accept`/`connect` dance `tests/common::connect_pair`
//! uses, plus the public reservation / fold-router surface. No
//! production API, arbitration, tie-breaking, rebroadcast, or
//! convergence behavior is added (Kyra ICB-0 constraint).
//!
//! # Measurement discipline (ICB v0.3 plan)
//!
//! - **The `Reserved` cross-node path does not converge** —
//!   `ReservationFold::merge` is arrival-order-dependent across
//!   publishers (`reservation.rs`), so ICB reports *divergence*, never
//!   a converged holder (E2).
//! - **`reserve_island` awaits the fan-out before returning** — its
//!   `Won` return is NOT local-commit latency (E1). ICB-2 stops the
//!   local timer on an independent exact-holder read.
//! - **Fold broadcast reaches direct peers only; the inbound
//!   `SUBPROTOCOL_FOLD` router does NOT rebroadcast** — an `A↔R↔B`
//!   chain does not deliver A's reservation to B (E3). The routed row
//!   is emitted only if a delivery preflight proves it.
//! - **Rejected merges emit no fold-watch event** — an exact-holder
//!   watcher cannot prove all competitors were processed, so the
//!   [`CountingRouter`] is the delivery barrier (counts every VERIFIED
//!   dispatch outcome: `Inserted` / `Replaced` / `Rejected`).
//!
//! See `docs/plans/ISLAND_CLAIM_BENCHMARK_PLAN.md`.

#![allow(dead_code)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use tokio::sync::watch;

use net::adapter::net::behavior::capability::{CapabilityFilter, CapabilitySet};
use net::adapter::net::behavior::fold::{
    ApplyOutcome, DispatchError, EnvelopeMeta, Fold, FoldChannelRouter, FoldKind, FoldRegistry,
    FoldStats, ReservationAnnouncement, ReservationFold, ReservationQuery, ReservationState,
    SignedAnnouncement,
};
use net::adapter::net::behavior::gang::ClaimOutcome;
use net::adapter::net::{EntityId, EntityKeypair, MeshNode, MeshNodeConfig};

/// Shared PSK across every ICB node (matches the CPB / chaos harness).
pub const PSK: [u8; 32] = [0x42u8; 32];

/// Worker count reported in the sample protocol.
pub const WORKER_THREADS: usize = 4;

/// Multi-threaded runtime for the transport path (copied narrow
/// generic helper — the ICB harness stays self-contained).
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Wall-clock micros since the epoch — the `Reserved` deadline space.
/// Benches run real std code (unlike the workflow sandbox).
pub fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_micros() as u64
}

/// A takeover deadline far beyond any ICB observation window — so the
/// `until_unix_us` takeover path (ICB-6) never fires inside an
/// uncontended / divergence sample and confounds the measurement.
pub const FAR_DEADLINE_US: u64 = 3_600_000_000; // +1 h

/// Far-future deadline as an absolute timestamp.
pub fn far_deadline() -> u64 {
    now_us() + FAR_DEADLINE_US
}

// ============================================================================
// Node + topology builders (public API only; reservations need a
// started transport + a pinned publisher entity, so every builder warms
// capabilities before any reservation flows).
// ============================================================================

fn mesh_config() -> MeshNodeConfig {
    let addr: SocketAddr = "127.0.0.1:0".parse().expect("bind addr");
    // Zero the announce debounce / rate-limit floor so warm-up (a
    // single capability announce per node) is not delayed. Reservations
    // ride `publish_fold_broadcast`, not the capability announcer, so
    // these knobs never touch the measured path.
    MeshNodeConfig::new(addr, PSK)
        .with_announce_debounce(Duration::ZERO)
        .with_min_announce_interval(Duration::ZERO)
}

/// Build one node (not yet started).
pub async fn node() -> Arc<MeshNode> {
    Arc::new(
        MeshNode::new(EntityKeypair::generate(), mesh_config())
            .await
            .expect("MeshNode::new"),
    )
}

/// Connect A→B via the handshake + accept pattern (replica of
/// `tests/common::connect_pair`; public-API only). Neither node started.
pub async fn connect(a: &Arc<MeshNode>, b: &Arc<MeshNode>) {
    let a_id = a.node_id();
    let b_pub = *b.public_key();
    let b_addr = b.local_addr();
    let b_id = b.node_id();
    let b_clone = b.clone();
    let accept = tokio::spawn(async move { b_clone.accept(a_id).await });
    a.connect(b_addr, &b_pub, b_id).await.expect("connect");
    accept.await.expect("accept task panicked").expect("accept");
}

/// A↔B direct, both started, warmed so each has pinned the other's
/// entity (required for reservation-signature verification).
pub async fn pair() -> (Arc<MeshNode>, Arc<MeshNode>) {
    let a = node().await;
    let b = node().await;
    connect(&a, &b).await;
    a.start_arc();
    b.start_arc();
    warm_all(&[a.clone(), b.clone()]).await;
    (a, b)
}

/// A full logical mesh of `n` distinct transport-connected nodes — the
/// construction distributed contention REQUIRES (every claimant must
/// receive every other claimant's reservation; item 3). Reports its own
/// logical session count via [`logical_sessions`]. All warmed.
pub async fn full_mesh(n: usize) -> Vec<Arc<MeshNode>> {
    let mut nodes = Vec::with_capacity(n);
    for _ in 0..n {
        nodes.push(node().await);
    }
    for i in 0..n {
        for j in (i + 1)..n {
            connect(&nodes[i], &nodes[j]).await;
        }
    }
    for nd in &nodes {
        nd.start_arc();
    }
    warm_all(&nodes).await;
    nodes
}

/// Logical (direct) session count of a full mesh of `n` nodes — the
/// number the distributed-view matrix reports (a full view needs the
/// full graph; item 3).
pub fn logical_sessions(n: usize) -> usize {
    n * n.saturating_sub(1) / 2
}

/// Warm every ordered pair: each node announces one sentinel manifest,
/// then we wait (bounded) until every node's capability fold exposes
/// every other node — establishing the mutual entity pins reservation
/// verification needs. Panics on non-convergence so a broken topology
/// fails loud.
pub async fn warm_all(nodes: &[Arc<MeshNode>]) {
    for (i, nd) in nodes.iter().enumerate() {
        nd.announce_capabilities(CapabilitySet::new().add_tag(format!("icb:warm:{i}")))
            .await
            .expect("warm announce");
    }
    for nd in nodes {
        for other in nodes {
            if Arc::ptr_eq(nd, other) {
                continue;
            }
            let oid = other.node_id();
            let ok = wait_until(Duration::from_secs(10), || {
                nd.find_nodes_by_filter(&permissive()).contains(&oid)
            })
            .await;
            assert!(ok, "warm_all: a node never learned a peer within 10s");
        }
    }
}

/// Directional warm: `src` announces a sentinel; wait until `dst` learns
/// `src`. Used where only one direction of the pin matters (e.g. a
/// routed chain where we prove `dst` KNOWS `src`'s capabilities yet
/// still never receives `src`'s reservation).
pub async fn warm_pair(src: &Arc<MeshNode>, dst: &Arc<MeshNode>) {
    src.announce_capabilities(CapabilitySet::new().add_tag(format!("icb:warm:{}", src.node_id())))
        .await
        .expect("warm announce");
    let sid = src.node_id();
    let ok = wait_until(Duration::from_secs(10), || {
        dst.find_nodes_by_filter(&permissive()).contains(&sid)
    })
    .await;
    assert!(ok, "warm_pair: dst never learned src within 10s");
}

/// Poll `cond` until true or `limit` elapses (returns whether it held).
/// One-time topology / preflight use only — never inside a timed region.
pub async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    cond()
}

/// A permissive capability filter — matches every publisher in the fold.
pub fn permissive() -> CapabilityFilter {
    CapabilityFilter::new()
}

// ============================================================================
// Exact-holder endpoint — the reservation analogue of CPB's
// `await_capability_state`. Stops only after an EXACT holder read
// succeeds, never at the bare `changed()` wake (E1; `signal_changed()`
// runs under the fold write lock).
// ============================================================================

/// The holder of `island` in this reservation fold, if any. `Free` and
/// "no entry" both read as `None`.
pub fn holder_of(fold: &Arc<Fold<ReservationFold>>, island: u64) -> Option<u64> {
    fold.query(ReservationQuery::State(island))
        .first()
        .and_then(|(_, state)| state.holder())
}

/// Await `island`'s holder becoming exactly `expected`, driven by the
/// missed-wakeup-safe fold watch. Checks the predicate FIRST (the change
/// may already be visible), then parks on `changed()`. Returns only
/// after the exact read matches — an unrelated wake just re-parks.
pub async fn await_reservation_holder(
    rx: &mut watch::Receiver<u64>,
    fold: &Arc<Fold<ReservationFold>>,
    island: u64,
    expected: u64,
) {
    loop {
        if holder_of(fold, island) == Some(expected) {
            return;
        }
        rx.changed().await.expect("fold sender alive");
    }
}

/// Await `island` reading as unheld (`Free` or swept) on this fold —
/// the fixture-reset endpoint (release then await exact Free).
pub async fn await_reservation_free(
    rx: &mut watch::Receiver<u64>,
    fold: &Arc<Fold<ReservationFold>>,
    island: u64,
) {
    loop {
        if holder_of(fold, island).is_none() {
            return;
        }
        rx.changed().await.expect("fold sender alive");
    }
}

/// Fixture reset for an UNCONTENDED sample: the holder releases, then we
/// require exact `Free` on the holder's own fold AND every relevant
/// observer within `timeout` — FAIL-LOUD. A failed release or a stuck
/// observer must abort the sample, never silently contaminate the next
/// one. Distributed-race samples must use FRESH island ids instead
/// (item 11), since divergent local views are not releasable by one holder.
pub async fn release_and_await_free(
    holder: &Arc<MeshNode>,
    observers: &[&Arc<MeshNode>],
    island: u64,
    timeout: Duration,
) {
    let outcome = holder
        .release_island(island)
        .await
        .expect("release transport/API");
    assert_eq!(
        outcome,
        ClaimOutcome::Won,
        "fixture holder must successfully release island {island:#x}"
    );
    // The holder's OWN fold must reach exact Free, whether or not the
    // caller listed it among `observers`.
    await_free_or_panic(holder, island, timeout).await;
    for obs in observers {
        await_free_or_panic(obs, island, timeout).await;
    }
}

/// Await exact `Free` for `island` on `node`'s reservation fold within
/// `timeout`, or panic (fail-loud reset).
async fn await_free_or_panic(node: &Arc<MeshNode>, island: u64, timeout: Duration) {
    let mut rx = node.reservation_fold().subscribe_changes();
    tokio::time::timeout(
        timeout,
        await_reservation_free(&mut rx, node.reservation_fold(), island),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "node {} did not reach exact Free for island {island:#x} within {timeout:?}",
            node.node_id()
        )
    });
}

// ============================================================================
// Signed reservation fixtures — for the counting-router witnesses (craft
// exact wire envelopes) and for exact-read tests against a local fold.
// ============================================================================

/// A signed `Reserved{holder = kp}` envelope for `island` at
/// `generation`, under `kp`'s own node id (so `verify` binds the
/// envelope's `node_id` to the publisher — `generation ≥ 1`).
pub fn reserve_ann(
    kp: &EntityKeypair,
    island: u64,
    generation: u64,
) -> SignedAnnouncement<ReservationAnnouncement> {
    SignedAnnouncement::sign(
        kp,
        ReservationFold::KIND_ID,
        0,
        kp.node_id(),
        generation,
        EnvelopeMeta::default(),
        ReservationAnnouncement {
            resource_id: island,
            state: ReservationState::Reserved {
                holder: kp.node_id(),
                until_unix_us: far_deadline(),
            },
        },
    )
    .expect("sign reservation")
}

/// Encoded wire bytes of [`reserve_ann`] — what an inbound frame carries.
pub fn reserve_bytes(kp: &EntityKeypair, island: u64, generation: u64) -> Vec<u8> {
    reserve_ann(kp, island, generation)
        .encode()
        .expect("encode")
}

/// Apply a signed `Reserved{holder = kp}` directly to a local fold
/// (bypasses transport) — for exact-read / watch tests.
pub fn apply_reserve(
    fold: &Arc<Fold<ReservationFold>>,
    kp: &EntityKeypair,
    island: u64,
    generation: u64,
) -> ApplyOutcome {
    fold.apply(reserve_ann(kp, island, generation))
        .expect("apply")
}

// ============================================================================
// Counting router (D6 / M2) — bench-only delivery accounting. Wraps a
// REPLACEMENT `FoldRegistry` built from the node's existing folds and is
// installed via `set_fold_router` (there is no getter for the live
// registry). It delegates verification + apply to the real registry
// FIRST, then counts every VERIFIED reservation dispatch outcome
// (`Inserted` / `Replaced` / `Rejected`), deduped by
// `(publisher, island, generation)` and filtered to the tracked island.
// A failed dispatch is never counted (Kyra: do not count before verify).
// ============================================================================

struct CountState {
    tracked_island: u64,
    seen: HashSet<(u64, u64, u64)>,
    count: usize,
}

/// An atomic snapshot of a counter's delivery endpoint — the unique
/// verified-delivery `count` and the distinct `publishers` behind it, read
/// together under ONE lock so an endpoint verifier sees a coherent
/// `(count, publisher-set)` pair (never a torn read across two separate lock
/// acquisitions, where a late unique tuple could land between them).
#[derive(Debug, Clone)]
pub struct DeliverySnapshot {
    pub count: usize,
    pub publishers: HashSet<u64>,
}

pub struct CountingRouter {
    inner: FoldRegistry,
    state: Mutex<CountState>,
    tx: watch::Sender<usize>,
}

impl CountingRouter {
    /// Wrap `inner`, tracking `tracked_island`.
    pub fn new(inner: FoldRegistry, tracked_island: u64) -> Self {
        let (tx, _rx) = watch::channel(0usize);
        Self {
            inner,
            state: Mutex::new(CountState {
                tracked_island,
                seen: HashSet::new(),
                count: 0,
            }),
            tx,
        }
    }

    /// Subscribe to the unique verified-delivery count — the poll-free
    /// wake the delivery barrier ([`wait_count`]) parks on.
    pub fn subscribe(&self) -> watch::Receiver<usize> {
        self.tx.subscribe()
    }

    /// Current unique verified-delivery count for the tracked island.
    pub fn count(&self) -> usize {
        self.state.lock().count
    }

    /// Snapshot of the distinct publishers whose verified reservation
    /// deliveries were counted for the tracked island — so a witness can
    /// prove the EXPECTED participant set was delivered, not merely the
    /// cardinality (a matching count with a wrong publisher would be a
    /// silent hole otherwise).
    pub fn seen_publishers(&self) -> HashSet<u64> {
        self.state
            .lock()
            .seen
            .iter()
            .map(|(publisher, _, _)| *publisher)
            .collect()
    }

    /// Atomic `(count, publishers)` snapshot for the tracked island — the
    /// endpoint verifier's single source of truth. Acquires the state lock
    /// ONCE so the count and its publisher set cannot tear across two reads
    /// (closing the window where a late unique tuple could land between a
    /// separate `count()` and `seen_publishers()`).
    pub fn delivery_snapshot(&self) -> DeliverySnapshot {
        let st = self.state.lock();
        DeliverySnapshot {
            count: st.count,
            publishers: st.seen.iter().map(|(publisher, _, _)| *publisher).collect(),
        }
    }

    /// Reset for a fresh sample: clear the seen set + count and retarget
    /// the tracked island (distributed-race samples use fresh islands).
    pub fn reset(&self, tracked_island: u64) {
        {
            let mut st = self.state.lock();
            st.tracked_island = tracked_island;
            st.seen.clear();
            st.count = 0;
        }
        let _ = self.tx.send(0);
    }

    fn record(&self, publisher_node: u64, island: u64, generation: u64) {
        let bumped = {
            let mut st = self.state.lock();
            if island != st.tracked_island {
                return; // wrong-island delivery: not counted
            }
            if st.seen.insert((publisher_node, island, generation)) {
                st.count += 1;
                Some(st.count)
            } else {
                None // duplicate tuple: not incremented
            }
        };
        if let Some(c) = bumped {
            let _ = self.tx.send(c);
        }
    }
}

impl FoldChannelRouter for CountingRouter {
    fn try_route(&self, publisher: &EntityId, bytes: &[u8]) -> Result<ApplyOutcome, DispatchError> {
        let is_reservation = peek_kind(bytes) == Some(ReservationFold::KIND_ID);
        // Delegate verification + apply to the real registry FIRST.
        let outcome = self.inner.try_route(publisher, bytes);
        // Count only a VERIFIED, dispatched reservation delivery. On the
        // Ok path the bytes are known-good, so the decode cannot fail.
        if is_reservation && outcome.is_ok() {
            if let Ok(ann) = SignedAnnouncement::<ReservationAnnouncement>::decode(bytes) {
                self.record(ann.node_id, ann.payload.resource_id, ann.generation);
            }
        }
        outcome
    }

    fn stats(&self) -> Vec<FoldStats> {
        self.inner.stats()
    }
}

/// Read the leading `kind: u16` varint (replicates the dispatch layer's
/// private `peek_kind`) to identify reservation frames cheaply, without
/// a full typed decode.
fn peek_kind(bytes: &[u8]) -> Option<u16> {
    postcard::take_from_bytes::<u16>(bytes).ok().map(|(k, _)| k)
}

/// Build a REPLACEMENT `FoldRegistry` from `node`'s three existing folds
/// (same `Arc<Fold<_>>` instances), wrap it in a [`CountingRouter`]
/// tracking `island`, and install it via `set_fold_router`. Returns the
/// shared handle for reading the delivery count.
pub fn install_counter(node: &Arc<MeshNode>, island: u64) -> Arc<CountingRouter> {
    let reg = FoldRegistry::new();
    reg.register(node.capability_fold().clone());
    reg.register(node.reservation_fold().clone());
    reg.register(node.island_fold().clone());
    let counter = Arc::new(CountingRouter::new(reg, island));
    node.set_fold_router(Some(counter.clone() as Arc<dyn FoldChannelRouter>));
    counter
}

/// EXACT delivery barrier: await the counting router reaching EXACTLY
/// `expected` unique verified deliveries within `deadline`. Poll-free
/// (parks on the count watch). Returns `true` only on an exact match;
/// **overshoot returns `false`** — an unexpected extra unique reservation
/// announcement must fail the sample, not satisfy it (Kyra ICB-0 Blocker
/// 1). A timeout (never reaching `expected`) also returns `false`.
pub async fn wait_count(
    counter: &Arc<CountingRouter>,
    expected: usize,
    deadline: Duration,
) -> bool {
    use std::cmp::Ordering;
    let mut rx = counter.subscribe();
    let fut = async {
        loop {
            match counter.count().cmp(&expected) {
                Ordering::Equal => return true,
                Ordering::Greater => return false, // overshoot fails the sample
                Ordering::Less => {
                    if rx.changed().await.is_err() {
                        return false;
                    }
                }
            }
        }
    };
    (tokio::time::timeout(deadline, fut).await).unwrap_or(false)
}

/// Reservation-delivery PREFLIGHT: does a reservation published by `src`
/// actually reach `dst`'s installed counting router? Retargets the
/// counter to a sentinel island, has `src` reserve it, and waits for the
/// delivery. Reservations do NOT relay (direct-peer broadcast only), so
/// an `A↔R↔B` chain returns `false` — the routed row must be refused.
pub async fn reservation_delivers(
    src: &Arc<MeshNode>,
    dst_counter: &Arc<CountingRouter>,
    sentinel_island: u64,
) -> bool {
    dst_counter.reset(sentinel_island);
    src.reserve_island(sentinel_island, far_deadline())
        .await
        .expect("reserve sentinel");
    wait_count(dst_counter, 1, Duration::from_secs(2)).await
}

/// Build a bare `CountingRouter` over a standalone reservation fold — for
/// the counting-rule unit witnesses (drive `try_route` directly with
/// crafted envelopes, no transport). Returns the router + the fold so a
/// caller can inspect applied state.
pub fn unit_router(tracked_island: u64) -> (Arc<CountingRouter>, Arc<Fold<ReservationFold>>) {
    let res_fold = Arc::new(Fold::<ReservationFold>::new());
    let reg = FoldRegistry::new();
    reg.register(res_fold.clone());
    let counter = Arc::new(CountingRouter::new(reg, tracked_island));
    (counter, res_fold)
}

// ============================================================================
// Matcher-fixture population readers (ICB-1 asserts population is stable
// across a timed matcher batch — item 11).
// ============================================================================

/// Number of capability hosts matching `filter` in this node's fold —
/// the "matched hosts" input population (bench-reconstructed, D3).
pub fn matched_host_count(node: &Arc<MeshNode>, filter: &CapabilityFilter) -> usize {
    node.find_nodes_by_filter(filter).len()
}

// ============================================================================
// Reporting skeletons — COMPLETED-latency and DIVERGENCE/censoring kept
// strictly separate (deliverable 9). ICB-3 divergence is architecture
// evidence, never fed to a latency histogram (M1).
// ============================================================================

/// hdrhistogram wrapper for COMPLETED boundaries only (matcher, local
/// commit, API return, remote visibility, fallback, takeover CAS).
/// Right-censored divergence samples never enter here (M1).
pub struct LatencyReport {
    hist: Histogram<u64>,
}

impl Default for LatencyReport {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyReport {
    pub fn new() -> Self {
        Self {
            hist: Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
                .expect("hdrhistogram alloc"),
        }
    }

    pub fn record(&mut self, ns: u64) {
        self.hist.record(ns.max(1)).expect("record");
    }

    pub fn samples(&self) -> u64 {
        self.hist.len()
    }

    pub fn quantile_us(&self, q: f64) -> f64 {
        self.hist.value_at_quantile(q) as f64 / 1_000.0
    }

    pub fn print_row(&self, label: &str) {
        let us = |v: u64| v as f64 / 1_000.0;
        println!(
            "── {label} · samples={} ──\n   p50={:.2}us p95={:.2}us p99={:.2}us max={:.2}us mean={:.2}us",
            self.samples(),
            us(self.hist.value_at_quantile(0.50)),
            us(self.hist.value_at_quantile(0.95)),
            us(self.hist.value_at_quantile(0.99)),
            us(self.hist.max()),
            self.hist.mean() / 1_000.0,
        );
    }
}

/// Divergence/censoring reporting skeleton for ICB-3 — SEMANTIC evidence,
/// not a latency SLO (M1/M6). Populated by ICB-3; defined here so the
/// separation from [`LatencyReport`] is structural.
#[derive(Debug, Default, Clone)]
pub struct DivergenceReport {
    pub label: String,
    pub claimants: usize,
    pub logical_sessions: usize,
    pub observation_window: Duration,
    pub optimistic_local_won: usize,
    pub claimant_self_belief: usize,
    pub foreign_rejected: usize,
    /// Agreement RATIOS reported three ways (M7-3): claimants only,
    /// non-claiming observers only, all nodes.
    pub claimant_holder_agreement: f64,
    pub observer_holder_agreement: f64,
    pub all_node_agreement: f64,
    pub samples_agreed: usize,
    pub samples_right_censored: usize,
    /// Samples that never became a valid divergence observation (missing
    /// delivery / count overshoot / claim-not-Won / publisher mismatch).
    /// NOT timeouts, and NEVER right-censored disagreement.
    pub invalid_samples: usize,
}

impl DivergenceReport {
    pub fn print(&self) {
        println!("── DIVERGENCE {} ──", self.label);
        println!(
            "   claimants={} logical_sessions={} window={:?}",
            self.claimants, self.logical_sessions, self.observation_window
        );
        // Holder shape is NOT reported here as singular extrema — the
        // separate HolderShapeAggregate range line is the sole holder-shape
        // output (Kyra ICB-3 closure: no unlabeled singular holder values).
        println!(
            "   optimistic_local_won={} claimant_self_belief={} foreign_rejected={} (holder-shape ranges printed separately)",
            self.optimistic_local_won, self.claimant_self_belief, self.foreign_rejected,
        );
        println!(
            "   agreement: claimant={:.2} observer={:.2} all-node={:.2}",
            self.claimant_holder_agreement, self.observer_holder_agreement, self.all_node_agreement,
        );
        println!(
            "   samples_agreed={} samples_right_censored={} invalid_samples={}",
            self.samples_agreed, self.samples_right_censored, self.invalid_samples,
        );
    }
}
