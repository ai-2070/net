//! `MeshBlobAdapter` — mesh-native blob storage adapter that uses
//! [`Redex`] as the underlying content-addressed store.
//!
//! Each blob chunk (or whole Small blob) is stored as a single-event
//! `RedexFile` at channel name `dataforts/blob/<hex32>` where `hex32`
//! is the chunk's BLAKE3 hash. Content-addressing makes the storage
//! layer trivially deduplicated — two writes of identical bytes
//! resolve to the same channel and are idempotent.
//!
//! The adapter is registered under the `mesh://` URI scheme. The URI
//! itself is opaque to the adapter (the content hash is the
//! authoritative address); operators conventionally pass
//! `mesh://<hex32>` for human-readable wire traces, but any
//! `mesh://*` URI works.
//!
//! # Manifest dispatch
//!
//! - [`BlobRef::Small`] — bytes live in a single chunk file. `store`
//!   writes the file, `fetch` reads it back.
//! - [`BlobRef::Manifest`] — `store` decomposes the input via
//!   [`chunk_payload`], writes each chunk as its own content-addressed
//!   `RedexFile`, and verifies the supplied chunk list against the
//!   recomputed chunks. `fetch` walks the manifest's `chunks` field
//!   and concatenates each chunk's bytes. `fetch_range` uses
//!   [`byte_range_to_chunks`] to only read the chunks the requested
//!   byte range covers.
//!
//! # What this adapter is NOT (yet, v0.2 PR-2a)
//!
//! - **Replication wiring is opt-in but un-tested in this PR.** The
//!   adapter constructor takes an optional [`ReplicationConfig`];
//!   when supplied, every per-chunk `RedexFile` opens with that
//!   config. Cross-node replication of blob chunks is therefore
//!   already plumbed through to RedEX's existing replication runtime
//!   — but the e2e mesh integration (a peer fetching a blob via
//!   `causal:<hex>` advertisement) lands in a follow-up.
//! - **No GC / refcount / pinning.** PR-4's scope per the plan.
//! - **No `blob-storage-unhealthy` health-gate tag emission.**
//!   Adapter doesn't advertise capabilities itself — that surface
//!   lands with the capability extension in PR-2b.
//! - **`stat::replicas_observed`** comes back as `0` until the
//!   mesh-side advertisement integration lands; `replica_target`
//!   reflects the operator's `ReplicationConfig::factor` when set.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;

use super::adapter::{BlobAdapter, BlobStat};
use super::admission::auth_allows_blob_op;
use super::blob_ref::{
    byte_range_to_chunks, chunk_payload, BlobRef, ChunkRef, ChunkedPayload, Encoding,
};
use super::error::BlobError;
use super::metrics::BlobMetrics;
use super::refcount::{BlobRefcountTable, DEFAULT_RETENTION_FLOOR};
use crate::adapter::net::behavior::TopologyScope;
use crate::adapter::net::channel::{AuthGuard, ChannelName};
use crate::adapter::net::redex::{Redex, RedexFileConfig, ReplicationConfig};

/// Per-chunk storage channel prefix. Each blob chunk lives at
/// `dataforts/blob/<hex32>` keyed on its BLAKE3 hash.
const CHUNK_CHANNEL_PREFIX: &str = "dataforts/blob/";

/// Default half-life applied to per-chunk blob-heat counters when
/// the operator opts into the heat-tracking path via
/// [`MeshBlobAdapter::with_blob_heat`]. 60 s mirrors the chain
/// heat half-life — a fetch every minute keeps the counter near
/// steady state; cold blobs decay below the emit threshold inside
/// a few minutes.
pub const DEFAULT_BLOB_HEAT_HALF_LIFE: Duration = Duration::from_secs(60);

/// Default high-water disk-usage ratio that triggers the
/// overflow tick. `0.85` lines up with the existing health-
/// gate clear threshold so overflow fires *before* the
/// `dataforts:blob-storage-unhealthy` advertisement — by the
/// time the node is unhealthy, overflow has already been
/// shedding for a while.
pub const DEFAULT_OVERFLOW_HIGH_WATER_RATIO: f64 = 0.85;

/// Default low-water disk-usage ratio that re-enters the
/// "not actively overflowing" state. `0.70` gives 15 points
/// of hysteresis to avoid flapping the active gauge near the
/// boundary; mirrors the migration-controller / health-gate
/// hysteresis discipline.
pub const DEFAULT_OVERFLOW_LOW_WATER_RATIO: f64 = 0.70;

/// Default per-tick push budget. Each push opens a chunk
/// channel with replication armed, so the cap bounds the
/// wire-side bandwidth burst when a node first crosses the
/// high-water mark.
pub const DEFAULT_OVERFLOW_MAX_PUSHES_PER_TICK: usize = 16;

/// Default tick cadence. Independent of the gravity tick —
/// overflow is push-driven by local disk state, not by
/// inbound heat. 30 s is short enough that a node above the
/// high-water mark reclaims meaningfully per minute without
/// thrashing the disk-stat probe.
pub const DEFAULT_OVERFLOW_TICK_INTERVAL_MS: u64 = 30_000;

/// Operator-tunable knobs for the active-overflow controller
/// (`BlobOverflowController`, lands in P2). P1 carries the
/// type + the `MeshBlobAdapter` builder / getter / setter
/// surface; the controller + tick driver land in P2.
///
/// `enabled` is the master switch. The remaining fields are
/// thresholds + budgets the controller reads when overflow
/// is active. Tuning the thresholds without flipping
/// `enabled` is a valid operator gesture — the next
/// `set_overflow_enabled(true)` call picks up the latest
/// thresholds without rebuilding the adapter.
///
/// See [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`] for the full
/// design.
///
/// [`DATAFORTS_BLOB_OVERFLOW_PLAN.md`]: ../../../../../docs/plans/DATAFORTS_BLOB_OVERFLOW_PLAN.md
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverflowConfig {
    /// Operator-visible master switch. `false` by default;
    /// the adapter never pushes, never advertises the
    /// `dataforts.blob.overflow` tag, and never accepts
    /// inbound pushes when this is `false`.
    pub enabled: bool,
    /// Local disk usage at or above this ratio triggers the
    /// overflow tick (controller reads + fires pushes).
    /// Bounded to `0.0..=1.0`; the setter clamps out-of-range
    /// values rather than rejecting them, on the theory that
    /// a misconfigured operator should still get a sane node.
    /// Default [`DEFAULT_OVERFLOW_HIGH_WATER_RATIO`] (0.85).
    pub high_water_ratio: f64,
    /// Local disk usage at or below this ratio clears the
    /// "actively overflowing" state. Must be strictly less
    /// than `high_water_ratio` for the hysteresis to mean
    /// anything; the setter doesn't enforce ordering (the
    /// controller's tick logic treats `low >= high` as
    /// "no hysteresis, fire every tick above low").
    /// Default [`DEFAULT_OVERFLOW_LOW_WATER_RATIO`] (0.70).
    pub low_water_ratio: f64,
    /// Maximum number of hashes pushed per tick. `0` is a
    /// degenerate "tick fires but pushes nothing" mode — the
    /// controller bumps the trigger counter without admitting
    /// any pushes. Useful for operator dashboards to observe
    /// "would have fired N times" before enabling real pushes.
    /// Default [`DEFAULT_OVERFLOW_MAX_PUSHES_PER_TICK`] (16).
    pub max_pushes_per_tick: usize,
    /// Topology scope bound on push-target selection. `Mesh`
    /// by default — the controller may pick any overflow-
    /// enabled peer in the mesh. `Zone` keeps overflow inside
    /// the zone (multi-cloud deployments configure this to
    /// keep overflow traffic off the WAN).
    pub scope: TopologyScope,
    /// Tick cadence in milliseconds. Operators drive the tick
    /// from their scheduling loop; the value here documents
    /// the recommended cadence and is surfaced in
    /// `prometheus_text` so dashboards can label it.
    /// Default [`DEFAULT_OVERFLOW_TICK_INTERVAL_MS`] (30 000).
    pub tick_interval_ms: u64,
}

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            high_water_ratio: DEFAULT_OVERFLOW_HIGH_WATER_RATIO,
            low_water_ratio: DEFAULT_OVERFLOW_LOW_WATER_RATIO,
            max_pushes_per_tick: DEFAULT_OVERFLOW_MAX_PUSHES_PER_TICK,
            scope: TopologyScope::Mesh,
            tick_interval_ms: DEFAULT_OVERFLOW_TICK_INTERVAL_MS,
        }
    }
}

/// `mesh://`-scheme adapter that stores chunks as content-addressed
/// [`RedexFile`](crate::adapter::net::redex::RedexFile)s. See the
/// module-level docs for the dispatch shape.
#[derive(Clone)]
pub struct MeshBlobAdapter {
    id: String,
    redex: Arc<Redex>,
    /// Whether per-chunk files persist to disk. Defaults to `false`
    /// (in-memory chunks; chunks vanish on process restart). Set
    /// via [`Self::with_persistent`] in production deployments.
    /// Requires `Redex::with_persistent_dir(...)` to have been
    /// configured on the underlying handle — without it, the
    /// per-chunk open surfaces a typed `RedexError`.
    persistent: bool,
    /// Optional per-chunk replication config. `None` keeps chunks
    /// single-node; `Some(_)` arms each per-chunk file with the
    /// existing RedEX replication runtime. Wiring `Redex::enable_replication(mesh)`
    /// is the operator's responsibility — without it, chunks open
    /// with replication set but the runtime fails to spawn (typed
    /// `RedexError`).
    replication: Option<ReplicationConfig>,
    /// Per-hash refcount + pin table. Drives [`Self::sweep_gc`] +
    /// fills in [`BlobStat::last_seen_unix_ms`] on stat queries.
    /// Cheap to clone (the `Arc`-backed `DashMap` shared inside);
    /// the adapter holds a clone
    /// and the operator's GC driver holds another for read-only
    /// observation.
    refcount: BlobRefcountTable,
    /// Operator-configured retention floor. Default
    /// [`DEFAULT_RETENTION_FLOOR`] (24 h); set via
    /// [`Self::with_retention_floor`] for shorter / longer
    /// windows.
    retention_floor: Duration,
    /// Atomic-counter registry surfaced via [`Self::metrics`].
    /// Cheap to clone; shared with the operator's Prometheus
    /// scrape.
    metrics: BlobMetrics,
    /// Optional auth guard used by [`Self::pin_authorized`] /
    /// [`Self::unpin_authorized`] / [`Self::delete_chunk_authorized`]
    /// to gate peer-initiated pin / unpin / delete ops against the
    /// publishing chain's `(origin_hash, ChannelName)` ACL. `None`
    /// (the default) leaves the `*_authorized` variants as a
    /// misconfiguration — the unauth `pin` / `unpin` / `delete_chunk`
    /// variants are still reachable for system-internal callers
    /// (GC sweep, chain-fold refcount incr/decr).
    auth_guard: Option<Arc<AuthGuard>>,
    /// Optional shared blob-heat registry. When wired (PR-5j-b),
    /// every successful [`Self::fetch`] / [`Self::fetch_range`]
    /// bumps the chunk's heat counter so the gravity layer can
    /// observe per-blob read pressure. `None` (the default) keeps
    /// fetches free of any heat side-effect. Cheap to clone
    /// (`Arc<Mutex<...>>` inside); operators typically share
    /// the same handle with the gravity controller's tick loop.
    blob_heat:
        Option<Arc<parking_lot::Mutex<crate::adapter::net::dataforts::gravity::BlobHeatRegistry>>>,
    /// Half-life applied to newly-entered blob-heat counters.
    /// Defaults to [`DEFAULT_BLOB_HEAT_HALF_LIFE`] (60 s); operators
    /// tune via [`Self::with_blob_heat`].
    blob_heat_half_life: Duration,
    /// Per-hash advisory lock. Serializes concurrent
    /// [`Self::store_chunk`] invocations on the same content
    /// hash so two callers can't both observe the chunk file
    /// empty and both append duplicate payloads. Entries are
    /// created lazily on first store of a hash and best-effort
    /// reclaimed once no caller is holding the lock; the map's
    /// long-term size is bounded by the rate of distinct
    /// concurrent stores, not by total distinct hashes ever
    /// seen.
    in_flight_stores: Arc<DashMap<[u8; 32], Arc<tokio::sync::Mutex<()>>>>,
    /// Active-overflow knobs (v0.3 P1 surface). Held behind
    /// an `Arc<RwLock<_>>` so the boolean toggle + threshold
    /// updates are cheap, lock-free for the steady-state
    /// read, and visible across every adapter clone. Default
    /// `OverflowConfig::default()` — `enabled = false`, so
    /// existing call sites observe v0.2 behavior unchanged.
    /// The push controller + receive-side handler land in
    /// P2 / P3; this field is the storage shape the rest of
    /// the work will compose against.
    overflow: Arc<parking_lot::RwLock<OverflowConfig>>,
    /// Hysteresis state for [`super::overflow::drive_blob_overflow_tick`].
    /// `true` iff the most recent tick observed disk usage at
    /// or above the high-water threshold; `false` iff the most
    /// recent tick observed disk usage at or below the
    /// low-water threshold. In the hysteresis band between the
    /// two, the prior value is preserved.
    ///
    /// Shared across adapter clones so an operator dashboard
    /// reading from one clone sees the live state set by the
    /// scheduler tick on another clone. `Relaxed` ordering is
    /// fine — the tick driver is the single writer; reads are
    /// observer-only.
    overflow_active: Arc<std::sync::atomic::AtomicBool>,
}

