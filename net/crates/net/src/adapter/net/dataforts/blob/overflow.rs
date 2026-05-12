//! Active-overflow controller (v0.3 P2).
//!
//! Push-side complement of [`super::migration::BlobMigrationController`].
//! Migration is *pull* (the local node decides to take an
//! advertised hot blob); overflow is *push* (the local node
//! decides to shed a cold blob and a remote node decides
//! whether to accept). The two surfaces parallel each other —
//! every reject reason on either side maps to a Prometheus
//! counter label so operators can dashboard both directions.
//!
//! See [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`] for the full design.
//!
//! # P2 scope
//!
//! Pure-logic controller + tick driver + hysteresis state
//! machine. The actual wire push (`OverflowPush` RPC) lands in
//! P3; this module abstracts that away behind the
//! [`OverflowPushSink`] trait so the tick can be unit-tested
//! against a recorder without spinning up a real mesh.
//!
//! [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`]: ../../../../../docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use super::error::BlobError;
use super::mesh::OverflowConfig;
use super::refcount::BlobRefcountTable;
use crate::adapter::net::behavior::capability::CapabilityIndex;
use crate::adapter::net::behavior::{
    is_blob_storage_unhealthy, BlobCapability, CapabilitySet, GravityCapability, TopologyScope,
};
use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;

/// One overflow-push candidate the controller is considering
/// for this tick. The push controller already selected a
/// target peer for `hash`; the tick driver routes the actual
/// push call through [`OverflowPushSink::push`].
///
/// Equivalent shape to [`super::migration::BlobMigrationCandidate`]
/// with the direction reversed — `target_node_id` is the
/// receive-side, not the publisher.
#[derive(Clone, Debug)]
pub struct BlobOverflowCandidate {
    /// 32-byte chunk hash to push.
    pub hash: [u8; 32],
    /// Wire size of the chunk in bytes. Drives the receiver's
    /// `disk_free_gb` admission gate; the sender supplies it
    /// here so the receiver doesn't have to round-trip a
    /// `stat` call first.
    pub size_bytes: u64,
    /// node_id of the selected receive-side peer.
    pub target_node_id: u64,
    /// Snapshot of the target's capability set at selection
    /// time. The receive-side admission decision will re-read
    /// the index fresh; this snapshot is for the sender's
    /// dashboards / debug logs.
    pub target_caps: CapabilitySet,
    /// Decayed heat rate of `hash` at controller-tick time.
    /// Coldest candidates come first when the tick truncates
    /// to `max_pushes_per_tick`. `0.0` for hashes that haven't
    /// been read since their last full decay window — these
    /// are the prime overflow targets.
    pub cold_rate: f64,
}

/// Per-tick report. Each field maps to a Prometheus counter
/// label (`dataforts_blob_overflow_*`) so operators can
/// dashboard the loop without hand-coding per-reason metrics.
/// Pre-tick state lives in [`step_overflow_hysteresis`]; this
/// report captures *only* the actions this tick took.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlobOverflowTickReport {
    /// Candidates that passed the controller's filters AND
    /// the sink's push call returned `Ok`. Counts pushes
    /// the wire-side accepted; durability watermark observation
    /// is the next layer (P3).
    pub admitted: u64,
    /// Candidates the controller computed but no overflow-
    /// enabled peer was reachable for. Bumps once per cold
    /// hash that found no target — the same hash on the next
    /// tick may find one as caps propagate.
    pub rejected_no_target: u64,
    /// Sink returned an error. Includes the receive-side
    /// admission rejections that P3 maps to typed
    /// `OverflowReject` variants, plus RPC transport errors,
    /// plus the local-side chunk-open errors. Operators
    /// disambiguate via the underlying wire counters; this
    /// counter is the aggregated send-side view.
    pub push_errors: u64,
    /// The hysteresis state at the start of the tick. `true`
    /// = the controller was already firing on the prior tick.
    /// Useful for telemetry: the difference between "ticks
    /// where overflow was firing the whole time" and "ticks
    /// where the state just transitioned high" is operator-
    /// meaningful (the latter signals a workload spike).
    pub was_active_at_start: bool,
    /// The hysteresis state at the end of the tick. The pair
    /// `(was_active_at_start, is_active_at_end)` documents
    /// the state machine transition this tick took.
    pub is_active_at_end: bool,
    /// Local disk-usage ratio at the start of the tick.
    /// `disk_used_bytes / disk_total_bytes`. Surfaced for
    /// operator dashboards.
    pub disk_ratio_at_start: f64,
    /// Local disk-usage ratio at the end of the tick. Equal
    /// to `disk_ratio_at_start` in P2 (the actual freed-bytes
    /// accounting needs the durability watermark observation
    /// from P3 + a re-poll of disk stats). Reserved for P3.
    pub disk_ratio_at_end: f64,
    /// Total bytes pushed this tick. Sum of `size_bytes` of
    /// every admitted candidate. P2 reports the *pushed*
    /// volume; *reclaimed* volume waits for P3's durability
    /// observation.
    pub pushed_bytes: u64,
}

/// Pure-logic hysteresis state machine. Given the prior
/// `active` state + the current `disk_ratio` + the two
/// thresholds, return whether this tick should fire pushes
/// and update `active` in place. Mirrors the existing
/// [`super::metrics::evaluate_health_gate`] discipline
/// (which uses identical 95 % / 85 % shape but for the
/// health-gate tag).
///
/// Hysteresis rule:
///
/// - `disk_ratio >= high_water` → active = `true`.
/// - `disk_ratio <= low_water` → active = `false`.
/// - `low_water < disk_ratio < high_water` → active = prior
///   value (stay where we were; the hysteresis band).
///
/// `low_water >= high_water` degenerates to "fire whenever
/// disk_ratio >= high_water, clear whenever disk_ratio <= low_water"
/// — operator misconfiguration but not unsafe; the active
/// state just doesn't get the hysteresis benefit.
///
/// Returns the post-tick `active` state. The function reads
/// from + writes to `active` under `Relaxed` ordering — the
/// caller is the single tick driver, so no cross-thread
/// ordering is needed; the atomic is for visibility across
/// adapter clones (operator dashboard reads from one clone,
/// tick fires on another).
pub fn step_overflow_hysteresis(
    active: &AtomicBool,
    disk_ratio: f64,
    high_water: f64,
    low_water: f64,
) -> bool {
    let was_active = active.load(Ordering::Relaxed);
    let now_active = if disk_ratio >= high_water {
        true
    } else if disk_ratio <= low_water {
        false
    } else {
        was_active
    };
    if now_active != was_active {
        active.store(now_active, Ordering::Relaxed);
    }
    now_active
}

/// Sink trait for the actual push action. P3 wires the
/// [`MeshNode`]-backed implementation that sends an
/// `OverflowPush` RPC and waits for the durability
/// watermark; P2 ships the trait + a recorder mock for
/// unit tests.
///
/// `push` is fire-once-per-tick per `(hash, target_node_id)`
/// pair — the controller dedups by hash before calling the
/// sink. Idempotent on the receive side anyway (an
/// already-stored chunk is a no-op store).
///
/// [`MeshNode`]: crate::adapter::net::MeshNode
#[async_trait]
pub trait OverflowPushSink: Send + Sync {
    /// Push `hash` (`size_bytes`) to the receive-side peer
    /// identified by `target_node_id`. Returns `Ok(())` when
    /// the wire-side acknowledgement landed; `Err(BlobError)`
    /// when the send failed for any reason (RPC transport
    /// error, receive-side admission rejection, chunk-open
    /// failure). The tick driver aggregates errors into the
    /// `push_errors` counter without disambiguating — wire-
    /// level counters break out per reason.
    async fn push(
        &self,
        hash: [u8; 32],
        size_bytes: u64,
        target_node_id: u64,
    ) -> Result<(), BlobError>;
}