impl MeshBlobAdapter {
    /// Construct a mesh-native adapter rooted at `redex`. Chunks are
    /// stored as in-memory `RedexFile`s by default — call
    /// [`Self::with_persistent`] to write to disk (requires the
    /// underlying `Redex` to be configured with a persistent dir),
    /// and / or [`Self::with_replication`] to opt every per-chunk
    /// file into the cross-node replication runtime.
    pub fn new(id: impl Into<String>, redex: Arc<Redex>) -> Self {
        Self {
            id: id.into(),
            redex,
            persistent: false,
            replication: None,
            refcount: BlobRefcountTable::new(),
            retention_floor: DEFAULT_RETENTION_FLOOR,
            metrics: BlobMetrics::new(),
            auth_guard: None,
            blob_heat: None,
            blob_heat_half_life: DEFAULT_BLOB_HEAT_HALF_LIFE,
            in_flight_stores: Arc::new(DashMap::new()),
            overflow: Arc::new(parking_lot::RwLock::new(OverflowConfig::default())),
            overflow_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Opt every per-chunk file into disk persistence. Default is
    /// in-memory; switch on for production deployments that want
    /// blob chunks to survive process restart.
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Per-chunk replication config applied to every newly-opened
    /// chunk file. Requires `Redex::enable_replication(mesh)` to
    /// have been called on the underlying handle; the per-chunk
    /// open surfaces a typed `RedexError` if not.
    pub fn with_replication(mut self, cfg: ReplicationConfig) -> Self {
        self.replication = Some(cfg);
        self
    }

    /// Override the default retention floor (24 h) applied by the
    /// GC sweep. Shorter floors reclaim disk faster at the cost
    /// of premature GC under racy refcount sources; longer floors
    /// are safer but consume more disk between sweeps. Tune to
    /// match the operator's chain-fold cadence.
    pub fn with_retention_floor(mut self, floor: Duration) -> Self {
        self.retention_floor = floor;
        self
    }

    /// Operator-configured disk capacity in bytes. Drives the
    /// `dataforts_blob_disk_capacity_bytes` gauge + the health-
    /// gate threshold. `0` (the default) disables the health
    /// gate entirely.
    pub fn with_disk_capacity(self, bytes: u64) -> Self {
        self.metrics.set_disk_capacity_bytes(bytes);
        self
    }

    /// Wire an [`AuthGuard`] handle so the `*_authorized` variants
    /// of [`Self::pin`] / [`Self::unpin`] / [`Self::delete_chunk`]
    /// can gate peer-initiated ops against the publishing chain's
    /// `(origin_hash, ChannelName)` ACL. The unauth variants stay
    /// reachable for system-internal callers (GC sweep,
    /// chain-fold-driven refcount maintenance).
    pub fn with_auth_guard(mut self, guard: Arc<AuthGuard>) -> Self {
        self.auth_guard = Some(guard);
        self
    }

    /// Wire a shared blob-heat registry. Each successful fetch
    /// then bumps the chunk hash's heat counter so a gravity
    /// tick can observe the read rate (PR-5j-b). The registry
    /// handle is cheap to clone (`Arc<Mutex>` inside); operators
    /// typically share the same handle with the gravity migration
    /// controller's tick loop.
    ///
    /// `half_life` controls the per-counter decay; pass
    /// [`DEFAULT_BLOB_HEAT_HALF_LIFE`] for the standard 60 s
    /// half-life or a custom value when tuning aggressive vs
    /// lazy migration cadence.
    pub fn with_blob_heat(
        mut self,
        registry: Arc<
            parking_lot::Mutex<crate::adapter::net::dataforts::gravity::BlobHeatRegistry>,
        >,
        half_life: Duration,
    ) -> Self {
        self.blob_heat = Some(registry);
        self.blob_heat_half_life = half_life;
        self
    }

    /// Install the supplied [`OverflowConfig`] as the initial
    /// overflow state. The `enabled` field of `config` is
    /// honored — passing `OverflowConfig { enabled: true, ..
    /// Default::default() }` is the typical "turn on with
    /// defaults" gesture. Subsequent
    /// [`Self::set_overflow_enabled`] / [`Self::set_overflow_config`]
    /// calls override the state set here.
    ///
    /// Default (no call to this builder) is
    /// `OverflowConfig::default()` with `enabled = false` —
    /// the v0.2 pull-only posture.
    pub fn with_overflow(self, config: OverflowConfig) -> Self {
        *self.overflow.write() = config;
        self
    }

    /// True iff the adapter is currently advertising
    /// `dataforts.blob.overflow` and accepting inbound
    /// `OverflowPush` requests. Cheap (one read-lock acquire);
    /// fine to call on the hot path.
    ///
    /// Returns the *runtime* state, so operators dashboarding
    /// "is overflow on" against a recently-toggled node see
    /// the live value rather than a build-time snapshot.
    pub fn overflow_enabled(&self) -> bool {
        self.overflow.read().enabled
    }

    /// Snapshot of the current overflow configuration. Returns
    /// a copy of the `OverflowConfig` (it's `Copy`); the lock
    /// is released before the return. Inspection-only; mutate
    /// via [`Self::set_overflow_enabled`] or
    /// [`Self::set_overflow_config`].
    pub fn overflow_config(&self) -> OverflowConfig {
        *self.overflow.read()
    }

    /// Flip the overflow master switch at runtime. No-op if
    /// `enabled` matches the current state. When the boolean
    /// transitions, the adapter's next capability rebroadcast
    /// adds (or removes) the `dataforts.blob.overflow` tag —
    /// peers see the change on the following announcement
    /// cycle.
    ///
    /// The adapter doesn't hold a `MeshNode` handle (the two
    /// are intentionally decoupled), so the rebroadcast itself
    /// happens through one of:
    ///
    /// - `MeshNode::announce_blob_overflow_state(adapter)` —
    ///   the convenience path: snapshots local caps, syncs the
    ///   `dataforts.blob.overflow` tag to the adapter's
    ///   current state, and announces in one call. Recommended.
    /// - Manual `announce_capabilities(updated_set)` where
    ///   `updated_set` carries the matching presence tag.
    ///
    /// Until the rebroadcast lands, the sender-side overflow
    /// tick short-circuits (the local caps snapshot doesn't yet
    /// reflect the new state — see
    /// `drive_blob_overflow_tick`) and peers reject any inbound
    /// nudge as `SenderNotOverflowing`.
    ///
    /// Cheap: one write-lock acquire, one bool store. Safe to
    /// call concurrently with reads via
    /// [`Self::overflow_enabled`] — the RwLock ensures the
    /// observed value is consistent with one toggle event.
    pub fn set_overflow_enabled(&self, enabled: bool) {
        self.overflow.write().enabled = enabled;
    }

    /// Replace the entire overflow configuration in one call.
    /// Useful when the operator wants to update thresholds
    /// (high-water, low-water, push budget, scope) without
    /// touching the master switch — pass the same `enabled`
    /// value the adapter currently has, plus the new
    /// thresholds. Or use this to atomically enable + tune in
    /// one call.
    pub fn set_overflow_config(&self, config: OverflowConfig) {
        *self.overflow.write() = config;
    }

    /// True iff the most recent overflow tick observed local
    /// disk at or above the high-water threshold (i.e. the
    /// controller is actively shedding). Mirrors the
    /// hysteresis state machine — stays `true` through the
    /// hysteresis band on the way down and only flips back to
    /// `false` once disk drops to or below the low-water
    /// threshold.
    ///
    /// Read-only observer; the tick driver is the single
    /// writer. Cheap (one atomic load) — safe to call on a
    /// dashboard hot path.
    pub fn overflow_active(&self) -> bool {
        self.overflow_active
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Internal accessor — the raw `Arc<AtomicBool>` for the
    /// hysteresis state. Crate-internal because the wire-
    /// level state machine is the only legitimate writer;
    /// operators get the read-only view via
    /// [`Self::overflow_active`]. P2 exposed this seam for an
    /// external tick driver; P4's
    /// [`Self::drive_overflow_tick`] is the in-tree caller
    /// (uses `&self.overflow_active` directly) — the public
    /// hook is still useful for tests that want to assert
    /// the atomic transitioned without driving a full tick.
    #[allow(dead_code)]
    pub(crate) fn overflow_active_handle(&self) -> &Arc<std::sync::atomic::AtomicBool> {
        &self.overflow_active
    }

    /// Convenience: drive one overflow tick + auto-record the
    /// resulting report into the adapter's metrics registry.
    /// Composes [`super::overflow::drive_blob_overflow_tick`]
    /// with [`super::metrics::BlobMetrics::record_overflow_tick`]
    /// so operators don't have to thread the report through
    /// two calls on every tick.
    ///
    /// `ctx` carries everything the controller needs that the
    /// adapter doesn't already own: the capability index, the
    /// heat registry, the sink, the local caps snapshot, and
    /// the disk-usage stats. The adapter contributes the
    /// `refcount`, `config`, and `overflow_active` hysteresis
    /// state from `self`. The closure `size_for_hash` stays
    /// separate (closures don't sit in struct fields without
    /// a `Box<dyn Fn>` wrapper that's heavier than the
    /// inlined-impl-Fn shape).
    ///
    /// The controller's `config` is read live from
    /// `self.overflow_config()` so an operator-toggled
    /// threshold lands on the next tick.
    ///
    /// Returns the [`super::overflow::BlobOverflowTickReport`]
    /// so callers can inspect per-tick state without a second
    /// metrics scrape.
    pub async fn drive_overflow_tick(
        &self,
        ctx: super::overflow::OverflowTickContext<'_>,
        size_for_hash: impl Fn([u8; 32]) -> Option<u64>,
    ) -> super::overflow::BlobOverflowTickReport {
        let config = self.overflow_config();
        let controller = super::overflow::BlobOverflowController::new(
            ctx.local_caps,
            ctx.capability_index,
            ctx.heat_registry,
            &self.refcount,
            &config,
        );
        let observation = super::overflow::OverflowTickObservation {
            disk_used_bytes: ctx.disk_used_bytes,
            disk_total_bytes: ctx.disk_total_bytes,
            hysteresis_active: &self.overflow_active,
            now: std::time::Instant::now(),
        };
        let report = super::overflow::drive_blob_overflow_tick(
            &controller,
            ctx.sink,
            observation,
            size_for_hash,
        )
        .await;
        self.metrics.record_overflow_tick(&report);
        report
    }

    /// Bump the receive-side overflow rejection counter for
    /// `reason`. Called by
    /// [`super::overflow::OverflowPushHandler`] on every
    /// inbound push that admission rejects; surfaces in the
    /// adapter's Prometheus body as
    /// `dataforts_blob_overflow_rejected_total{reason}`.
    ///
    /// The sender's own metrics bump
    /// `dataforts_blob_overflow_push_errors_total` on the same
    /// event (via the controller's `push_errors` counter);
    /// the two surfaces are complementary so operators
    /// dashboarding either side see matching volumes.
    pub fn record_overflow_reject(&self, reason: super::admission::OverflowReject) {
        self.metrics.record_overflow_reject(reason);
    }

    /// True iff this adapter is wired to bump a shared blob-heat
    /// registry on fetch.
    pub fn blob_heat_enabled(&self) -> bool {
        self.blob_heat.is_some()
    }

    /// Bump the heat counters for every chunk hash a fetch
    /// touched. No-op when no registry is wired. Pure side-effect
    /// — returns nothing. The registry's lock is a parking_lot
    /// `Mutex` which does NOT poison on panic, so any panic
    /// inside another holder leaves the registry usable; we
    /// acquire unconditionally without any poison handling.
    fn bump_heat(&self, hashes: &[[u8; 32]]) {
        if let Some(reg) = self.blob_heat.as_ref() {
            let now = std::time::Instant::now();
            let mut guard = reg.lock();
            for h in hashes {
                guard.entry_mut(*h, self.blob_heat_half_life, now).bump(now);
            }
        }
    }

    /// Run one tick of the blob-heat registry: walk every tracked
    /// hash, apply decay, ask the supplied `policy` whether to
    /// emit, and route each `Emit { rate }` / `Withdraw` decision
    /// through `sink` (as `announce_blob_heat_batch`). Returns
    /// the count of emissions that landed (Emit + Withdraw
    /// combined). PR-5j-c emission path; operators drive from a
    /// periodic task at `DataGravityPolicy::emit_interval`
    /// cadence.
    ///
    /// No-op (`Ok(0)`) when no registry is wired. The emission
    /// snapshot is taken under the registry lock; the lock is
    /// released *before* awaiting the sink, so a concurrent
    /// `fetch` on this adapter can keep bumping heat in parallel.
    /// The lock is `!Send` across `.await` — holding it past an
    /// `await` would also break the runtime's task model (a task
    /// rescheduled to a different worker while holding a thread-
    /// affine guard) — which is the real concern. parking_lot
    /// mutexes don't poison; the explicit scoping below is about
    /// preserving `Send` for the awaited future.
    pub async fn tick_blob_heat(
        &self,
        policy: &crate::adapter::net::dataforts::gravity::DataGravityPolicy,
        sink: &dyn crate::adapter::net::dataforts::gravity::BlobHeatSink,
    ) -> Result<u64, BlobError> {
        use crate::adapter::net::dataforts::gravity::HeatEmission;
        let reg = match self.blob_heat.as_ref() {
            Some(r) => r,
            None => return Ok(0),
        };
        let emissions = {
            let mut guard = reg.lock();
            guard.tick(policy, std::time::Instant::now())
        };
        let mut updates: Vec<([u8; 32], Option<f64>)> = Vec::with_capacity(emissions.len());
        for (hash, em) in &emissions {
            match em {
                HeatEmission::Emit { rate } => updates.push((*hash, Some(*rate))),
                HeatEmission::Withdraw => updates.push((*hash, None)),
                HeatEmission::Suppress => {}
            }
        }
        if !updates.is_empty() {
            sink.announce_blob_heat_batch(&updates).await.map_err(|e| {
                BlobError::Backend(format!("blob heat tick: announce batch failed: {}", e))
            })?;
        }
        Ok(updates.len() as u64)
    }

    /// Pin `hash` against GC, gated by an
    /// [`AuthGuard::is_authorized_full`] check on
    /// `(origin_hash, channel)`. Returns
    /// [`BlobError::Backend`] if the adapter has no guard
    /// configured (operator misconfiguration on the peer-facing
    /// path) or if the caller is not authorized for `channel`.
    ///
    /// `channel` is the canonical name of the chain that
    /// originally published the blob — the caller of the pin op
    /// must be authorized on that chain.
    pub fn pin_authorized(
        &self,
        hash: [u8; 32],
        origin_hash: u64,
        channel: &ChannelName,
        now_unix_ms: u64,
    ) -> Result<(), BlobError> {
        let guard = self.auth_guard.as_ref().ok_or_else(|| {
            BlobError::Unauthorized("pin_authorized requires AuthGuard wiring".to_string())
        })?;
        auth_allows_blob_op(guard, origin_hash, channel)?;
        self.refcount.pin(hash, now_unix_ms);
        Ok(())
    }

    /// Unpin `hash`, gated by an
    /// [`AuthGuard::is_authorized_full`] check on
    /// `(origin_hash, channel)`. Returns
    /// [`BlobError::Backend`] if no guard is configured or the
    /// caller is not authorized.
    pub fn unpin_authorized(
        &self,
        hash: [u8; 32],
        origin_hash: u64,
        channel: &ChannelName,
        now_unix_ms: u64,
    ) -> Result<(), BlobError> {
        let guard = self.auth_guard.as_ref().ok_or_else(|| {
            BlobError::Unauthorized("unpin_authorized requires AuthGuard wiring".to_string())
        })?;
        auth_allows_blob_op(guard, origin_hash, channel)?;
        self.refcount.unpin(hash, now_unix_ms);
        Ok(())
    }

    /// Delete a single chunk file by content hash, gated by an
    /// [`AuthGuard::is_authorized_full`] check on
    /// `(origin_hash, channel)`. Mirrors
    /// [`Self::delete_chunk`] on the success path; returns a typed
    /// `BlobError::Backend` if no guard is configured or the
    /// caller is not authorized.
    ///
    /// System-internal callers (the GC sweep) use the unauth
    /// [`Self::delete_chunk`] variant — only peer-initiated
    /// deletes route through this gate.
    pub async fn delete_chunk_authorized(
        &self,
        hash: &[u8; 32],
        origin_hash: u64,
        channel: &ChannelName,
    ) -> Result<(), BlobError> {
        let guard = self.auth_guard.as_ref().ok_or_else(|| {
            BlobError::Unauthorized("delete_chunk_authorized requires AuthGuard wiring".to_string())
        })?;
        auth_allows_blob_op(guard, origin_hash, channel)?;
        self.delete_chunk(hash).await
    }

    /// Refcount table reference. Operators bump via
    /// [`BlobRefcountTable::incr`] from chain-fold / CortEX
    /// integration sites; the adapter reads on sweep + stat
    /// paths.
    pub fn refcount_table(&self) -> &BlobRefcountTable {
        &self.refcount
    }

    /// Atomic-counter registry surfaced for Prometheus scrape.
    pub fn metrics(&self) -> &BlobMetrics {
        &self.metrics
    }

    /// Render a Prometheus-text snapshot for the operator scrape.
    /// Concatenates the counter / gauge bodies with the live
    /// `gc_pending_total` from the refcount table.
    pub fn prometheus_text(&self) -> String {
        let pending = self.refcount.zero_refcount_count() as u64;
        self.metrics
            .snapshot()
            .to_prometheus_text(&self.id, pending)
    }

    /// Pin `hash` against GC. Operator escape hatch — pinned
    /// hashes survive sweep regardless of refcount + retention
    /// floor. Returns the hash for ergonomic chaining.
    ///
    /// `now_unix_ms` should be the operator's current wall-clock
    /// — used to stamp `last_seen` and (if the hash is new)
    /// `first_seen`.
    pub fn pin(&self, hash: [u8; 32], now_unix_ms: u64) {
        self.refcount.pin(hash, now_unix_ms);
    }

    /// Unpin `hash`. After this, the hash returns to the normal
    /// refcount / retention-floor sweep contract.
    pub fn unpin(&self, hash: [u8; 32], now_unix_ms: u64) {
        self.refcount.unpin(hash, now_unix_ms);
    }

    /// Run a GC sweep. Pure-logic in two halves: decide (which
    /// hashes are deletable under the refcount + retention +
    /// pressure + pin rules), then act (delete the chunk files,
    /// remove the refcount entries, bump
    /// `dataforts_blob_gc_swept_total`). The two halves are
    /// fused here for the typical operator-driven sweep; advanced
    /// callers can invoke
    /// [`BlobRefcountTable::deletable_hashes`] +
    /// [`Self::delete_chunk`] directly for dry-run / batched
    /// flows.
    ///
    /// Returns the count of chunks actually swept (may be less
    /// than `deletable_hashes` if some chunk-file deletes failed —
    /// the failures are logged but the refcount entry is left in
    /// place so the next sweep retries).
    pub async fn sweep_gc(
        &self,
        now_unix_ms: u64,
        disk_pressure_critical: bool,
    ) -> Result<u64, BlobError> {
        let candidates = self.refcount.deletable_hashes(
            now_unix_ms,
            self.retention_floor,
            disk_pressure_critical,
        );
        let mut swept: u64 = 0;
        for hash in candidates {
            // delete_chunk drops the refcount entry on success
            // and preserves it on error so the next sweep can
            // retry.
            if self.delete_chunk(&hash).await.is_ok() {
                swept = swept.saturating_add(1);
            }
        }
        self.metrics.record_gc_swept(swept);
        Ok(swept)
    }

    /// Delete a single chunk file by content hash. The chunk's
    /// `RedexFile` is closed + removed from the Redex manager,
    /// and the refcount table entry is dropped on success so
    /// `stat()` stops surfacing a stale `last_seen_unix_ms` for a
    /// deleted blob and any subsequent re-store starts a fresh
    /// retention-floor clock. Idempotent on the success path —
    /// closing an already-closed file returns `Ok(())` from the
    /// Redex layer. Used internally by [`Self::sweep_gc`] and by
    /// the peer-initiated [`Self::delete_chunk_authorized`];
    /// reachable directly for operators running batched / dry-run
    /// flows against [`BlobRefcountTable::deletable_hashes`].
    ///
    /// On `Err` the refcount entry is preserved so the next sweep
    /// can retry — chunk-file close failures shouldn't strand the
    /// retention clock.
    pub async fn delete_chunk(&self, hash: &[u8; 32]) -> Result<(), BlobError> {
        let channel = Self::chunk_channel(hash);
        self.redex
            .close_file(&channel)
            .map_err(|e| BlobError::Backend(format!("mesh blob: close chunk: {}", e)))?;
        self.refcount.remove(hash);
        Ok(())
    }

    /// Channel name for a given chunk hash. Public accessor so
    /// e2e tests + operator tools can construct chunk channels for
    /// `Redex::open_file` / `replication_coordinator_for` lookups
    /// without re-implementing the `dataforts/blob/<hex32>` format
    /// (and risking drift).
    pub fn chunk_channel_for_hash(hash: &[u8; 32]) -> ChannelName {
        Self::chunk_channel(hash)
    }

    /// Channel name for a given chunk hash. Pure function; safe to
    /// inline.
    fn chunk_channel(hash: &[u8; 32]) -> ChannelName {
        let mut name = String::with_capacity(CHUNK_CHANNEL_PREFIX.len() + 64);
        name.push_str(CHUNK_CHANNEL_PREFIX);
        for b in hash {
            use std::fmt::Write;
            let _ = write!(name, "{:02x}", b);
        }
        ChannelName::new(&name).expect("hex-formatted name under reserved prefix is always valid")
    }

    /// `RedexFileConfig` template applied to every chunk open. The
    /// operator opts into disk persistence via [`Self::with_persistent`]
    /// and into cross-node replication via [`Self::with_replication`].
    fn chunk_file_config(&self) -> RedexFileConfig {
        let mut cfg = RedexFileConfig::new().with_persistent(self.persistent);
        if let Some(rep) = self.replication.clone() {
            cfg = cfg.with_replication(Some(rep));
        }
        cfg
    }

    /// Store a single chunk. Idempotent — if the chunk file already
    /// holds content (re-store of identical bytes against the same
    /// content-address), this is a no-op. Verifies the bytes hash
    /// to the supplied hash before writing.
    ///
    /// Concurrent stores of the same hash serialize through a per-
    /// hash advisory lock so two callers can't both observe the
    /// file empty and both append the same payload (the TOCTOU
    /// would leave the chunk file with duplicate events; reads
    /// still return correct bytes but the underlying storage
    /// wastes space and the layout is non-deterministic). The
    /// idempotent-skip branch also verifies the existing on-disk
    /// bytes against the supplied hash before accepting — a
    /// corrupted prior write (e.g. truncated replication catch-up)
    /// surfaces as `HashMismatch` rather than silently passing the
    /// honest caller's `store` call.
    async fn store_chunk(&self, hash: &[u8; 32], bytes: &[u8]) -> Result<(), BlobError> {
        // Defensive: verify the supplied bytes hash to the supplied
        // hash. The substrate-side `store` already verified at the
        // top of the call; this is a second-pass guard in case
        // this helper is called from a non-substrate path.
        let computed: [u8; 32] = blake3::hash(bytes).into();
        if computed != *hash {
            return Err(BlobError::HashMismatch {
                expected: *hash,
                actual: computed,
            });
        }
        // Per-hash serialization: one in-flight `store_chunk` per
        // content hash at a time. The lock entry is created lazily
        // and best-effort reclaimed after the store completes.
        let lock = self
            .in_flight_stores
            .entry(*hash)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let result = {
            let _guard = lock.lock().await;
            self.store_chunk_locked(hash, bytes).await
        };
        // Best-effort cleanup: drop the local Arc, then remove the
        // map entry only when no other caller is currently holding
        // it. Concurrent waiters keep the Arc alive and the entry
        // stays until the last one finishes.
        drop(lock);
        self.in_flight_stores
            .remove_if(hash, |_, m| Arc::strong_count(m) == 1);
        result
    }

    /// Body of [`Self::store_chunk`] run under the per-hash lock.
    /// Split out so the lock-acquire / cleanup wrapper can early-
    /// return cleanly via `?` without the per-hash entry leaking.
    async fn store_chunk_locked(&self, hash: &[u8; 32], bytes: &[u8]) -> Result<(), BlobError> {
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        let now_ms = now_unix_ms();
        if !file.is_empty() {
            // Idempotent fast-path. Content-addressed semantics
            // promise the on-disk bytes match the hash, but a
            // corrupted prior write (e.g. replication catch-up
            // wrote bad bytes before our honest store landed)
            // would otherwise be silently affirmed. Verify before
            // returning Ok.
            let events = file.read_range(0, file.len() as u64);
            let existing = events.into_iter().next().ok_or_else(|| {
                BlobError::Backend(
                    "mesh blob: chunk file non-empty but read returned no events".to_string(),
                )
            })?;
            let computed_existing: [u8; 32] = blake3::hash(&existing.payload).into();
            if computed_existing != *hash {
                return Err(BlobError::HashMismatch {
                    expected: *hash,
                    actual: computed_existing,
                });
            }
            self.refcount.store_observed(*hash, now_ms);
            return Ok(());
        }
        file.append(bytes)
            .map_err(|e| BlobError::Backend(format!("mesh blob: append chunk: {}", e)))?;
        self.refcount.store_observed(*hash, now_ms);
        Ok(())
    }

    /// Fetch a single chunk by hash. Returns `BlobError::NotFound`
    /// when the chunk file is absent or empty.
    async fn fetch_chunk(&self, hash: &[u8; 32]) -> Result<Vec<u8>, BlobError> {
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        let len = file.len() as u64;
        if len == 0 {
            return Err(BlobError::NotFound(format!("mesh://{}", hex32(hash))));
        }
        // Chunks are content-addressed single-event files; read seq 0.
        // Future variations (heat-sourced replicas with multi-event
        // append history) would walk the chain — out of scope here.
        let events = file.read_range(0, len);
        let first = events
            .into_iter()
            .next()
            .ok_or_else(|| BlobError::NotFound(format!("mesh://{}", hex32(hash))))?;
        let bytes = first.payload.to_vec();
        // Defense-in-depth verification — a corrupted on-disk chunk
        // shouldn't propagate silently. The substrate verifies
        // `BlobRef`-level hashes at higher layers, but per-chunk
        // verify catches the manifest-fan-out case where any single
        // bad chunk corrupts the assembled output.
        let computed: [u8; 32] = blake3::hash(&bytes).into();
        if computed != *hash {
            return Err(BlobError::HashMismatch {
                expected: *hash,
                actual: computed,
            });
        }
        Ok(bytes)
    }
}

#[async_trait]
impl BlobAdapter for MeshBlobAdapter {
    fn adapter_id(&self) -> &str {
        &self.id
    }

    fn accepted_schemes(&self) -> &[&str] {
        &["mesh"]
    }

    async fn store(&self, blob_ref: &BlobRef, bytes: &[u8]) -> Result<(), BlobError> {
        let result = match blob_ref {
            BlobRef::Small { hash, size, .. } => {
                // Size guard — caller may have stamped a wrong size
                // before publishing. Reject rather than silently
                // accept truncated content.
                if *size != bytes.len() as u64 {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Small size mismatch: declared {}, actual {}",
                        size,
                        bytes.len()
                    )));
                }
                self.store_chunk(hash, bytes).await
            }
            BlobRef::Manifest {
                chunks,
                total_size,
                encoding,
                ..
            } => {
                // Reject ReedSolomon at v0.2 — the encoding tag is
                // reserved on the wire for forward-compat; the
                // store path doesn't actually compute parity chunks.
                if !matches!(encoding, Encoding::Replicated) {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: encoding {:?} is reserved for v0.3 and \
                         not supported by the v0.2 store path",
                        encoding
                    )));
                }
                if *total_size != bytes.len() as u64 {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Manifest total_size mismatch: declared {}, actual {}",
                        total_size,
                        bytes.len()
                    )));
                }
                // Re-chunk the input and verify the resulting hash
                // list matches what the BlobRef advertises. A
                // caller that constructed a Manifest by hand with
                // hashes that don't match the bytes can't poison
                // the store.
                let recomputed = chunk_payload(bytes)?;
                let recomputed_chunks: Vec<(ChunkRef, &[u8])> = match recomputed {
                    ChunkedPayload::Chunked { chunks, .. } => chunks,
                    ChunkedPayload::Inline { payload, hash } => {
                        // Caller advertised a Manifest but the
                        // payload fits in a Small. Surface as an
                        // explicit mismatch — the BlobRef and the
                        // bytes disagree on shape.
                        let _ = (payload, hash);
                        return Err(BlobError::Backend(
                            "mesh blob: Manifest with payload ≤ chunk threshold; \
                             caller should have produced BlobRef::Small"
                                .to_owned(),
                        ));
                    }
                };
                if recomputed_chunks.len() != chunks.len() {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Manifest chunk count mismatch: declared {}, actual {}",
                        chunks.len(),
                        recomputed_chunks.len()
                    )));
                }
                for (i, (recomputed_chunk, chunk_bytes)) in recomputed_chunks.iter().enumerate() {
                    if recomputed_chunk.hash != chunks[i].hash {
                        return Err(BlobError::Backend(format!(
                            "mesh blob: chunk {} hash mismatch",
                            i,
                        )));
                    }
                    if recomputed_chunk.size != chunks[i].size {
                        return Err(BlobError::Backend(format!(
                            "mesh blob: chunk {} size mismatch",
                            i,
                        )));
                    }
                    self.store_chunk(&recomputed_chunk.hash, chunk_bytes)
                        .await?;
                }
                Ok(())
            }
        };
        if result.is_ok() {
            self.metrics.record_store(bytes.len() as u64);
        }
        result
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobError> {
        let result = match blob_ref {
            BlobRef::Small { hash, .. } => self.fetch_chunk(hash).await,
            BlobRef::Manifest {
                chunks,
                total_size: _,
                ..
            } => {
                // Let Vec grow as we extend. The declared
                // `total_size` is bounded by `BLOB_REF_MAX_SIZE`
                // (16 GiB) — pre-allocating that on a fetch of a
                // hostile manifest forces an upfront allocation
                // regardless of how many bytes actually arrive,
                // and on 32-bit targets `as usize` truncates
                // silently. Callers needing streaming on large
                // blobs should use the substrate's per-chunk
                // fetch path; this bulk-fetch hits the same
                // memory cost on first byte either way.
                let mut out: Vec<u8> = Vec::new();
                let mut err: Option<BlobError> = None;
                for chunk in chunks {
                    match self.fetch_chunk(&chunk.hash).await {
                        Ok(chunk_bytes) if chunk_bytes.len() as u64 != chunk.size as u64 => {
                            err = Some(BlobError::Backend(format!(
                                "mesh blob: chunk {} fetched size {} != declared {}",
                                hex32(&chunk.hash),
                                chunk_bytes.len(),
                                chunk.size
                            )));
                            break;
                        }
                        Ok(chunk_bytes) => {
                            out.extend_from_slice(&chunk_bytes);
                        }
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = err {
                    Err(e)
                } else {
                    Ok(out)
                }
            }
        };
        if result.is_ok() {
            self.metrics.record_fetch();
            // PR-5j-b: bump blob heat for every chunk hash a
            // successful fetch resolved. No-op when no registry
            // is wired.
            if self.blob_heat.is_some() {
                let hashes: Vec<[u8; 32]> = match blob_ref {
                    BlobRef::Small { hash, .. } => vec![*hash],
                    BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
                };
                self.bump_heat(&hashes);
            }
        }
        result
    }

    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: std::ops::Range<u64>,
    ) -> Result<Vec<u8>, BlobError> {
        if range.start > range.end {
            return Err(BlobError::Backend(format!(
                "mesh blob: range.start ({}) > range.end ({})",
                range.start, range.end
            )));
        }
        let len = range.end - range.start;
        if len == 0 {
            return Ok(Vec::new());
        }
        let (result, touched): (Result<Vec<u8>, BlobError>, Vec<[u8; 32]>) = match blob_ref {
            BlobRef::Small { hash, size, .. } => {
                if range.end > *size {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: range.end {} exceeds Small size {}",
                        range.end, size
                    )));
                }
                match self.fetch_chunk(hash).await {
                    Ok(bytes) => (
                        Ok(bytes[range.start as usize..range.end as usize].to_vec()),
                        vec![*hash],
                    ),
                    Err(e) => (Err(e), Vec::new()),
                }
            }
            BlobRef::Manifest { .. } => {
                let requests = byte_range_to_chunks(blob_ref, range.start, range.end)?;
                let mut out = Vec::with_capacity(len as usize);
                let chunks = blob_ref.chunks();
                let mut touched = Vec::with_capacity(requests.len());
                let mut err: Option<BlobError> = None;
                for req in &requests {
                    let chunk = &chunks[req.chunk_index];
                    match self.fetch_chunk(&chunk.hash).await {
                        Ok(chunk_bytes) => {
                            out.extend_from_slice(
                                &chunk_bytes
                                    [req.start_in_chunk as usize..req.end_in_chunk as usize],
                            );
                            touched.push(chunk.hash);
                        }
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = err {
                    (Err(e), Vec::new())
                } else {
                    (Ok(out), touched)
                }
            }
        };
        if result.is_ok() && !touched.is_empty() {
            self.bump_heat(&touched);
        }
        result
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        match blob_ref {
            BlobRef::Small { hash, .. } => self.chunk_exists(hash),
            BlobRef::Manifest { chunks, .. } => {
                for chunk in chunks {
                    if !self.chunk_exists(&chunk.hash)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    async fn delete(&self, _blob_ref: &BlobRef) -> Result<(), BlobError> {
        // PR-2a defers refcount-aware delete to PR-4. For now this
        // is a no-op — the GC sweep is what reclaims chunks, and
        // it's keyed on the refcount source list (chain folds /
        // CortEX indexes / out-of-band scanner) that lands later.
        // Returning `Ok(())` here matches the default trait impl;
        // we override only to make the layering explicit + to give
        // PR-4 a single place to wire in.
        Ok(())
    }

    /// Open each chunk channel against the local
    /// [`Redex`] handle using
    /// the adapter's existing `chunk_file_config`. When
    /// replication is configured + active on the underlying
    /// handle, the per-channel runtime spawned by `open_file`
    /// begins syncing from peers carrying the chunk's
    /// `causal:<hex>` advertisement — that's the cross-node fetch
    /// path. Returns `Ok(())` as soon as every chunk channel has
    /// been opened; the actual chunk arrival is asynchronous and
    /// reachable via `fetch` / `exists` once the
    /// replication-runtime sync completes.
    ///
    /// No-op when the chunk is already locally present (the
    /// `open_file` fast path on the existing entry skips the
    /// spawn; the chunk-file `len()` check on a subsequent
    /// `fetch` returns the bytes without going over the network).
    async fn prefetch(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let cfg = self.chunk_file_config();
        let hashes: Vec<[u8; 32]> = match blob_ref {
            BlobRef::Small { hash, .. } => vec![*hash],
            BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
        };
        for hash in hashes {
            let channel = Self::chunk_channel(&hash);
            self.redex.open_file(&channel, cfg.clone()).map_err(|e| {
                BlobError::Backend(format!("mesh blob: prefetch open chunk: {}", e))
            })?;
        }
        Ok(())
    }

    async fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat, BlobError> {
        // v0.2 PR-4a — `last_seen_unix_ms` now comes from the
        // refcount table when the hash is tracked. For Small
        // blobs that's the single chunk; for Manifest blobs we
        // surface the most recent touch across all chunks.
        // `replicas_observed` still 0 until the cross-node
        // advertisement count wires up (PR-5).
        let replica_target = self.replication.as_ref().map(|c| c.factor);
        let last_seen_unix_ms = match blob_ref {
            BlobRef::Small { hash, .. } => self.refcount.get(hash).map(|e| e.last_seen_unix_ms),
            BlobRef::Manifest { chunks, .. } => chunks
                .iter()
                .filter_map(|c| self.refcount.get(&c.hash).map(|e| e.last_seen_unix_ms))
                .max(),
        };
        Ok(BlobStat {
            size: blob_ref.size(),
            replicas_observed: 0,
            replica_target,
            last_seen_unix_ms,
            encoding: blob_ref.encoding(),
        })
    }

    async fn list(
        &self,
        opts: &super::adapter::BlobListOptions,
    ) -> Result<Vec<super::adapter::BlobInventoryEntry>, BlobError> {
        // Parse the caller's hex prefix into a byte pattern up
        // front so the per-entry filter doesn't allocate a 64-
        // char hex string just to throw it away. An invalid
        // prefix (non-hex character) matches nothing — a typo
        // in the operator's search box shouldn't crash the
        // BLOBS tab or surface as an error.
        let pattern = opts.prefix_hex.as_deref().map(parse_hex_prefix);
        if matches!(pattern, Some(None)) {
            return Ok(Vec::new());
        }
        let pattern = pattern.flatten();
        // Pull a stable, prefix-filtered snapshot in one pass —
        // entries that don't match the prefix never touch the
        // output Vec, and we skip hex-encoding their hashes.
        // The typical adapter holds tens of thousands of
        // entries; a narrow prefix against that scale is the
        // hot path Deck operators actually take.
        let raw = self.refcount.snapshot_filter(|hash| match &pattern {
            Some(pat) => hash_matches_pattern(hash, pat),
            None => true,
        });
        let mut entries: Vec<super::adapter::BlobInventoryEntry> = raw
            .into_iter()
            .map(|(hash, e)| super::adapter::BlobInventoryEntry {
                adapter_id: self.id.clone(),
                hash_hex: hex_encode(&hash),
                refcount: e.refcount,
                pinned: e.pinned,
                first_seen_unix_ms: e.first_seen_unix_ms,
                last_seen_unix_ms: e.last_seen_unix_ms,
            })
            .collect();
        // Most-recently-touched first — operators triaging
        // an incident want the freshest churn at the top.
        entries.sort_by(|a, b| b.last_seen_unix_ms.cmp(&a.last_seen_unix_ms));
        if opts.limit > 0 && entries.len() > opts.limit {
            entries.truncate(opts.limit);
        }
        Ok(entries)
    }
}

/// Pattern for matching a hex prefix against a raw `[u8; 32]`
/// without allocating the entry's hex string. `full_bytes` is the
/// strict byte prefix (one byte per two hex chars); `half_nibble`
/// is the high nibble of an odd-length prefix's trailing
/// character, paired with the byte index that nibble compares
/// against. `None` for the half-nibble when the prefix length is
/// even.
#[derive(Debug, Clone)]
struct HexPrefixPattern {
    full_bytes: Vec<u8>,
    half_nibble: Option<(usize, u8)>,
}

/// Parse a hex prefix into a [`HexPrefixPattern`]. Returns
/// `None` on any non-hex character so the caller can short-
/// circuit to an empty result. An empty prefix yields an
/// always-matching pattern.
fn parse_hex_prefix(prefix: &str) -> Option<HexPrefixPattern> {
    let lower = prefix.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut full_bytes = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        full_bytes.push((hi << 4) | lo);
        i += 2;
    }
    let half_nibble = if i < bytes.len() {
        Some((full_bytes.len(), hex_nibble(bytes[i])?))
    } else {
        None
    };
    Some(HexPrefixPattern {
        full_bytes,
        half_nibble,
    })
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn hash_matches_pattern(hash: &[u8; 32], pat: &HexPrefixPattern) -> bool {
    if pat.full_bytes.len() > hash.len() {
        return false;
    }
    if hash[..pat.full_bytes.len()] != pat.full_bytes[..] {
        return false;
    }
    if let Some((idx, nibble)) = pat.half_nibble {
        if idx >= hash.len() {
            return false;
        }
        if (hash[idx] >> 4) != nibble {
            return false;
        }
    }
    true
}

/// Lowercase-hex render of a 32-byte hash. Inline to avoid a
/// `hex` crate dependency here; the substrate already has
/// `blake3::Hash::to_hex` but we hold raw `[u8; 32]` keys.
fn hex_encode(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

impl MeshBlobAdapter {
    /// Local-storage existence probe — checks the chunk file is open
    /// with non-zero length. Sync; the `BlobAdapter::exists` async
    /// wrapper above just routes here.
    ///
    /// Side effect: when the adapter is configured with
    /// [`MeshBlobAdapter::with_replication`], the underlying
    /// `Redex::open_file` registers the chunk channel with the
    /// replication runtime as part of the open. A pure
    /// "probe-without-side-effects" semantic would require a
    /// `stat`-only path that doesn't go through `open_file`;
    /// today, an `exists` query on a not-yet-locally-resident
    /// hash will cause the substrate to begin advertising +
    /// pulling that hash. Callers running long-tail existence
    /// scans against an arbitrarily-large hash list should be
    /// aware that the side effect compounds.
    fn chunk_exists(&self, hash: &[u8; 32]) -> Result<bool, BlobError> {
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        Ok(!file.is_empty())
    }

    /// Flush every chunk file referenced by `blob_ref` to disk.
    /// Used by `publish_with_blob` (see
    /// `super::publish_with_blob`) under
    /// [`BlobDurability::DurableOnLocal`](crate::adapter::net::dataforts::BlobDurability::DurableOnLocal)
    /// to satisfy "blob survives local node restart" before the
    /// publish step. No-op for `BestEffort`; `ReplicatedTo(n)`
    /// composes this with a wait-for-replicas poll above.
    ///
    /// Iterates `BlobRef::Small` as a single chunk; iterates
    /// `BlobRef::Manifest` over every `ChunkRef`. Each chunk's
    /// underlying `RedexFile::sync` runs sequentially — the call
    /// order is stable but partial-progress on error means some
    /// chunks may have been flushed before the failure point.
    /// Surface as `BlobError::Backend` for the operator to
    /// retry / inspect.
    pub async fn sync_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let hashes: Vec<[u8; 32]> = match blob_ref {
            BlobRef::Small { hash, .. } => vec![*hash],
            BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
        };
        for hash in hashes {
            let channel = Self::chunk_channel(&hash);
            // `get_file` returns `None` if no file is registered;
            // a sync of a not-yet-stored chunk is a layering bug,
            // surface a typed error.
            let file = self.redex.get_file(&channel).ok_or_else(|| {
                BlobError::NotFound(format!(
                    "mesh blob: chunk {} not stored locally — sync_blob \
                     requires prior store",
                    hex32(&hash)
                ))
            })?;
            file.sync()
                .map_err(|e| BlobError::Backend(format!("mesh blob: chunk sync: {}", e)))?;
        }
        Ok(())
    }
}

use super::hex32;

/// Wall-clock unix milliseconds. Used for refcount-table
/// `first_seen` / `last_seen` stamps. Saturates at 0 if the system
/// clock is set before the unix epoch — pathological but possible
/// in test harnesses.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::blob_ref::BLOB_CHUNK_SIZE_BYTES;
    use super::*;

    fn make_adapter() -> MeshBlobAdapter {
        let redex = Arc::new(Redex::new());
        MeshBlobAdapter::new("mesh-test", redex)
    }

    /// BLAKE3 a payload + wrap as a `BlobRef::Small`.
    fn small_ref_for(payload: &[u8]) -> BlobRef {
        let hash: [u8; 32] = blake3::hash(payload).into();
        BlobRef::small(
            format!("mesh://{}", hex32(&hash)),
            hash,
            payload.len() as u64,
        )
    }

    #[tokio::test]
    async fn store_fetch_small_round_trip() {
        let adapter = make_adapter();
        let payload = b"the small blob payload".to_vec();
        let blob = small_ref_for(&payload);

        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    #[tokio::test]
    async fn list_enumerates_stored_chunks_with_metadata() {
        use super::super::adapter::BlobListOptions;
        let adapter = make_adapter();
        // Store three distinct payloads → three distinct chunk
        // hashes land in the refcount table via the store path.
        for payload in [
            b"blob-one".to_vec(),
            b"blob-two-other-bytes".to_vec(),
            b"blob-three-with-still-different".to_vec(),
        ] {
            let blob = small_ref_for(&payload);
            adapter.store(&blob, &payload).await.unwrap();
        }
        // No filter → every entry comes back. Sort order is
        // last-seen-desc; we only assert the set since the
        // three stores land in the same millisecond on most
        // hosts.
        let entries = adapter.list(&BlobListOptions::default()).await.unwrap();
        assert_eq!(entries.len(), 3, "all three stored chunks should enumerate");
        for e in &entries {
            assert_eq!(e.hash_hex.len(), 64, "32-byte hash → 64 hex chars");
            assert!(e.last_seen_unix_ms > 0);
            assert!(e.first_seen_unix_ms <= e.last_seen_unix_ms);
        }
    }

    #[tokio::test]
    async fn list_prefix_filter_narrows_to_matching_hash() {
        use super::super::adapter::BlobListOptions;
        let adapter = make_adapter();
        let payload = b"prefix-target".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let all = adapter.list(&BlobListOptions::default()).await.unwrap();
        assert_eq!(all.len(), 1);
        let prefix = all[0].hash_hex[..4].to_string();
        let narrowed = adapter
            .list(&BlobListOptions {
                prefix_hex: Some(prefix.clone()),
                limit: 0,
            })
            .await
            .unwrap();
        assert_eq!(narrowed.len(), 1);
        assert!(narrowed[0].hash_hex.starts_with(&prefix));
        // Bogus prefix → empty result.
        let empty = adapter
            .list(&BlobListOptions {
                prefix_hex: Some("zzz".to_string()),
                limit: 0,
            })
            .await
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn list_odd_length_prefix_matches_high_nibble() {
        // Odd-length hex prefixes are a real path in the
        // Deck's BLOBS tab (operators type three-or-five-hex-
        // char prefixes when they only remember the leading
        // nibbles). The matcher must compare the trailing
        // nibble against the high half of the next byte —
        // pinning that here so a future refactor can't quietly
        // round odd prefixes down to the even-length case.
        use super::super::adapter::BlobListOptions;
        let adapter = make_adapter();
        let payload = b"odd-prefix-target".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let all = adapter.list(&BlobListOptions::default()).await.unwrap();
        assert_eq!(all.len(), 1);
        let prefix_odd = all[0].hash_hex[..3].to_string();
        let narrowed = adapter
            .list(&BlobListOptions {
                prefix_hex: Some(prefix_odd.clone()),
                limit: 0,
            })
            .await
            .unwrap();
        assert_eq!(narrowed.len(), 1);
        assert!(narrowed[0].hash_hex.starts_with(&prefix_odd));
        // The same odd prefix's leading nibble flipped should
        // miss the hash entirely.
        let mut flipped: Vec<u8> = prefix_odd.bytes().collect();
        let last = *flipped.last().unwrap();
        flipped.pop();
        // Pick any other hex digit for the trailing nibble.
        let other = if last == b'0' { b'1' } else { b'0' };
        flipped.push(other);
        let flipped = String::from_utf8(flipped).unwrap();
        let missed = adapter
            .list(&BlobListOptions {
                prefix_hex: Some(flipped),
                limit: 0,
            })
            .await
            .unwrap();
        assert!(missed.is_empty(), "flipped nibble must not match");
    }

    #[tokio::test]
    async fn list_invalid_hex_prefix_returns_empty_not_error() {
        // A typo in the operator's search box should produce
        // an empty result, not crash the tab or return Err.
        use super::super::adapter::BlobListOptions;
        let adapter = make_adapter();
        adapter
            .store(&small_ref_for(b"bytes"), b"bytes".as_ref())
            .await
            .unwrap();
        let out = adapter
            .list(&BlobListOptions {
                prefix_hex: Some("not-hex".into()),
                limit: 0,
            })
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn list_limit_caps_result_count() {
        use super::super::adapter::BlobListOptions;
        let adapter = make_adapter();
        for i in 0u32..5 {
            let payload = format!("payload-{i}").into_bytes();
            let blob = small_ref_for(&payload);
            adapter.store(&blob, &payload).await.unwrap();
        }
        let limited = adapter
            .list(&BlobListOptions {
                prefix_hex: None,
                limit: 2,
            })
            .await
            .unwrap();
        assert_eq!(limited.len(), 2, "limit caps the result count");
    }

    #[tokio::test]
    async fn store_is_idempotent_for_identical_bytes() {
        let adapter = make_adapter();
        let payload = b"idempotent".to_vec();
        let blob = small_ref_for(&payload);

        adapter.store(&blob, &payload).await.unwrap();
        // Second store of identical content must succeed — content-
        // addressed storage is naturally idempotent.
        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    /// Concurrent stores of the same hash must serialize through
    /// the per-hash advisory lock. Pre-fix, two callers could each
    /// observe `file.is_empty() == true` and both `append`, leaving
    /// the chunk file with duplicate events. The fetch path reads
    /// the first event so reads stayed correct, but the on-disk
    /// layout was non-deterministic and wasted space. Post-fix,
    /// exactly one append lands; the second caller's fast-path
    /// observes the bytes and skips.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_store_chunk_serializes_per_hash() {
        let adapter = make_adapter();
        let payload = b"concurrent serialize".to_vec();
        let blob = small_ref_for(&payload);

        // Fire N parallel stores of the same content.
        let n = 16;
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let adapter = adapter.clone();
            let blob = blob.clone();
            let payload = payload.clone();
            handles.push(tokio::spawn(
                async move { adapter.store(&blob, &payload).await },
            ));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        // Fetch must return the original bytes — and *only* the
        // original bytes. A pre-fix run could leave the file with
        // duplicate events; the read path takes the first event so
        // the bytes still match, but we can additionally inspect
        // the underlying chunk channel to assert exactly one event.
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
        let hash = match &blob {
            BlobRef::Small { hash, .. } => *hash,
            _ => panic!("expected Small"),
        };
        let channel = MeshBlobAdapter::chunk_channel_for_hash(&hash);
        let file = adapter
            .redex
            .open_file(&channel, RedexFileConfig::new())
            .unwrap();
        let events = file.read_range(0, file.len() as u64);
        assert_eq!(
            events.len(),
            1,
            "per-hash serialization must coalesce concurrent stores to one append"
        );
    }

    /// Idempotent fast-path must verify the existing on-disk
    /// bytes against the supplied hash. A pre-existing corrupted
    /// payload at the same channel (e.g. truncated replication
    /// catch-up) surfaces as `HashMismatch` rather than silently
    /// being affirmed by an honest caller's `store`.
    #[tokio::test]
    async fn store_chunk_idempotent_path_verifies_existing_bytes() {
        use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;
        let adapter = make_adapter();
        // Pre-poison the chunk channel for our intended hash with
        // bytes that DON'T hash to the advertised value.
        let intended_payload = b"honest payload".to_vec();
        let intended_hash: [u8; 32] = blake3::hash(&intended_payload).into();
        let channel = MeshBlobAdapter::chunk_channel_for_hash(&intended_hash);
        let file = adapter
            .redex
            .open_file(&channel, RedexFileConfig::new())
            .unwrap();
        // Append corrupted content (hash mismatch). Bypasses the
        // adapter's verify because we're writing directly to the
        // RedEX layer.
        file.append(b"corrupted content").unwrap();

        // Now an honest caller tries to store the intended payload.
        // The adapter must NOT silently pass — the on-disk content
        // doesn't match the advertised hash.
        let blob = BlobRef::small(
            "mesh://verify",
            intended_hash,
            intended_payload.len() as u64,
        );
        let err = adapter.store(&blob, &intended_payload).await.unwrap_err();
        assert!(
            matches!(err, BlobError::HashMismatch { .. }),
            "idempotent fast-path must verify existing bytes; got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn store_rejects_size_mismatch_on_small() {
        let adapter = make_adapter();
        let payload = b"truth".to_vec();
        let hash: [u8; 32] = blake3::hash(&payload).into();
        // Caller stamps a wrong size on the BlobRef.
        let lying = BlobRef::small("mesh://lie", hash, 999);
        let err = adapter.store(&lying, &payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }

    #[tokio::test]
    async fn store_rejects_bytes_that_dont_hash_to_advertised() {
        let adapter = make_adapter();
        let advertised: &[u8] = b"truth";
        let attempted: &[u8] = b"a lie";
        let hash: [u8; 32] = blake3::hash(advertised).into();
        let blob = BlobRef::small("mesh://tamper", hash, attempted.len() as u64);
        let err = adapter.store(&blob, attempted).await.unwrap_err();
        // Either HashMismatch (from store_chunk verify) or Backend
        // (size mismatch fires first if sizes differ); both are
        // acceptable as long as the store rejects.
        assert!(matches!(
            err,
            BlobError::HashMismatch { .. } | BlobError::Backend(_)
        ));
    }

    #[tokio::test]
    async fn fetch_missing_returns_not_found() {
        let adapter = make_adapter();
        let blob = BlobRef::small("mesh://ghost", [0xFF; 32], 0);
        let err = adapter.fetch(&blob).await.unwrap_err();
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    #[tokio::test]
    async fn exists_reports_correctly() {
        let adapter = make_adapter();
        let payload = b"existential".to_vec();
        let blob = small_ref_for(&payload);
        assert!(!adapter.exists(&blob).await.unwrap());
        adapter.store(&blob, &payload).await.unwrap();
        assert!(adapter.exists(&blob).await.unwrap());
    }

    #[tokio::test]
    async fn store_fetch_manifest_multi_chunk() {
        let adapter = make_adapter();
        // Payload large enough to chunk: 4 MiB + a bit.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        // Drive chunking via the pure-logic helper, then build the
        // BlobRef::Manifest the same way an honest caller would.
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            ChunkedPayload::Inline { .. } => panic!("expected Chunked for >4MiB payload"),
        };
        let blob = BlobRef::manifest("mesh://multi", Encoding::Replicated, chunk_refs).unwrap();

        adapter.store(&blob, &payload).await.unwrap();
        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(fetched, payload);
    }

    #[tokio::test]
    async fn fetch_range_against_manifest_returns_correct_slice() {
        let adapter = make_adapter();
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize * 2 + 500))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest("mesh://range", Encoding::Replicated, chunk_refs).unwrap();
        adapter.store(&blob, &payload).await.unwrap();

        // Pick a range that spans the first / second chunk boundary.
        let start = BLOB_CHUNK_SIZE_BYTES - 100;
        let end = BLOB_CHUNK_SIZE_BYTES + 100;
        let fetched = adapter.fetch_range(&blob, start..end).await.unwrap();
        assert_eq!(fetched, payload[start as usize..end as usize]);
    }

    #[tokio::test]
    async fn fetch_range_against_small() {
        let adapter = make_adapter();
        let payload = b"hello world, mesh blob adapter".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let fetched = adapter.fetch_range(&blob, 6..11).await.unwrap();
        assert_eq!(fetched, b"world");
    }

    #[tokio::test]
    async fn store_rejects_reed_solomon_encoding() {
        let adapter = make_adapter();
        let payload: Vec<u8> = vec![0xAA; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let chunk_refs: Vec<ChunkRef> = match chunk_payload(&payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest(
            "mesh://rs",
            Encoding::ReedSolomon { k: 4, m: 2 },
            chunk_refs,
        )
        .unwrap();
        let err = adapter.store(&blob, &payload).await.unwrap_err();
        // ReedSolomon is reserved for v0.3 — store rejects.
        assert!(matches!(err, BlobError::Backend(_)));
    }

    #[tokio::test]
    async fn stat_returns_size_plus_metadata() {
        let adapter = make_adapter();
        let payload = b"observable".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let stat = adapter.stat(&blob).await.unwrap();
        assert_eq!(stat.size, payload.len() as u64);
        assert!(stat.replicas_observed == 0); // PR-2b lands the capability count
        assert_eq!(stat.replica_target, None); // None — no replication configured
        assert_eq!(stat.encoding, None); // Small has no encoding
    }

    #[tokio::test]
    async fn stat_surfaces_replica_target_when_replication_set() {
        // We can't actually exercise replication without a mesh —
        // but we can pin that the `replica_target` field reflects
        // the operator's config when set.
        use crate::adapter::net::redex::PlacementStrategy;
        let redex = Arc::new(Redex::new());
        let rep = ReplicationConfig {
            factor: 3,
            placement: PlacementStrategy::Standard,
            ..ReplicationConfig::default()
        };
        let adapter = MeshBlobAdapter::new("mesh-rep", redex).with_replication(rep);
        let blob = BlobRef::small("mesh://x", [0; 32], 0);
        let stat = adapter.stat(&blob).await.unwrap();
        assert_eq!(stat.replica_target, Some(3));
    }

    #[tokio::test]
    async fn delete_is_noop_in_pr2a() {
        // PR-2a's delete is a no-op pending the PR-4 refcount work.
        // Pin the contract so a future change is visible.
        let adapter = make_adapter();
        let blob = BlobRef::small("mesh://x", [0; 32], 0);
        adapter.delete(&blob).await.unwrap();
    }

    #[tokio::test]
    async fn manifest_store_rejects_size_mismatch() {
        let adapter = make_adapter();
        let real_payload: Vec<u8> = vec![0xAA; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let chunk_refs: Vec<ChunkRef> = match chunk_payload(&real_payload).unwrap() {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest("mesh://x", Encoding::Replicated, chunk_refs).unwrap();
        // Try storing a payload of the wrong size.
        let fake_payload: Vec<u8> = vec![0xBB; 500];
        let err = adapter.store(&blob, &fake_payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }

    // --- PR-4a: refcount + GC + metrics + pinning ---

    #[tokio::test]
    async fn store_records_into_refcount_table() {
        let adapter = make_adapter();
        let payload = b"refcount tracked".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let hash = blob.small_hash().unwrap();
        let entry = adapter.refcount_table().get(hash).expect("hash tracked");
        assert_eq!(entry.refcount, 0); // store_observed doesn't bump refcount
        assert!(entry.first_seen_unix_ms > 0);
        assert!(!entry.pinned);
    }

    #[tokio::test]
    async fn store_increments_metrics() {
        let adapter = make_adapter();
        let payload = b"metric me".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let snap = adapter.metrics().snapshot();
        assert_eq!(snap.blobs_stored_total, 1);
        assert_eq!(snap.bytes_stored_total, payload.len() as u64);
    }

    #[tokio::test]
    async fn fetch_increments_metrics() {
        let adapter = make_adapter();
        let payload = b"fetch me".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let _ = adapter.fetch(&blob).await.unwrap();
        assert_eq!(adapter.metrics().snapshot().blobs_fetched_total, 1);
    }

    #[tokio::test]
    async fn pin_protects_hash_from_gc() {
        let adapter = make_adapter().with_retention_floor(std::time::Duration::from_millis(0));
        let payload = b"pinned forever".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let hash = *blob.small_hash().unwrap();
        adapter.pin(hash, now_unix_ms());

        // Zero retention floor + zero refcount + pinned: sweep
        // must NOT touch it.
        let swept = adapter
            .sweep_gc(now_unix_ms() + 1_000_000, false)
            .await
            .unwrap();
        assert_eq!(swept, 0);
        assert!(adapter.exists(&blob).await.unwrap());
    }

    #[tokio::test]
    async fn unpin_returns_hash_to_normal_sweep_contract() {
        let adapter = make_adapter().with_retention_floor(std::time::Duration::from_millis(0));
        let payload = b"unpin me".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let hash = *blob.small_hash().unwrap();
        let now = now_unix_ms();
        adapter.pin(hash, now);
        adapter.unpin(hash, now);

        // After unpin, sweep should remove the chunk.
        let swept = adapter.sweep_gc(now + 1_000_000, false).await.unwrap();
        assert_eq!(swept, 1);
    }

    #[tokio::test]
    async fn sweep_gc_skips_under_disk_pressure() {
        let adapter = make_adapter().with_retention_floor(std::time::Duration::from_millis(0));
        let payload = b"pressured".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let now = now_unix_ms();

        // Critical disk pressure: don't make a bad day worse.
        let swept = adapter.sweep_gc(now + 1_000_000, true).await.unwrap();
        assert_eq!(swept, 0);
    }

    #[tokio::test]
    async fn sweep_gc_records_swept_count_in_metrics() {
        let adapter = make_adapter().with_retention_floor(std::time::Duration::from_millis(0));
        for i in 0..3u8 {
            let payload = vec![i; 100];
            let blob = small_ref_for(&payload);
            adapter.store(&blob, &payload).await.unwrap();
        }
        let now = now_unix_ms();
        let swept = adapter.sweep_gc(now + 1_000_000, false).await.unwrap();
        assert_eq!(swept, 3);
        let snap = adapter.metrics().snapshot();
        assert_eq!(snap.gc_swept_total, 3);
    }

    #[tokio::test]
    async fn stat_surfaces_last_seen_from_refcount_table() {
        let adapter = make_adapter();
        let payload = b"stat me".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let stat = adapter.stat(&blob).await.unwrap();
        assert!(stat.last_seen_unix_ms.is_some());
        assert!(stat.last_seen_unix_ms.unwrap() > 0);
    }

    #[tokio::test]
    async fn prometheus_text_includes_gc_pending_count() {
        let adapter = make_adapter();
        let payload = b"pending".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        let text = adapter.prometheus_text();
        assert!(text.contains("dataforts_blob_gc_pending"));
        assert!(text.contains("dataforts_blobs_stored_total"));
    }

    #[tokio::test]
    async fn with_disk_capacity_sets_the_gauge() {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new("mesh-cap", redex).with_disk_capacity(1 << 30);
        let snap = adapter.metrics().snapshot();
        assert_eq!(snap.disk_capacity_bytes, 1 << 30);
    }

    #[tokio::test]
    async fn manifest_store_rejects_chunk_hash_mismatch() {
        let adapter = make_adapter();
        // Build a chunk list pointing at bogus hashes, then try to
        // store the *correct* bytes against it. Should reject
        // because the recomputed chunk hashes don't match.
        let payload: Vec<u8> = vec![0xCC; BLOB_CHUNK_SIZE_BYTES as usize + 1];
        let bogus_chunks = vec![
            ChunkRef {
                hash: [0; 32],
                size: BLOB_CHUNK_SIZE_BYTES as u32,
            },
            ChunkRef {
                hash: [1; 32],
                size: 1,
            },
        ];
        let blob = BlobRef::manifest("mesh://x", Encoding::Replicated, bogus_chunks).unwrap();
        let err = adapter.store(&blob, &payload).await.unwrap_err();
        assert!(matches!(err, BlobError::Backend(_)));
    }

    // --- G-6 AuthGuard wiring on pin / unpin / delete_chunk ---

    fn auth_channel() -> ChannelName {
        ChannelName::new("dataforts/auth-test").unwrap()
    }

    fn other_channel() -> ChannelName {
        ChannelName::new("dataforts/other").unwrap()
    }

    fn adapter_with_authorized_origin(origin_hash: u64) -> (MeshBlobAdapter, ChannelName) {
        let redex = Arc::new(Redex::new());
        let guard = Arc::new(AuthGuard::new());
        let channel = auth_channel();
        guard.allow_channel(origin_hash, &channel);
        let adapter = MeshBlobAdapter::new("mesh-auth-test", redex).with_auth_guard(guard);
        (adapter, channel)
    }

    #[test]
    fn pin_authorized_admits_when_origin_is_in_acl() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let hash = [0x11_u8; 32];
        adapter
            .pin_authorized(hash, origin, &channel, 1_000)
            .unwrap();
        // Pinned entries are deletable=false under sweep — verify
        // via the refcount table accessor.
        assert!(adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
    }

    #[test]
    fn pin_authorized_rejects_when_origin_not_in_acl() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let hash = [0x22_u8; 32];
        let intruder: u64 = 0xDEAD_BEEF;
        let err = adapter
            .pin_authorized(hash, intruder, &channel, 1_000)
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
        assert!(!adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
    }

    #[test]
    fn pin_authorized_rejects_when_origin_authorized_for_different_channel() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, _) = adapter_with_authorized_origin(origin);
        let wrong = other_channel();
        let hash = [0x33_u8; 32];
        let err = adapter
            .pin_authorized(hash, origin, &wrong, 1_000)
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
        assert!(!adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
    }

    #[test]
    fn pin_authorized_rejects_when_no_guard_configured() {
        let adapter = make_adapter();
        let hash = [0x44_u8; 32];
        let channel = auth_channel();
        let err = adapter
            .pin_authorized(hash, 0xCAFE_BABE, &channel, 1_000)
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    #[test]
    fn unpin_authorized_round_trips_against_pinned_hash() {
        let origin: u64 = 0xC0FFEE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let hash = [0x55_u8; 32];
        adapter
            .pin_authorized(hash, origin, &channel, 1_000)
            .unwrap();
        assert!(adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
        adapter
            .unpin_authorized(hash, origin, &channel, 2_000)
            .unwrap();
        assert!(!adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
    }

    #[test]
    fn unpin_authorized_rejects_unauthorized_origin() {
        let origin: u64 = 0xC0FFEE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let hash = [0x66_u8; 32];
        adapter
            .pin_authorized(hash, origin, &channel, 1_000)
            .unwrap();
        let intruder: u64 = 0xBAAD_F00D;
        let err = adapter
            .unpin_authorized(hash, intruder, &channel, 2_000)
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
        // Pin must still be in place — auth failure cannot remove it.
        assert!(adapter
            .refcount_table()
            .get(&hash)
            .map(|e| e.pinned)
            .unwrap_or(false));
    }

    #[test]
    fn unpin_authorized_rejects_when_no_guard_configured() {
        let adapter = make_adapter();
        let hash = [0x77_u8; 32];
        let channel = auth_channel();
        let err = adapter
            .unpin_authorized(hash, 0xCAFE_BABE, &channel, 1_000)
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn delete_chunk_authorized_admits_when_origin_in_acl() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let payload = b"authorized delete".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        assert!(adapter.exists(&blob).await.unwrap());

        let hash = match &blob {
            BlobRef::Small { hash, .. } => *hash,
            _ => panic!("expected Small"),
        };
        // Pre-condition: refcount entry exists from the store.
        assert!(adapter.refcount_table().get(&hash).is_some());

        adapter
            .delete_chunk_authorized(&hash, origin, &channel)
            .await
            .unwrap();
        // The chunk file is closed — fetch surfaces NotFound.
        let err = adapter.fetch(&blob).await.unwrap_err();
        assert!(matches!(err, BlobError::NotFound(_)));
        // Refcount entry must be cleaned up alongside the chunk file,
        // so stat() stops reporting a stale last_seen and any
        // subsequent re-store starts a fresh retention-floor clock.
        assert!(
            adapter.refcount_table().get(&hash).is_none(),
            "authorized delete must drop the refcount entry"
        );
        let stat = adapter.stat(&blob).await.unwrap();
        assert!(
            stat.last_seen_unix_ms.is_none(),
            "stat must not surface a stale last_seen for a deleted blob"
        );
    }

    #[tokio::test]
    async fn delete_chunk_authorized_rejects_unauthorized_origin() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let payload = b"protected".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let hash = match &blob {
            BlobRef::Small { hash, .. } => *hash,
            _ => panic!("expected Small"),
        };
        let intruder: u64 = 0xDEAD_BEEF;
        let err = adapter
            .delete_chunk_authorized(&hash, intruder, &channel)
            .await
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
        // Chunk must still be readable — failed auth cannot delete.
        assert!(adapter.exists(&blob).await.unwrap());
    }

    #[tokio::test]
    async fn delete_chunk_authorized_rejects_when_no_guard_configured() {
        let adapter = make_adapter();
        let hash = [0x88_u8; 32];
        let channel = auth_channel();
        let err = adapter
            .delete_chunk_authorized(&hash, 0xCAFE_BABE, &channel)
            .await
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    // --- PR-5j-b: blob heat bumps on fetch ---

    #[tokio::test]
    async fn fetch_bumps_blob_heat_when_registry_wired() {
        use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
        let redex = Arc::new(Redex::new());
        let registry = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
        let adapter = MeshBlobAdapter::new("mesh-heat", redex)
            .with_blob_heat(registry.clone(), DEFAULT_BLOB_HEAT_HALF_LIFE);
        assert!(adapter.blob_heat_enabled());

        let payload = b"hot blob".to_vec();
        let blob = small_ref_for(&payload);
        let hash = match &blob {
            BlobRef::Small { hash, .. } => *hash,
            _ => panic!("expected Small"),
        };
        adapter.store(&blob, &payload).await.unwrap();

        // First fetch initializes the counter at rate=1.
        let _ = adapter.fetch(&blob).await.unwrap();
        {
            let guard = registry.lock();
            let counter = guard.get(&hash).expect("heat entry must exist after fetch");
            assert!(counter.rate() > 0.0, "rate must be > 0 after one fetch");
        }

        // Second fetch bumps the same counter — rate climbs (modulo
        // decay, which is negligible over the test's tight window).
        let _ = adapter.fetch(&blob).await.unwrap();
        let after_second = registry.lock().get(&hash).map(|c| c.rate()).unwrap_or(0.0);
        assert!(
            after_second >= 1.0,
            "rate must remain >= 1.0 after second fetch (got {after_second})"
        );
    }

    #[tokio::test]
    async fn fetch_without_heat_registry_is_silent() {
        let adapter = make_adapter();
        assert!(!adapter.blob_heat_enabled());
        let payload = b"silent fetch".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();
        // Fetch succeeds and doesn't touch any registry (there
        // isn't one to touch — the assertion is implicit: no panic).
        let bytes = adapter.fetch(&blob).await.unwrap();
        assert_eq!(bytes, payload);
    }

    /// Recorder sink — captures every announce / withdraw call.
    /// Used by the tick tests to assert on the emitted sequence.
    #[derive(Default)]
    struct RecorderBlobHeatSink {
        announces: parking_lot::Mutex<Vec<([u8; 32], f64)>>,
        withdraws: parking_lot::Mutex<Vec<[u8; 32]>>,
    }

    #[async_trait]
    impl crate::adapter::net::dataforts::gravity::BlobHeatSink for RecorderBlobHeatSink {
        async fn announce_blob_heat(
            &self,
            hash: [u8; 32],
            rate: f64,
        ) -> Result<(), crate::error::AdapterError> {
            self.announces.lock().push((hash, rate));
            Ok(())
        }
        async fn withdraw_blob_heat(
            &self,
            hash: [u8; 32],
        ) -> Result<(), crate::error::AdapterError> {
            self.withdraws.lock().push(hash);
            Ok(())
        }
    }

    #[tokio::test]
    async fn tick_blob_heat_no_op_without_registry() {
        let adapter = make_adapter();
        let sink = RecorderBlobHeatSink::default();
        let policy = crate::adapter::net::dataforts::gravity::DataGravityPolicy::default();
        let emitted = adapter.tick_blob_heat(&policy, &sink).await.unwrap();
        assert_eq!(emitted, 0);
        assert!(sink.announces.lock().is_empty());
    }

    #[tokio::test]
    async fn tick_blob_heat_emits_after_repeated_fetches() {
        use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
        let redex = Arc::new(Redex::new());
        let registry = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
        let adapter = MeshBlobAdapter::new("mesh-heat-tick", redex)
            .with_blob_heat(registry.clone(), DEFAULT_BLOB_HEAT_HALF_LIFE);

        let payload = b"hot tick".to_vec();
        let blob = small_ref_for(&payload);
        let hash = match &blob {
            BlobRef::Small { hash, .. } => *hash,
            _ => panic!("expected Small"),
        };
        adapter.store(&blob, &payload).await.unwrap();

        // Build up heat with several reads.
        for _ in 0..8 {
            adapter.fetch(&blob).await.unwrap();
        }

        let sink = RecorderBlobHeatSink::default();
        let policy = crate::adapter::net::dataforts::gravity::DataGravityPolicy::default();
        let emitted = adapter.tick_blob_heat(&policy, &sink).await.unwrap();
        assert!(
            emitted >= 1,
            "tick must emit at least one entry; got {emitted}"
        );
        let announces = sink.announces.lock().clone();
        assert!(
            announces.iter().any(|(h, rate)| *h == hash && *rate > 0.0),
            "announce list must mention our hot hash with a positive rate; got {announces:?}"
        );
    }

    #[tokio::test]
    async fn fetch_range_bumps_blob_heat_for_touched_chunks_only() {
        use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
        let redex = Arc::new(Redex::new());
        let registry = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
        let adapter = MeshBlobAdapter::new("mesh-heat-range", redex)
            .with_blob_heat(registry.clone(), DEFAULT_BLOB_HEAT_HALF_LIFE);

        // 2-chunk payload — fetch_range over the first chunk only
        // should bump exactly hash[0], not hash[1].
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize + 500))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob =
            BlobRef::manifest("mesh://heat", Encoding::Replicated, chunk_refs.clone()).unwrap();
        adapter.store(&blob, &payload).await.unwrap();

        // Range entirely inside the first chunk.
        let _ = adapter.fetch_range(&blob, 0..1024).await.unwrap();

        let guard = registry.lock();
        assert!(
            guard.get(&chunk_refs[0].hash).is_some(),
            "first chunk's heat must bump on fetch_range over its bytes"
        );
        assert!(
            guard.get(&chunk_refs[1].hash).is_none(),
            "second chunk's heat must NOT bump when range doesn't touch it"
        );
    }

    // ========================================================================
    // OverflowConfig + master switch (P1)
    //
    // P1 carries the type + the builder / getter / setter surface; the
    // push controller + receive-side handler land in P2 / P3. These
    // tests pin the storage contract — defaults match the disabled-by-
    // default posture, the runtime toggle is observable across clones,
    // and the typed config round-trips through the setter.
    // ========================================================================

    #[test]
    fn overflow_disabled_by_default() {
        // Out-of-the-box `MeshBlobAdapter::new` matches v0.2
        // behavior: overflow off, default thresholds visible in
        // the config snapshot.
        let adapter = make_adapter();
        assert!(!adapter.overflow_enabled());
        let cfg = adapter.overflow_config();
        assert_eq!(cfg, OverflowConfig::default());
        assert!(!cfg.enabled);
        assert_eq!(cfg.high_water_ratio, DEFAULT_OVERFLOW_HIGH_WATER_RATIO);
        assert_eq!(cfg.low_water_ratio, DEFAULT_OVERFLOW_LOW_WATER_RATIO);
        assert_eq!(
            cfg.max_pushes_per_tick,
            DEFAULT_OVERFLOW_MAX_PUSHES_PER_TICK
        );
        assert_eq!(cfg.scope, TopologyScope::Mesh);
        assert_eq!(cfg.tick_interval_ms, DEFAULT_OVERFLOW_TICK_INTERVAL_MS);
    }

    #[test]
    fn overflow_with_overflow_builder_seeds_initial_state() {
        // `with_overflow(OverflowConfig { enabled: true, .. })`
        // is the typical "turn on at construction" path.
        let adapter = make_adapter().with_overflow(OverflowConfig {
            enabled: true,
            high_water_ratio: 0.80,
            max_pushes_per_tick: 8,
            ..Default::default()
        });
        assert!(adapter.overflow_enabled());
        let cfg = adapter.overflow_config();
        assert_eq!(cfg.high_water_ratio, 0.80);
        assert_eq!(cfg.max_pushes_per_tick, 8);
        // Unspecified fields inherit defaults.
        assert_eq!(cfg.low_water_ratio, DEFAULT_OVERFLOW_LOW_WATER_RATIO);
        assert_eq!(cfg.scope, TopologyScope::Mesh);
    }

    #[test]
    fn overflow_set_enabled_runtime_toggle_observable() {
        // The runtime setter is the operator's master switch
        // for live deployments — it must be observable without
        // rebuilding the adapter, and visible to existing clones.
        let adapter = make_adapter();
        let clone = adapter.clone();
        assert!(!adapter.overflow_enabled());
        assert!(!clone.overflow_enabled());

        adapter.set_overflow_enabled(true);
        assert!(adapter.overflow_enabled());
        // The Arc<RwLock<_>> is shared across clones — flipping
        // through one handle is visible from the other.
        assert!(clone.overflow_enabled());

        adapter.set_overflow_enabled(false);
        assert!(!adapter.overflow_enabled());
        assert!(!clone.overflow_enabled());
    }

    #[test]
    fn overflow_set_config_replaces_full_config() {
        // The whole-config setter lets operators atomically
        // enable + tune in one call. Useful when the toggle
        // and the threshold update should land together.
        let adapter = make_adapter();
        let new_cfg = OverflowConfig {
            enabled: true,
            high_water_ratio: 0.92,
            low_water_ratio: 0.65,
            max_pushes_per_tick: 4,
            scope: TopologyScope::Zone,
            tick_interval_ms: 60_000,
        };
        adapter.set_overflow_config(new_cfg);
        assert_eq!(adapter.overflow_config(), new_cfg);
        assert!(adapter.overflow_enabled());
    }

    #[test]
    fn overflow_set_enabled_preserves_tunables() {
        // Operators tuning the master switch shouldn't lose
        // their threshold overrides. Verify the toggle path
        // preserves the rest of the config.
        let adapter = make_adapter().with_overflow(OverflowConfig {
            enabled: false,
            high_water_ratio: 0.90,
            max_pushes_per_tick: 32,
            scope: TopologyScope::Region,
            ..Default::default()
        });
        adapter.set_overflow_enabled(true);
        let cfg = adapter.overflow_config();
        assert!(cfg.enabled);
        assert_eq!(cfg.high_water_ratio, 0.90);
        assert_eq!(cfg.max_pushes_per_tick, 32);
        assert_eq!(cfg.scope, TopologyScope::Region);
    }

    #[test]
    fn overflow_active_starts_false_and_clones_share_state() {
        // P2 hysteresis state is held behind an `Arc<AtomicBool>`
        // on the adapter, so an operator dashboard reading
        // `overflow_active()` on one clone sees the live state
        // set by the tick driver on another clone. Verify the
        // shared-state contract directly via the internal
        // handle.
        let adapter = make_adapter();
        let clone = adapter.clone();
        assert!(!adapter.overflow_active());
        assert!(!clone.overflow_active());

        adapter
            .overflow_active_handle()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(adapter.overflow_active());
        assert!(clone.overflow_active());

        adapter
            .overflow_active_handle()
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!adapter.overflow_active());
        assert!(!clone.overflow_active());
    }
}