/// Active-overflow controller. Borrows the inputs it needs
/// (local caps, capability index, heat registry, refcount
/// table, config); the controller itself is stateless. The
/// hysteresis state lives in the caller as an `AtomicBool`
/// passed into [`drive_blob_overflow_tick`].
///
/// Lifetimes:
/// - `'a` — the controller's borrows. Typically the operator
///   constructs the controller per tick inside the scheduler
///   loop; the borrows are valid for the lifetime of the
///   tick await.
pub struct BlobOverflowController<'a> {
    /// Local node's capability set. Read for the local
    /// gravity scope (target-selection scope filter) and
    /// for the overflow-enabled self-check. The latter is
    /// belt-and-suspenders — the adapter's setter already
    /// gates `set_overflow_enabled(true)` against the local
    /// state — but cheaper than threading the bool in
    /// separately.
    pub local_caps: &'a CapabilitySet,
    /// Index of peer capability sets. The controller walks
    /// every overflow-enabled peer to score target
    /// selection.
    pub capability_index: &'a CapabilityIndex,
    /// Per-chunk heat registry. The controller walks every
    /// tracked hash, decays each rate to `now`, and ranks
    /// candidates coldest-first.
    pub heat_registry: &'a Arc<parking_lot::Mutex<BlobHeatRegistry>>,
    /// Per-hash refcount + pin table. Candidates are
    /// filtered against this: only `refcount == 0 &&
    /// !pinned` hashes are eligible for push in P2. The
    /// richer "all-references-are-cache" rule (which would
    /// allow shedding a chunk still held by a greedy cache
    /// entry) lands when per-source refcount inspection is
    /// added to [`BlobRefcountTable`].
    pub refcount: &'a BlobRefcountTable,
    /// Operator-tunable knobs. Read for `scope`,
    /// `max_pushes_per_tick`, and the high/low water
    /// thresholds (consumed by the hysteresis state machine).
    pub config: &'a OverflowConfig,
}

impl<'a> BlobOverflowController<'a> {
    /// Construct a controller from borrows. `new` is sugar
    /// for the struct literal — operators that prefer the
    /// builder shape can call this; tests usually use the
    /// literal for clarity.
    pub fn new(
        local_caps: &'a CapabilitySet,
        capability_index: &'a CapabilityIndex,
        heat_registry: &'a Arc<parking_lot::Mutex<BlobHeatRegistry>>,
        refcount: &'a BlobRefcountTable,
        config: &'a OverflowConfig,
    ) -> Self {
        Self {
            local_caps,
            capability_index,
            heat_registry,
            refcount,
            config,
        }
    }

    /// Compute every candidate for this tick — coldest first,
    /// truncated to `config.max_pushes_per_tick`. `size_for_hash`
    /// is an operator-supplied resolver (the controller doesn't
    /// know chunk sizes directly; `MeshBlobAdapter::stat_chunk`
    /// or an equivalent answers this).
    ///
    /// The function:
    ///
    /// 1. Snapshots `(hash, decayed_rate)` from the heat
    ///    registry under a brief read lock.
    /// 2. Filters out pinned hashes + hashes with nonzero
    ///    refcount + hashes whose `size_for_hash` returns
    ///    `None` (controller can't run the disk-gate without
    ///    a size; abstain rather than guess).
    /// 3. Sorts ascending by `(decayed_rate, hash)` — ties
    ///    broken by hash bytes for determinism.
    /// 4. For each candidate hash (in cold-first order),
    ///    walks the capability index for an overflow-
    ///    enabled peer with `disk_free_gb >= ceil(size /
    ///    1 GiB)` matching the local gravity scope; picks
    ///    the peer with the highest `disk_free_gb` (ties
    ///    broken by lowest `node_id`).
    /// 5. Drops candidates with no eligible target; the
    ///    tick reports those as `rejected_no_target` via
    ///    the difference between the heat-registry
    ///    candidate count and the returned vec length.
    /// 6. Truncates the result to
    ///    `config.max_pushes_per_tick`.
    pub fn candidates(
        &self,
        now: Instant,
        size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
    ) -> Vec<BlobOverflowCandidate> {
        // Step 1: snapshot heat-registry entries.
        let snap: Vec<([u8; 32], f64)> = {
            let guard = self.heat_registry.lock();
            guard
                .iter()
                .map(|(h, c)| {
                    // Compute decayed rate without mutating
                    // the counter — keeps the iteration
                    // read-only so a concurrent fetch path
                    // bumping a different hash isn't
                    // blocked.
                    let elapsed = now.saturating_duration_since(c.last_update());
                    let half_life_s = c.half_life().as_secs_f64();
                    let rate = if half_life_s == 0.0 || c.rate() == 0.0 {
                        c.rate()
                    } else {
                        let half_lives = elapsed.as_secs_f64() / half_life_s;
                        if half_lives > 64.0 {
                            0.0
                        } else {
                            c.rate() * 0.5_f64.powf(half_lives)
                        }
                    };
                    (*h, rate)
                })
                .collect()
        };

        // Step 2: filter on pin / refcount / size. A hash
        // with `refcount > 0` is not pushable in P2 — the
        // per-source refcount split (cache vs fold) is a
        // future refinement.
        let mut filtered: Vec<([u8; 32], f64, u64)> = snap
            .into_iter()
            .filter_map(|(h, rate)| {
                let entry = self.refcount.get(&h)?;
                if entry.pinned {
                    return None;
                }
                if entry.refcount > 0 {
                    return None;
                }
                let size = size_for_hash(h)?;
                Some((h, rate, size))
            })
            .collect();

        // Step 3: stable sort coldest-first. Ties broken by
        // hash bytes for determinism. NaN is impossible
        // here (decayed rate is always finite + non-negative
        // when input is finite, and the heat-counter ensures
        // finite input), so `partial_cmp` is safe.
        filtered.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        // Step 4-5: target selection per hash.
        let local_gravity = GravityCapability::from_capability_set(self.local_caps);
        let mut out: Vec<BlobOverflowCandidate> = Vec::new();
        for (hash, cold_rate, size_bytes) in filtered {
            if let Some((target_node_id, target_caps)) =
                self.pick_target(size_bytes, local_gravity.scope)
            {
                out.push(BlobOverflowCandidate {
                    hash,
                    size_bytes,
                    target_node_id,
                    target_caps,
                    cold_rate,
                });
            }
            // No target: just skip; the tick will bump
            // `rejected_no_target` based on how many
            // candidates dropped out.
            if out.len() >= self.config.max_pushes_per_tick {
                break;
            }
        }
        out
    }

    /// Find the best overflow-receiver peer for a chunk of
    /// `size_bytes`. Filter rule:
    ///
    /// - `cap.blob.storage = true`
    /// - `cap.blob.overflow_enabled = true`
    /// - `cap.blob.disk_free_gb >= ceil(size / 1 GiB)`
    /// - peer's `cap.gravity.scope` covers `local_scope`
    ///   (the local node won't push outside its own scope
    ///   bound)
    /// - peer is not `dataforts:blob-storage-unhealthy`
    ///
    /// Ranking: highest `disk_free_gb` wins (greedy spread
    /// across peers); ties broken by lowest `node_id` for
    /// determinism.
    fn pick_target(
        &self,
        size_bytes: u64,
        local_scope: TopologyScope,
    ) -> Option<(u64, CapabilitySet)> {
        let required_gb = size_bytes.div_ceil(1 << 30);
        let mut best: Option<(u64, u64, CapabilitySet)> = None; // (disk_free_gb, node_id, caps)
        for node_id in self.capability_index.all_nodes() {
            let Some(caps) = self.capability_index.get(node_id) else {
                continue;
            };
            let peer_blob = BlobCapability::from_capability_set(&caps);
            if !peer_blob.storage || !peer_blob.overflow_enabled {
                continue;
            }
            if peer_blob.disk_free_gb < required_gb {
                continue;
            }
            if is_blob_storage_unhealthy(&caps) {
                continue;
            }
            // Local-scope-covers-peer-scope check. We're
            // pushing OUT, so the local node's scope bound
            // is the gate — peers outside our scope can't
            // receive our overflow.
            let peer_gravity = GravityCapability::from_capability_set(&caps);
            if !scope_covers(local_scope, peer_gravity.scope) {
                continue;
            }
            // Update best by disk_free_gb desc, then
            // node_id asc.
            match &best {
                None => best = Some((peer_blob.disk_free_gb, node_id, caps)),
                Some((d, n, _)) => {
                    let is_better =
                        peer_blob.disk_free_gb > *d || (peer_blob.disk_free_gb == *d && node_id < *n);
                    if is_better {
                        best = Some((peer_blob.disk_free_gb, node_id, caps));
                    }
                }
            }
        }
        best.map(|(_, node_id, caps)| (node_id, caps))
    }
}

/// Drive one overflow tick.
///
/// Composes the hysteresis state machine + the controller's
/// candidate computation + the sink's push action into a
/// single async entry point. Operators call this from a
/// periodic task at `config.tick_interval_ms` cadence; the
/// function is idempotent against repeated calls (the
/// hysteresis state filters out spurious ticks).
///
/// Returns a [`BlobOverflowTickReport`] with per-reason
/// counters. Operators aggregate the report into Prometheus
/// metrics in P4.
///
/// Argument order matches the migration tick: state inputs
/// first (controller + sink), then the per-tick observables
/// (disk stats + hysteresis ref + now + size resolver).
pub async fn drive_blob_overflow_tick(
    controller: &BlobOverflowController<'_>,
    sink: &dyn OverflowPushSink,
    disk_used_bytes: u64,
    disk_total_bytes: u64,
    hysteresis_active: &AtomicBool,
    now: Instant,
    size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
) -> BlobOverflowTickReport {
    let mut report = BlobOverflowTickReport::default();
    let disk_ratio = if disk_total_bytes == 0 {
        // Adapter without a configured disk-cap reports
        // `disk_total = 0`; the safe default is "never
        // fire" rather than "always fire" (the latter
        // would push the moment any chunk lands on a
        // mis-configured node).
        0.0
    } else {
        disk_used_bytes as f64 / disk_total_bytes as f64
    };
    report.disk_ratio_at_start = disk_ratio;
    report.was_active_at_start = hysteresis_active.load(Ordering::Relaxed);

    let fire = step_overflow_hysteresis(
        hysteresis_active,
        disk_ratio,
        controller.config.high_water_ratio,
        controller.config.low_water_ratio,
    );
    report.is_active_at_end = fire;

    // Master switch gate. Even if disk crossed the high
    // water mark, a disabled overflow config means we
    // never push. Pin this so toggling `enabled = false`
    // is a hard stop.
    if !controller.config.enabled || !fire {
        report.disk_ratio_at_end = disk_ratio;
        return report;
    }

    // Compute candidates. `pre_pick_count` is the number
    // of cold hashes that passed the pin / refcount / size
    // filters; `candidates.len()` is the number that ALSO
    // found a target peer. The difference is
    // `rejected_no_target`.
    let candidates = controller.candidates(now, &size_for_hash);
    // Re-derive pre-pick count via a second pass (cheaper
    // than threading it through `candidates`): walk the
    // heat registry under read lock, count entries that
    // pass the pin / refcount / size filter.
    let pre_pick_count: usize = {
        let guard = controller.heat_registry.lock();
        guard
            .iter()
            .filter(|(h, _)| {
                controller
                    .refcount
                    .get(h)
                    .map(|e| !e.pinned && e.refcount == 0)
                    .unwrap_or(false)
                    && size_for_hash(**h).is_some()
            })
            .count()
    };
    // No-target candidates: pre_pick_count - candidates.len(),
    // capped at config.max_pushes_per_tick so over-pre-pick
    // doesn't inflate the counter.
    let no_target = pre_pick_count
        .saturating_sub(candidates.len())
        .min(controller.config.max_pushes_per_tick);
    report.rejected_no_target = no_target as u64;

    // Fire pushes. `max_pushes_per_tick = 0` is a valid
    // "trigger only, no real pushes" mode — the candidates
    // list will be empty so we drop straight through.
    for candidate in candidates {
        match sink
            .push(candidate.hash, candidate.size_bytes, candidate.target_node_id)
            .await
        {
            Ok(()) => {
                report.admitted += 1;
                report.pushed_bytes = report.pushed_bytes.saturating_add(candidate.size_bytes);
            }
            Err(e) => {
                tracing::trace!(
                    error = ?e,
                    hash = ?candidate.hash,
                    target = candidate.target_node_id,
                    "blob overflow: push failed; counted"
                );
                report.push_errors += 1;
            }
        }
    }

    // disk_ratio_at_end stays equal to start in P2; P3
    // wires the durability watermark + a fresh disk-stat
    // poll to surface the post-tick reclaim.
    report.disk_ratio_at_end = disk_ratio;
    report
}

/// `local` scope covers `peer` iff a push from a node with
/// scope `local` can land on a node with scope `peer`. The
/// rule mirrors the migration controller's
/// `scope_at_least_as_narrow`: a Zone-scoped local node can
/// push to Zone / Region / Mesh peers (the peer's scope
/// covers the local one), but a Mesh-scoped local node
/// can't push to a Node-scoped peer (the peer's scope is
/// narrower and won't accept the cross-scope artifact).
///
/// `local == Mesh` covers any peer scope (mesh is the widest
/// scope — any peer is reachable). `local == Node` is the
/// degenerate case: only same-node receivers qualify; in
/// practice this is the "never push" config.
fn scope_covers(local: TopologyScope, peer: TopologyScope) -> bool {
    use TopologyScope::*;
    matches!(
        (local, peer),
        (Mesh, _) | (Region, Region | Mesh) | (Zone, Zone | Region | Mesh) | (Node, Node)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::CapabilityAnnouncement;
    use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
    use crate::adapter::net::identity::EntityId;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    fn hex64(byte: u8) -> ([u8; 32], String) {
        let mut h = [0u8; 32];
        h.fill(byte);
        let hex: String = h.iter().map(|b| format!("{:02x}", b)).collect();
        (h, hex)
    }

    /// Build a `CapabilitySet` for an overflow-enabled peer
    /// with `disk_free_gb` headroom and mesh-wide gravity.
    fn overflow_peer_caps(disk_free_gb: u64) -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag(format!("dataforts.blob.disk_free_gb={}", disk_free_gb))
            .add_tag("dataforts.blob.overflow")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128")
    }

    /// Local node config: overflow-enabled, mesh scope.
    fn overflow_enabled_local_caps() -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.overflow")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128")
    }

    /// Recorder sink — records every push call's
    /// `(hash, size, target)` tuple. The `fail` toggle lets
    /// tests inject sink errors to exercise the
    /// `push_errors` counter.
    struct OverflowPushRecorder {
        calls: Arc<parking_lot::Mutex<Vec<([u8; 32], u64, u64)>>>,
        fail_count: Arc<AtomicU64>,
    }

    impl OverflowPushRecorder {
        fn new() -> Self {
            Self {
                calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
                fail_count: Arc::new(AtomicU64::new(0)),
            }
        }

        fn calls(&self) -> Vec<([u8; 32], u64, u64)> {
            self.calls.lock().clone()
        }
    }

    #[async_trait]
    impl OverflowPushSink for OverflowPushRecorder {
        async fn push(
            &self,
            hash: [u8; 32],
            size_bytes: u64,
            target_node_id: u64,
        ) -> Result<(), BlobError> {
            if self.fail_count.load(Ordering::Relaxed) > 0 {
                self.fail_count.fetch_sub(1, Ordering::Relaxed);
                return Err(BlobError::NotFound("simulated push failure".to_string()));
            }
            self.calls.lock().push((hash, size_bytes, target_node_id));
            Ok(())
        }
    }

    /// Build a `BlobHeatRegistry` with a list of `(hash, rate)`
    /// pairs at the supplied `now` instant. Each entry is
    /// freshly seeded and bumped to its target rate via direct
    /// counter access.
    fn heat_registry_with(
        now: Instant,
        entries: &[([u8; 32], f64)],
    ) -> Arc<parking_lot::Mutex<BlobHeatRegistry>> {
        let mut reg = BlobHeatRegistry::new();
        for (hash, rate) in entries {
            let counter = reg.entry_mut(*hash, Duration::from_secs(60), now);
            // Bump `*rate` times to reach the target rate;
            // each bump adds 1.0 after decay. `now` is the
            // same for every bump so no decay happens.
            for _ in 0..(*rate as usize) {
                counter.bump(now);
            }
        }
        Arc::new(parking_lot::Mutex::new(reg))
    }

    /// Refcount table where every supplied hash is
    /// `refcount = 0, !pinned` — eligible for overflow.
    /// `store_observed` is the cheapest way to land an
    /// entry at refcount 0 + a recorded `first_seen` time.
    fn refcount_with_zero(hashes: &[[u8; 32]], now_ms: u64) -> BlobRefcountTable {
        let rc = BlobRefcountTable::new();
        for h in hashes {
            rc.store_observed(*h, now_ms);
        }
        rc
    }

    fn cap_index_with(peers: &[(u64, [u8; 32], CapabilitySet)]) -> CapabilityIndex {
        let index = CapabilityIndex::new();
        for (idx, (node_id, entity_bytes, caps)) in peers.iter().enumerate() {
            let entity = EntityId::from_bytes(*entity_bytes);
            // CapabilityAnnouncement::new(node_id, entity, version, caps)
            let announce =
                CapabilityAnnouncement::new(*node_id, entity, 1 + idx as u64, caps.clone());
            index.index(announce);
        }
        index
    }

    // ========================================================================
    // step_overflow_hysteresis (pure-logic state machine)
    // ========================================================================

    #[test]
    fn hysteresis_fires_above_high_water() {
        let active = AtomicBool::new(false);
        assert!(step_overflow_hysteresis(&active, 0.90, 0.85, 0.70));
        assert!(active.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_clears_below_low_water() {
        let active = AtomicBool::new(true);
        assert!(!step_overflow_hysteresis(&active, 0.65, 0.85, 0.70));
        assert!(!active.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_holds_state_in_band() {
        // Between low (0.70) and high (0.85): hold prior
        // state regardless of which boundary was last
        // crossed.
        let active = AtomicBool::new(true);
        assert!(step_overflow_hysteresis(&active, 0.80, 0.85, 0.70));
        assert!(active.load(Ordering::Relaxed));

        let inactive = AtomicBool::new(false);
        assert!(!step_overflow_hysteresis(&inactive, 0.80, 0.85, 0.70));
        assert!(!inactive.load(Ordering::Relaxed));
    }

    #[test]
    fn hysteresis_boundary_inclusive() {
        // disk_ratio == high_water fires (>=);
        // disk_ratio == low_water clears (<=).
        let active = AtomicBool::new(false);
        assert!(step_overflow_hysteresis(&active, 0.85, 0.85, 0.70));
        let active2 = AtomicBool::new(true);
        assert!(!step_overflow_hysteresis(&active2, 0.70, 0.85, 0.70));
    }

    // ========================================================================
    // BlobOverflowController::candidates
    // ========================================================================

    #[test]
    fn controller_candidates_returns_coldest_first() {
        // Three hashes with rates 0.0 / 1.0 / 5.0 →
        // ordering A (cold) / B (warm) / C (hot).
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let (b, _) = hex64(0xBB);
        let (c, _) = hex64(0xCC);
        let heat = heat_registry_with(now, &[(a, 0.0), (b, 1.0), (c, 5.0)]);
        let refcount = refcount_with_zero(&[a, b, c], 1_000_000);
        let peer = (
            99u64,
            [0x11; 32],
            overflow_peer_caps(50),
        );
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(cands.len(), 3);
        // a (rate 0.0) first; c (rate 5.0) last.
        assert_eq!(cands[0].hash, a);
        assert_eq!(cands[2].hash, c);
    }

    #[test]
    fn controller_skips_pinned_hashes() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let (b, _) = hex64(0xBB);
        let heat = heat_registry_with(now, &[(a, 0.0), (b, 0.0)]);
        let refcount = BlobRefcountTable::new();
        refcount.store_observed(a, 1_000_000);
        refcount.pin(a, 1_000_000);
        refcount.store_observed(b, 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        // Pinned `a` skipped; only unpinned `b` surfaces.
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].hash, b);
    }

    #[test]
    fn controller_skips_hashes_with_nonzero_refcount() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = BlobRefcountTable::new();
        refcount.incr(a, 1_000_000); // refcount = 1, not droppable
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        assert!(controller.candidates(now, |_| Some(1024)).is_empty());
    }

    #[test]
    fn controller_picks_highest_disk_free_target() {
        // Two peers, both overflow-enabled. Peer 99 has 40
        // GiB free; peer 88 has 80 GiB free. Greedy spread
        // → peer 88 wins.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer_low = (99u64, [0x11; 32], overflow_peer_caps(40));
        let peer_high = (88u64, [0x22; 32], overflow_peer_caps(80));
        let index = cap_index_with(&[peer_low, peer_high]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);

        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].target_node_id, 88);
    }

    #[test]
    fn controller_skips_peers_without_overflow_tag() {
        // Peer has storage + disk + scope BUT no overflow
        // tag → not a valid target.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let no_overflow_peer_caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=80")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh")
            .add_tag("dataforts.gravity.proximity=128");
        let peer = (99u64, [0x11; 32], no_overflow_peer_caps);
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        assert!(controller.candidates(now, |_| Some(1024)).is_empty());
    }

    #[test]
    fn controller_skips_peers_with_insufficient_disk() {
        // Peer has 1 GiB free; we're pushing a 4 GiB blob →
        // no target.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(1));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 16,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let four_gib: u64 = 4 * (1 << 30);
        assert!(controller.candidates(now, |_| Some(four_gib)).is_empty());
    }

    #[test]
    fn controller_truncates_to_max_pushes_per_tick() {
        let now = Instant::now();
        let hashes: Vec<[u8; 32]> = (0..5).map(|i| hex64(i as u8).0).collect();
        let entries: Vec<([u8; 32], f64)> = hashes.iter().map(|h| (*h, 0.0)).collect();
        let heat = heat_registry_with(now, &entries);
        let refcount = refcount_with_zero(&hashes, 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            max_pushes_per_tick: 2,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let cands = controller.candidates(now, |_| Some(1024));
        assert_eq!(cands.len(), 2, "max_pushes_per_tick caps the candidate list");
    }

    // ========================================================================
    // drive_blob_overflow_tick — end-to-end against the recorder sink
    // ========================================================================

    #[tokio::test]
    async fn tick_no_op_when_below_low_water() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        // disk_ratio = 0.50 — below low_water (0.70).
        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            500,
            1000,
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert!(!report.is_active_at_end);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_fires_above_high_water_and_pushes_to_recorder() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        // disk_ratio = 0.90 — above high_water (0.85).
        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            900,
            1000,
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 1);
        assert!(report.is_active_at_end);
        assert_eq!(report.pushed_bytes, 1024);
        let calls = sink.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, a);
        assert_eq!(calls[0].2, 99);
    }

    #[tokio::test]
    async fn tick_master_switch_off_skips_pushes_even_above_high_water() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: false, // master switch off
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            900,
            1000,
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        // Hysteresis still transitions (the disk state machine
        // is independent of the master switch — operators
        // dashboarding the active gauge should see it climb
        // before they enable). Pushes don't fire.
        assert!(report.is_active_at_end);
        assert_eq!(report.admitted, 0);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_records_push_errors_when_sink_fails() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();
        sink.fail_count.store(1, Ordering::Relaxed);

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            900,
            1000,
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.push_errors, 1);
        assert_eq!(report.pushed_bytes, 0);
    }

    #[tokio::test]
    async fn tick_records_no_target_when_no_overflow_enabled_peer() {
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        // Peer has no overflow tag.
        let no_overflow_peer_caps = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=80")
            .add_tag("dataforts.gravity.enabled")
            .add_tag("dataforts.gravity.scope=mesh");
        let peer = (99u64, [0x11; 32], no_overflow_peer_caps);
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            900,
            1000,
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.admitted, 0);
        assert_eq!(report.rejected_no_target, 1);
        assert_eq!(sink.calls().len(), 0);
    }

    #[tokio::test]
    async fn tick_zero_disk_total_never_fires() {
        // disk_total = 0 → ratio = 0.0 → always below high
        // water. Defends against misconfigured nodes that
        // would push the moment any chunk lands.
        let now = Instant::now();
        let (a, _) = hex64(0xAA);
        let heat = heat_registry_with(now, &[(a, 0.0)]);
        let refcount = refcount_with_zero(&[a], 1_000_000);
        let peer = (99u64, [0x11; 32], overflow_peer_caps(50));
        let index = cap_index_with(&[peer]);
        let local = overflow_enabled_local_caps();
        let cfg = OverflowConfig {
            enabled: true,
            ..Default::default()
        };
        let controller = BlobOverflowController::new(&local, &index, &heat, &refcount, &cfg);
        let active = AtomicBool::new(false);
        let sink = OverflowPushRecorder::new();

        let report = drive_blob_overflow_tick(
            &controller,
            &sink,
            500,
            0, // disk_total = 0
            &active,
            now,
            |_| Some(1024),
        )
        .await;
        assert_eq!(report.disk_ratio_at_start, 0.0);
        assert!(!report.is_active_at_end);
        assert_eq!(sink.calls().len(), 0);
    }

    // ========================================================================
    // scope_covers
    // ========================================================================

    #[test]
    fn scope_covers_mesh_covers_everything() {
        use TopologyScope::*;
        for peer in [Node, Zone, Region, Mesh] {
            assert!(scope_covers(Mesh, peer));
        }
    }

    #[test]
    fn scope_covers_zone_does_not_cover_node() {
        // Zone-scoped sender can't push to a Node-scoped
        // peer (peer is narrower; won't accept cross-scope).
        assert!(!scope_covers(TopologyScope::Zone, TopologyScope::Node));
        // But Zone-scoped sender CAN push to Zone / Region /
        // Mesh peers (peer's scope covers the sender's).
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Zone));
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Region));
        assert!(scope_covers(TopologyScope::Zone, TopologyScope::Mesh));
    }
}
